//! Durable work queue. Every worker update is conditional on its recorded attempt.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    database::Database,
    error::{AppError, Result},
    models::{ErrorCode, Job, JobKind, JobStatus, PublicError},
    providers::is_safe_provider_detail,
};

const MAX_PAYLOAD_BYTES: usize = 256 * 1024;
const MAX_RESULT_BYTES: usize = 1024 * 1024;
const MAX_ATTEMPTS: u32 = 32;
const MAX_NETWORK_RETRIES: u8 = 5;
const MAX_CONCURRENCY: usize = 8;
const MAX_LIST: usize = 500;
const COLUMNS: &str =
    "id,kind,status,progress,payload_json,result_json,error_json,created_at,updated_at";

pub type JobEventCallback = Arc<dyn Fn(&Job) + Send + Sync>;
type Tokens = HashMap<String, (u32, Arc<AtomicBool>)>;

#[derive(Clone)]
pub struct JobService {
    database: Database,
    tokens: Arc<Mutex<Tokens>>,
    callback: Option<JobEventCallback>,
}

#[derive(Debug, Clone)]
pub struct ClaimedJob {
    pub job: Job,
    pub payload: Value,
    pub attempt: u32,
    pub cancellation: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Envelope {
    version: u8,
    payload: Value,
    message: String,
    book_ids: Vec<String>,
    attempt: u32,
    network_retries: u8,
    next_retry_at: Option<String>,
}

struct Record {
    id: String,
    kind: String,
    status: String,
    progress: f64,
    payload: String,
    result: Option<String>,
    error: Option<String>,
    created_at: String,
    updated_at: String,
}

impl JobService {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            tokens: Arc::new(Mutex::new(HashMap::new())),
            callback: None,
        }
    }

    /// The callback runs after commit, without queue locks; the Manager supplies UI events.
    pub fn with_event_callback(mut self, callback: JobEventCallback) -> Self {
        self.callback = Some(callback);
        self
    }

    pub fn enqueue(&self, kind: JobKind, payload: Value) -> Result<Job> {
        self.enqueue_initial(kind, payload, None)
    }

    /// Persist restart-critical identifiers in the same INSERT as the queued work.
    pub fn enqueue_with_result(
        &self,
        kind: JobKind,
        payload: Value,
        initial_result: Value,
    ) -> Result<Job> {
        validate_value(&initial_result, MAX_RESULT_BYTES)?;
        self.enqueue_initial(kind, payload, Some(initial_result))
    }

    fn enqueue_initial(
        &self,
        kind: JobKind,
        payload: Value,
        initial_result: Option<Value>,
    ) -> Result<Job> {
        validate_payload(&payload)?;
        let book_ids = payload_book_ids(&payload)?;
        let envelope = Envelope {
            version: 1,
            payload,
            message: String::new(),
            book_ids,
            attempt: 0,
            network_retries: 0,
            next_retry_at: None,
        };
        let id = Uuid::new_v4().to_string();
        let now = timestamp();
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Enrichment can be requested concurrently by the UI and several conversations.
        // Origins and baseline revisions remain independent work, while a matching live
        // request is one durable job. Terminal history never prevents an explicit retry.
        if kind == JobKind::Enrich
            && let Some(book_id) = envelope.payload.get("id").and_then(Value::as_str)
        {
            let origin = envelope.payload.get("origin").and_then(Value::as_str);
            let baseline = envelope
                .payload
                .get("baselineRevision")
                .and_then(Value::as_u64)
                .and_then(|value| i64::try_from(value).ok());
            let valid_origin = envelope
                .payload
                .get("origin")
                .is_none_or(|value| value.is_null() || value.is_string());
            let valid_baseline = envelope
                .payload
                .get("baselineRevision")
                .is_none_or(|value| {
                    value.is_null()
                        || value
                            .as_u64()
                            .is_some_and(|value| i64::try_from(value).is_ok())
                });
            if valid_origin && valid_baseline {
                let existing:Option<String>=transaction.query_row("SELECT id FROM jobs WHERE kind='enrich' AND status IN ('queued','running','waitingForConfiguration','waitingForNetwork') AND json_extract(payload_json,'$.payload.id')=?1 AND json_extract(payload_json,'$.payload.origin') IS ?2 AND json_extract(payload_json,'$.payload.baselineRevision') IS ?3 ORDER BY rowid LIMIT 1",params![book_id,origin,baseline],|row|row.get(0)).optional()?;
                if let Some(existing) = existing {
                    let job = decode(load(&transaction, &existing)?)?.0;
                    transaction.commit()?;
                    return Ok(job);
                }
            }
        }
        transaction.execute("INSERT INTO jobs(id,kind,status,progress,payload_json,result_json,created_at,updated_at) VALUES(?1,?2,'queued',0,?3,?4,?5,?5)", params![id,kind.as_str(),serde_json::to_string(&envelope)?,initial_result.as_ref().map(serde_json::to_string).transpose()?,now])?;
        let job = decode(load(&transaction, &id)?)?.0;
        transaction.commit()?;
        self.notify(&job);
        Ok(job)
    }

    pub fn get(&self, id: &str) -> Result<Job> {
        validate_id(id)?;
        Ok(decode(load(&self.database.connect()?, id)?)?.0)
    }

    pub fn payload(&self, id: &str) -> Result<Value> {
        validate_id(id)?;
        Ok(decode(load(&self.database.connect()?, id)?)?.1.payload)
    }

    /// Import recovery schedules automatic enrichment once, regardless of list pagination/status.
    pub fn has_enrichment_for_book(&self, book_id: &str) -> Result<bool> {
        validate_id(book_id)?;
        let connection = self.database.connect()?;
        Ok(connection.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE kind='enrich' AND (json_extract(payload_json,'$.payload.bookId')=?1 OR EXISTS(SELECT 1 FROM json_each(payload_json,'$.bookIds') WHERE type='text' AND value=?1)))",[book_id],|row|row.get(0))?)
    }

    /// Explicit assistant verification is blocked only by unfinished work, not history.
    pub fn has_active_enrichment_for_book(&self, book_id: &str) -> Result<bool> {
        validate_id(book_id)?;
        let connection = self.database.connect()?;
        Ok(connection.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE kind='enrich' AND status IN ('queued','running','waitingForConfiguration','waitingForNetwork') AND (json_extract(payload_json,'$.payload.id')=?1 OR json_extract(payload_json,'$.payload.bookId')=?1 OR EXISTS(SELECT 1 FROM json_each(payload_json,'$.bookIds') WHERE type='text' AND value=?1)))",[book_id],|row|row.get(0))?)
    }

    /// Latest 500 jobs, including terminal states, in deterministic creation order.
    pub fn list(&self) -> Result<Vec<Job>> {
        let connection = self.database.connect()?;
        let mut statement = connection.prepare(&format!(
            "SELECT {COLUMNS} FROM jobs ORDER BY rowid DESC LIMIT ?1"
        ))?;
        statement
            .query_map([MAX_LIST as i64], record)?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|record| Ok(decode(record)?.0))
            .collect()
    }

    /// BEGIN IMMEDIATE makes the running count and queued-to-running CAS one operation.
    pub fn claim_next(&self, max_concurrency: usize) -> Result<Option<ClaimedJob>> {
        if !(1..=MAX_CONCURRENCY).contains(&max_concurrency) {
            return Err(AppError::InvalidInput(
                "Job concurrency must be between 1 and 8".into(),
            ));
        }
        for _ in 0..32 {
            let mut tokens = self.tokens()?;
            let mut connection = self.database.connect()?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let running: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM jobs WHERE status='running'",
                [],
                |row| row.get(0),
            )?;
            if running >= max_concurrency as i64 {
                return Ok(None);
            }
            let next = transaction
                .query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM jobs WHERE status='queued' ORDER BY rowid LIMIT 1"
                    ),
                    [],
                    record,
                )
                .optional()?;
            let Some(record) = next else { return Ok(None) };
            let id = record.id.clone();
            let (mut job, mut envelope) = match decode(record) {
                Ok(value) => value,
                Err(_) => {
                    quarantine(&transaction, &id)?;
                    transaction.commit()?;
                    drop(tokens);
                    self.notify(&self.get(&id)?);
                    continue;
                }
            };
            if envelope.attempt >= MAX_ATTEMPTS {
                job.status = JobStatus::Failed;
                job.error = Some(public_error(
                    ErrorCode::OperationConflict,
                    "The job exhausted its execution attempts",
                    false,
                ));
                job.updated_at = timestamp();
                save(&transaction, &job, &envelope)?;
                transaction.commit()?;
                drop(tokens);
                self.notify(&job);
                continue;
            }
            envelope.attempt += 1;
            envelope.next_retry_at = None;
            envelope.message.clear();
            job.status = JobStatus::Running;
            job.progress = 0.0;
            job.message.clear();
            job.error = None;
            job.updated_at = timestamp();
            let changed = transaction.execute("UPDATE jobs SET status='running',progress=0,payload_json=?2,error_json=NULL,updated_at=?3 WHERE id=?1 AND status='queued'", params![job.id,serde_json::to_string(&envelope)?,job.updated_at])?;
            if changed != 1 {
                return Err(AppError::Conflict("The job was already claimed".into()));
            }
            let cancellation = Arc::new(AtomicBool::new(false));
            transaction.commit()?;
            if let Some((_, old)) =
                tokens.insert(job.id.clone(), (envelope.attempt, cancellation.clone()))
            {
                old.store(true, Ordering::Release);
            }
            drop(tokens);
            self.notify(&job);
            return Ok(Some(ClaimedJob {
                job,
                payload: envelope.payload,
                attempt: envelope.attempt,
                cancellation,
            }));
        }
        Ok(None)
    }

    pub fn update_progress(&self, claim: &ClaimedJob, progress: f64, message: &str) -> Result<Job> {
        if !progress.is_finite() || !(0.0..=1.0).contains(&progress) {
            return Err(AppError::InvalidInput(
                "Job progress must be finite and between zero and one".into(),
            ));
        }
        validate_message(message)?;
        self.update_claimed(claim, |job, envelope| {
            if progress < job.progress {
                return Err(AppError::InvalidInput(
                    "Job progress cannot move backwards within an attempt".into(),
                ));
            }
            job.progress = progress;
            job.message = message.into();
            envelope.message = message.into();
            Ok(())
        })
    }

    pub fn update_result(&self, claim: &ClaimedJob, result: Value) -> Result<Job> {
        validate_value(&result, MAX_RESULT_BYTES)?;
        self.update_claimed(claim, |job, _| {
            job.result = Some(result);
            Ok(())
        })
    }

    pub fn complete(&self, claim: &ClaimedJob, result: Value) -> Result<Job> {
        validate_value(&result, MAX_RESULT_BYTES)?;
        self.update_claimed(claim, |job, envelope| {
            job.status = JobStatus::Completed;
            job.progress = 1.0;
            job.result = Some(result);
            job.error = None;
            job.message.clear();
            envelope.message.clear();
            Ok(())
        })
    }

    pub fn fail(&self, claim: &ClaimedJob, error: PublicError) -> Result<Job> {
        let error = safe_error(error)?;
        self.update_claimed(claim, |job, _| {
            job.status = JobStatus::Failed;
            job.error = Some(error);
            Ok(())
        })
    }

    pub fn wait_for_configuration(&self, claim: &ClaimedJob, error: PublicError) -> Result<Job> {
        let mut error = safe_error(error)?;
        error.retryable = true;
        self.update_claimed(claim, |job, envelope| {
            job.status = JobStatus::WaitingForConfiguration;
            job.error = Some(error);
            envelope.next_retry_at = None;
            Ok(())
        })
    }

    pub fn wait_for_network(&self, claim: &ClaimedJob, error: PublicError) -> Result<Job> {
        let mut error = safe_error(error)?;
        self.update_claimed(claim, |job, envelope| {
            envelope.network_retries += 1;
            if envelope.network_retries > MAX_NETWORK_RETRIES {
                job.status = JobStatus::Failed;
                job.error = Some(public_error(
                    ErrorCode::NetworkUnavailable,
                    "The job exhausted its network retries",
                    false,
                ));
                envelope.next_retry_at = None;
            } else {
                error.retryable = true;
                job.status = JobStatus::WaitingForNetwork;
                job.error = Some(error);
                let delay = if envelope.network_retries == 1 {
                    30
                } else {
                    60
                };
                envelope.next_retry_at = Some(
                    (Utc::now() + Duration::seconds(delay))
                        .to_rfc3339_opts(SecondsFormat::Nanos, true),
                );
            }
            Ok(())
        })
    }

    /// Terminal jobs remain terminal. A clone's active worker observes the shared flag.
    pub fn cancel(&self, id: &str) -> Result<Job> {
        validate_id(id)?;
        let mut tokens = self.tokens()?;
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut job, mut envelope) = decode(load(&transaction, id)?)?;
        if !terminal(job.status) {
            job.status = JobStatus::Cancelled;
            job.error = Some(public_error(
                ErrorCode::Cancelled,
                "Operation cancelled",
                false,
            ));
            job.message.clear();
            envelope.message.clear();
            envelope.next_retry_at = None;
            job.updated_at = timestamp();
            save(&transaction, &job, &envelope)?;
        }
        transaction.commit()?;
        if let Some((_, token)) = tokens.remove(id) {
            token.store(true, Ordering::Release);
        }
        drop(tokens);
        self.notify(&job);
        Ok(job)
    }

    /// Call once at Manager startup, before spawning any workers.
    pub fn recover(&self) -> Result<usize> {
        self.wake(JobStatus::Running, None)
    }

    /// Explicit configuration changes wake configuration waits; periodic polling does not.
    pub fn wake_configuration(&self) -> Result<usize> {
        self.wake(JobStatus::WaitingForConfiguration, None)
    }

    /// Poll periodically. Due times and a five-retry budget survive application restarts.
    pub fn retry_network_due(&self) -> Result<usize> {
        self.wake(JobStatus::WaitingForNetwork, Some(Utc::now()))
    }

    pub fn cancellation_token(&self, id: &str) -> Result<Arc<AtomicBool>> {
        validate_id(id)?;
        let mut tokens = self.tokens()?;
        let (job, envelope) = decode(load(&self.database.connect()?, id)?)?;
        let terminal_or_waiting = job.status != JobStatus::Running;
        if terminal_or_waiting {
            return Ok(Arc::new(AtomicBool::new(true)));
        }
        let entry = tokens.entry(id.into()).or_insert_with(|| {
            (
                envelope.attempt,
                Arc::new(AtomicBool::new(terminal_or_waiting)),
            )
        });
        if entry.0 != envelope.attempt {
            entry.1.store(true, Ordering::Release);
            *entry = (
                envelope.attempt,
                Arc::new(AtomicBool::new(terminal_or_waiting)),
            );
        }
        if terminal_or_waiting {
            entry.1.store(true, Ordering::Release);
        }
        Ok(entry.1.clone())
    }

    fn update_claimed(
        &self,
        claim: &ClaimedJob,
        change: impl FnOnce(&mut Job, &mut Envelope) -> Result<()>,
    ) -> Result<Job> {
        validate_id(&claim.job.id)?;
        let mut tokens = self.tokens()?;
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut job, mut envelope) = decode(load(&transaction, &claim.job.id)?)?;
        if job.status == JobStatus::Cancelled || claim.cancellation.load(Ordering::Acquire) {
            return Err(AppError::Cancelled);
        }
        if job.status != JobStatus::Running || envelope.attempt != claim.attempt {
            return Err(AppError::Conflict(
                "The job attempt is no longer running".into(),
            ));
        }
        change(&mut job, &mut envelope)?;
        job.updated_at = timestamp();
        save(&transaction, &job, &envelope)?;
        transaction.commit()?;
        if job.status != JobStatus::Running
            && let Some((_, token)) = tokens.remove(&job.id)
        {
            token.store(true, Ordering::Release);
        }
        drop(tokens);
        self.notify(&job);
        Ok(job)
    }

    fn wake(&self, status: JobStatus, now: Option<DateTime<Utc>>) -> Result<usize> {
        let mut tokens = self.tokens()?;
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let records = {
            let mut statement = transaction.prepare(&format!(
                "SELECT {COLUMNS} FROM jobs WHERE status=?1 ORDER BY rowid LIMIT ?2"
            ))?;
            statement
                .query_map(params![status.as_str(), MAX_LIST as i64], record)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut changed = Vec::new();
        for record in records {
            let id = record.id.clone();
            let (mut job, mut envelope) = match decode(record) {
                Ok(value) => value,
                Err(_) => {
                    quarantine(&transaction, &id)?;
                    changed.push(decode(load(&transaction, &id)?)?.0);
                    continue;
                }
            };
            if let Some(now) = now {
                let Some(due) = envelope.next_retry_at.as_deref() else {
                    quarantine(&transaction, &id)?;
                    changed.push(decode(load(&transaction, &id)?)?.0);
                    continue;
                };
                if DateTime::parse_from_rfc3339(due)
                    .map_err(|_| AppError::InvalidInput("Invalid retry timestamp".into()))?
                    > now
                {
                    continue;
                }
            }
            job.status = JobStatus::Queued;
            job.progress = 0.0;
            job.error = None;
            job.message = if status == JobStatus::Running {
                "Resuming after application restart".into()
            } else {
                String::new()
            };
            envelope.message = job.message.clone();
            envelope.next_retry_at = None;
            job.updated_at = timestamp();
            save(&transaction, &job, &envelope)?;
            changed.push(job);
        }
        transaction.commit()?;
        for job in &changed {
            if let Some((_, token)) = tokens.remove(&job.id) {
                token.store(true, Ordering::Release);
            }
        }
        drop(tokens);
        for job in &changed {
            self.notify(job);
        }
        Ok(changed.len())
    }

    fn tokens(&self) -> Result<MutexGuard<'_, Tokens>> {
        self.tokens
            .lock()
            .map_err(|_| AppError::Conflict("The job worker registry is unavailable".into()))
    }

    fn notify(&self, job: &Job) {
        if let Some(callback) = &self.callback {
            callback(job);
        }
    }
}

fn record(row: &Row<'_>) -> rusqlite::Result<Record> {
    Ok(Record {
        id: row.get(0)?,
        kind: row.get(1)?,
        status: row.get(2)?,
        progress: row.get(3)?,
        payload: row.get(4)?,
        result: row.get(5)?,
        error: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}
fn load(connection: &Connection, id: &str) -> Result<Record> {
    connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM jobs WHERE id=?1"),
            [id],
            record,
        )
        .optional()?
        .ok_or_else(|| AppError::NotFound("Job not found".into()))
}
fn decode(record: Record) -> Result<(Job, Envelope)> {
    validate_id(&record.id)?;
    if record.payload.len() > MAX_PAYLOAD_BYTES + 16 * 1024 {
        return Err(AppError::InvalidInput(
            "Stored job payload exceeds its bound".into(),
        ));
    }
    let envelope: Envelope = serde_json::from_str(&record.payload)?;
    if envelope.version != 1
        || envelope.attempt > MAX_ATTEMPTS
        || envelope.network_retries > MAX_NETWORK_RETRIES + 1
    {
        return Err(AppError::InvalidInput(
            "Stored job envelope is invalid".into(),
        ));
    }
    validate_payload(&envelope.payload)?;
    validate_message(&envelope.message)?;
    if envelope.book_ids != payload_book_ids(&envelope.payload)? {
        return Err(AppError::InvalidInput(
            "Stored job book identifiers do not match the payload".into(),
        ));
    }
    if let Some(due) = &envelope.next_retry_at {
        DateTime::parse_from_rfc3339(due)
            .map_err(|_| AppError::InvalidInput("Stored job retry date is invalid".into()))?;
    }
    if !record.progress.is_finite() || !(0.0..=1.0).contains(&record.progress) {
        return Err(AppError::InvalidInput(
            "Stored job progress is invalid".into(),
        ));
    }
    let result = record
        .result
        .map(|value| {
            if value.len() > MAX_RESULT_BYTES {
                return Err(AppError::InvalidInput(
                    "Stored job result exceeds its bound".into(),
                ));
            }
            let value: Value = serde_json::from_str(&value)?;
            validate_value(&value, MAX_RESULT_BYTES)?;
            Ok(value)
        })
        .transpose()?;
    let error = record
        .error
        .map(|value| {
            if value.len() > 8192 {
                return Err(AppError::InvalidInput(
                    "Stored job error exceeds its bound".into(),
                ));
            }
            safe_error(serde_json::from_str(&value)?)
        })
        .transpose()?;
    DateTime::parse_from_rfc3339(&record.created_at)
        .map_err(|_| AppError::InvalidInput("Stored job creation date is invalid".into()))?;
    DateTime::parse_from_rfc3339(&record.updated_at)
        .map_err(|_| AppError::InvalidInput("Stored job update date is invalid".into()))?;
    let job = Job {
        id: record.id,
        kind: record
            .kind
            .parse()
            .map_err(|_| AppError::InvalidInput("Stored job kind is invalid".into()))?,
        status: record
            .status
            .parse()
            .map_err(|_| AppError::InvalidInput("Stored job status is invalid".into()))?,
        progress: record.progress,
        message: envelope.message.clone(),
        book_ids: envelope.book_ids.clone(),
        result,
        error,
        created_at: record.created_at,
        updated_at: record.updated_at,
    };
    Ok((job, envelope))
}
fn save(connection: &Connection, job: &Job, envelope: &Envelope) -> Result<()> {
    connection.execute("UPDATE jobs SET status=?2,progress=?3,payload_json=?4,result_json=?5,error_json=?6,updated_at=?7 WHERE id=?1",params![job.id,job.status.as_str(),job.progress,serde_json::to_string(envelope)?,job.result.as_ref().map(serde_json::to_string).transpose()?,job.error.as_ref().map(serde_json::to_string).transpose()?,job.updated_at])?;
    Ok(())
}
fn quarantine(connection: &Connection, id: &str) -> Result<()> {
    let envelope = Envelope {
        version: 1,
        payload: serde_json::json!({}),
        message: String::new(),
        book_ids: Vec::new(),
        attempt: 0,
        network_retries: 0,
        next_retry_at: None,
    };
    let error = public_error(
        ErrorCode::InvalidInput,
        "The stored job payload is malformed and cannot be executed",
        false,
    );
    connection.execute("UPDATE jobs SET kind=CASE WHEN kind IN ('import','enrich','optimize','convert','deviceIndex','transfer','chat') THEN kind ELSE 'import' END,status='failed',progress=0,payload_json=?2,result_json=NULL,error_json=?3,updated_at=?4 WHERE id=?1",params![id,serde_json::to_string(&envelope)?,serde_json::to_string(&error)?,timestamp()])?;
    Ok(())
}
fn validate_payload(value: &Value) -> Result<()> {
    if !value.is_object() {
        return Err(AppError::InvalidInput(
            "Job payload must be a JSON object".into(),
        ));
    }
    validate_value(value, MAX_PAYLOAD_BYTES)
}
fn validate_value(value: &Value, limit: usize) -> Result<()> {
    let mut stack = vec![(value, 0usize)];
    let mut nodes = 0;
    while let Some((value, depth)) = stack.pop() {
        nodes += 1;
        if depth > 32 || nodes > 10_000 {
            return Err(AppError::InvalidInput(
                "Job JSON structure exceeds its bound".into(),
            ));
        }
        match value {
            Value::Object(values) => {
                for (key, value) in values {
                    let key = key.to_ascii_lowercase().replace(['_', '-'], "");
                    if matches!(
                        key.as_str(),
                        "apikey"
                            | "password"
                            | "secret"
                            | "authorization"
                            | "accesstoken"
                            | "refreshtoken"
                    ) {
                        return Err(AppError::InvalidInput(
                            "Job JSON must not persist provider secrets".into(),
                        ));
                    }
                    stack.push((value, depth + 1));
                }
            }
            Value::Array(values) => stack.extend(values.iter().map(|value| (value, depth + 1))),
            _ => {}
        }
    }
    if serde_json::to_vec(value)?.len() > limit {
        return Err(AppError::InvalidInput(
            "Job JSON exceeds its byte bound".into(),
        ));
    }
    Ok(())
}
fn payload_book_ids(payload: &Value) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    if let Some(id) = payload.get("bookId") {
        let id = id
            .as_str()
            .ok_or_else(|| AppError::InvalidInput("Job bookId must be a string".into()))?;
        validate_id(id)?;
        ids.push(id.into());
    }
    if let Some(values) = payload.get("bookIds") {
        let values = values
            .as_array()
            .ok_or_else(|| AppError::InvalidInput("Job bookIds must be an array".into()))?;
        if values.len() > 512 {
            return Err(AppError::InvalidInput(
                "Job accepts at most 512 book identifiers".into(),
            ));
        }
        for value in values {
            let id = value.as_str().ok_or_else(|| {
                AppError::InvalidInput("Job book identifier must be a string".into())
            })?;
            validate_id(id)?;
            if !ids.iter().any(|value| value == id) {
                ids.push(id.into());
            }
        }
    }
    if ids.len() > 512 {
        return Err(AppError::InvalidInput(
            "Job accepts at most 512 distinct book identifiers".into(),
        ));
    }
    Ok(ids)
}
fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Err(AppError::InvalidInput(
            "Invalid job or book identifier".into(),
        ))
    } else {
        Ok(())
    }
}
fn validate_message(message: &str) -> Result<()> {
    if message.chars().count() > 1000
        || message
            .chars()
            .any(|character| character.is_control() && !character.is_whitespace())
    {
        Err(AppError::InvalidInput(
            "Job message exceeds its bound or contains unsafe controls".into(),
        ))
    } else {
        Ok(())
    }
}
fn safe_error(mut error: PublicError) -> Result<PublicError> {
    validate_message(&error.message)?;
    error.detail = error
        .detail
        .filter(|detail| is_safe_provider_detail(error.code, detail));
    Ok(error)
}
fn public_error(code: ErrorCode, message: &str, retryable: bool) -> PublicError {
    PublicError {
        code,
        message: message.into(),
        retryable,
        detail: None,
    }
}
fn terminal(status: JobStatus) -> bool {
    matches!(
        status,
        JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
    )
}
fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Barrier;

    fn queue() -> (tempfile::TempDir, JobService) {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::new(&directory.path().join("profile")).unwrap();
        (directory, JobService::new(database))
    }
    fn network_error() -> PublicError {
        public_error(ErrorCode::NetworkUnavailable, "Network unavailable", true)
    }

    #[test]
    fn queue_preserves_payload_public_fields_and_events_after_commit() {
        let (_directory, service) = queue();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let database = service.database.clone();
        let service = service.with_event_callback(Arc::new(move |job| {
            assert!(load(&database.connect().unwrap(), &job.id).is_ok());
            captured.lock().unwrap().push(job.status);
        }));
        let job = service
            .enqueue(
                JobKind::Transfer,
                json!({"bookIds":["book-1","book-1","book-2"],"deviceId":"reader"}),
            )
            .unwrap();
        assert_eq!(job.book_ids, vec!["book-1", "book-2"]);
        assert_eq!(service.payload(&job.id).unwrap()["deviceId"], "reader");
        assert_eq!(service.list().unwrap(), vec![job.clone()]);
        let claim = service.claim_next(1).unwrap().unwrap();
        service.update_progress(&claim, 0.5, "Copying").unwrap();
        service
            .update_result(&claim, json!({"conversationId":"real-conversation"}))
            .unwrap();
        let complete = service.complete(&claim, json!({"copied":2})).unwrap();
        assert_eq!(complete.status, JobStatus::Completed);
        assert_eq!(complete.progress, 1.0);
        assert_eq!(
            service.cancel(&job.id).unwrap().status,
            JobStatus::Completed
        );
        assert_eq!(events.lock().unwrap().first(), Some(&JobStatus::Queued));
        assert!(events.lock().unwrap().contains(&JobStatus::Completed));
    }

    #[test]
    fn initial_result_is_durable_before_claim_and_invalid_initial_data_is_not_enqueued() {
        let (_directory, service) = queue();
        let result = json!({"conversationId":"conversation-real"});
        let job = service
            .enqueue_with_result(JobKind::Chat, json!({"text":"Question"}), result.clone())
            .unwrap();
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.result, Some(result.clone()));
        let reopened = JobService::new(service.database.clone());
        assert_eq!(reopened.get(&job.id).unwrap().result, Some(result.clone()));
        let claim = reopened.claim_next(1).unwrap().unwrap();
        assert_eq!(claim.job.result, Some(result.clone()));
        assert_eq!(reopened.recover().unwrap(), 1);
        assert_eq!(reopened.get(&job.id).unwrap().result, Some(result));
        for invalid in [
            json!({"text":"x".repeat(MAX_RESULT_BYTES)}),
            json!({"apiKey":"private"}),
        ] {
            assert!(
                service
                    .enqueue_with_result(JobKind::Chat, json!({}), invalid)
                    .is_err()
            );
        }
        assert_eq!(service.list().unwrap().len(), 1);
    }

    #[test]
    fn automatic_enrichment_detection_is_unpaginated_and_includes_terminal_jobs() {
        let (_directory, service) = queue();
        let existing = service
            .enqueue(JobKind::Enrich, json!({"bookId":"original-book"}))
            .unwrap();
        service.cancel(&existing.id).unwrap();
        for index in 0..MAX_LIST {
            service
                .enqueue(JobKind::Import, json!({"batch":index}))
                .unwrap();
        }
        assert!(
            !service
                .list()
                .unwrap()
                .iter()
                .any(|job| job.id == existing.id)
        );
        assert!(service.has_enrichment_for_book("original-book").unwrap());
        assert!(!service.has_enrichment_for_book("unrelated-book").unwrap());
        let array = service
            .enqueue(JobKind::Enrich, json!({"bookIds":["other-book"]}))
            .unwrap();
        assert!(service.has_enrichment_for_book("other-book").unwrap());
        service
            .database
            .connect()
            .unwrap()
            .execute(
                "UPDATE jobs SET status='completed' WHERE id=?1",
                [&array.id],
            )
            .unwrap();
        assert!(service.has_enrichment_for_book("other-book").unwrap());
        assert!(service.has_enrichment_for_book("../invalid").is_err());
    }

    #[test]
    fn active_enrichment_excludes_terminal_history_but_includes_all_waiting_states() {
        for terminal in [
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Cancelled,
        ] {
            let (_directory, service) = queue();
            let job = service
                .enqueue(JobKind::Enrich, json!({"id":"book-a","bookIds":["book-a"]}))
                .unwrap();
            assert!(service.has_active_enrichment_for_book("book-a").unwrap());
            let claim = service.claim_next(1).unwrap().unwrap();
            assert!(service.has_active_enrichment_for_book("book-a").unwrap());
            match terminal {
                JobStatus::Completed => {
                    service.complete(&claim, json!({})).unwrap();
                }
                JobStatus::Failed => {
                    service.fail(&claim, network_error()).unwrap();
                }
                JobStatus::Cancelled => {
                    service.cancel(&job.id).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(!service.has_active_enrichment_for_book("book-a").unwrap());
            assert!(service.has_enrichment_for_book("book-a").unwrap());
        }
        let (_directory, service) = queue();
        service
            .enqueue(JobKind::Enrich, json!({"id":"book-a","bookIds":["book-a"]}))
            .unwrap();
        let claim = service.claim_next(1).unwrap().unwrap();
        service
            .wait_for_configuration(&claim, network_error())
            .unwrap();
        assert!(service.has_active_enrichment_for_book("book-a").unwrap());
        service.wake_configuration().unwrap();
        let claim = service.claim_next(1).unwrap().unwrap();
        service.wait_for_network(&claim, network_error()).unwrap();
        assert!(service.has_active_enrichment_for_book("book-a").unwrap());
        assert!(!service.has_active_enrichment_for_book("book-b").unwrap());
        assert!(service.has_active_enrichment_for_book("../book").is_err());
    }

    #[test]
    fn enrichment_enqueue_reuses_only_matching_active_origin_and_baseline() {
        let (_directory, service) = queue();
        let payload = json!({"id":"book-a","bookIds":["book-a"],"origin":"assistantReview","baselineRevision":2});
        let first = service.enqueue(JobKind::Enrich, payload.clone()).unwrap();
        assert_eq!(
            first.id,
            service
                .enqueue(JobKind::Enrich, payload.clone())
                .unwrap()
                .id
        );
        let claim = service.claim_next(1).unwrap().unwrap();
        assert_eq!(
            first.id,
            service
                .enqueue(JobKind::Enrich, payload.clone())
                .unwrap()
                .id
        );
        service
            .wait_for_configuration(&claim, network_error())
            .unwrap();
        assert_eq!(
            first.id,
            service
                .enqueue(JobKind::Enrich, payload.clone())
                .unwrap()
                .id
        );
        let different_origin = service
            .enqueue(
                JobKind::Enrich,
                json!({"id":"book-a","bookIds":["book-a"],"origin":"manual","baselineRevision":2}),
            )
            .unwrap();
        let different_baseline=service.enqueue(JobKind::Enrich,json!({"id":"book-a","bookIds":["book-a"],"origin":"assistantReview","baselineRevision":3})).unwrap();
        assert_ne!(first.id, different_origin.id);
        assert_ne!(first.id, different_baseline.id);
        service.cancel(&first.id).unwrap();
        let retried = service.enqueue(JobKind::Enrich, payload.clone()).unwrap();
        assert_ne!(first.id, retried.id);
        assert_eq!(
            retried.id,
            service.enqueue(JobKind::Enrich, payload).unwrap().id
        );
        let transfer = service
            .enqueue(JobKind::Transfer, json!({"id":"book-a"}))
            .unwrap();
        assert_ne!(
            transfer.id,
            service
                .enqueue(JobKind::Transfer, json!({"id":"book-a"}))
                .unwrap()
                .id
        );
    }

    #[test]
    fn simultaneous_enrichment_requests_create_one_durable_job() {
        let (_directory, service) = queue();
        let barrier = Arc::new(Barrier::new(6));
        let workers=(0..6).map(|_|{
            let database=service.database.clone();let barrier=barrier.clone();
            std::thread::spawn(move||{
                let queue=JobService::new(database);barrier.wait();
                queue.enqueue(JobKind::Enrich,json!({"id":"book-a","bookIds":["book-a"],"origin":"assistantReview","baselineRevision":2})).unwrap().id
            })
        }).collect::<Vec<_>>();
        let ids = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), 1);
        assert_eq!(service.list().unwrap().len(), 1);
    }

    #[test]
    fn concurrent_claims_obey_global_capacity_and_never_claim_twice() {
        let (_directory, service) = queue();
        for index in 0..12 {
            service
                .enqueue(JobKind::Import, json!({"index":index}))
                .unwrap();
        }
        let barrier = Arc::new(Barrier::new(12));
        let threads = (0..12)
            .map(|_| {
                let service = JobService::new(service.database.clone());
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    service.claim_next(3).unwrap()
                })
            })
            .collect::<Vec<_>>();
        let claims = threads
            .into_iter()
            .filter_map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(claims.len(), 3);
        let mut ids = claims
            .iter()
            .map(|claim| claim.job.id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 3);
        assert!(service.claim_next(0).is_err());
        assert!(service.claim_next(9).is_err());
        assert!(service.claim_next(3).unwrap().is_none());
    }

    #[test]
    fn cancellation_signals_worker_and_refuses_every_late_transition() {
        let (_directory, service) = queue();
        let job = service
            .enqueue(JobKind::Enrich, json!({"bookId":"book"}))
            .unwrap();
        let claim = service.claim_next(1).unwrap().unwrap();
        let token = service.cancellation_token(&job.id).unwrap();
        assert!(Arc::ptr_eq(&token, &claim.cancellation));
        assert_eq!(
            service.clone().cancel(&job.id).unwrap().status,
            JobStatus::Cancelled
        );
        assert!(token.load(Ordering::Acquire));
        assert!(matches!(
            service.complete(&claim, json!({})),
            Err(AppError::Cancelled)
        ));
        assert!(service.update_progress(&claim, 0.6, "Late").is_err());
        assert!(service.fail(&claim, network_error()).is_err());
        assert!(
            service
                .wait_for_configuration(&claim, network_error())
                .is_err()
        );
        assert!(service.wait_for_network(&claim, network_error()).is_err());
        assert_eq!(service.recover().unwrap(), 0);
        assert_eq!(service.wake_configuration().unwrap(), 0);
        assert_eq!(service.retry_network_due().unwrap(), 0);
        assert_eq!(service.get(&job.id).unwrap().status, JobStatus::Cancelled);
    }

    #[test]
    fn restart_invalidates_old_attempt_but_preserves_initial_result_and_payload() {
        let (_directory, service) = queue();
        let job = service
            .enqueue(
                JobKind::Chat,
                json!({"conversationId":"existing","text":"Question"}),
            )
            .unwrap();
        let old = service.claim_next(1).unwrap().unwrap();
        assert_eq!(old.job.id, job.id);
        service.update_progress(&old, 0.5, "Answering").unwrap();
        service
            .update_result(&old, json!({"conversationId":"existing"}))
            .unwrap();
        let restarted = JobService::new(service.database.clone());
        assert_eq!(restarted.recover().unwrap(), 1);
        assert_eq!(restarted.recover().unwrap(), 0);
        let claim = restarted.claim_next(1).unwrap().unwrap();
        assert_eq!(claim.attempt, old.attempt + 1);
        assert_eq!(claim.job.progress, 0.0);
        assert_eq!(
            claim.job.result.as_ref().unwrap()["conversationId"],
            "existing"
        );
        assert_eq!(claim.payload["text"], "Question");
        assert!(service.complete(&old, json!({"wrong":true})).is_err());
        assert_eq!(
            restarted
                .complete(&claim, json!({"answer":"ok"}))
                .unwrap()
                .status,
            JobStatus::Completed
        );
    }

    #[test]
    fn waiting_configuration_is_explicit_and_network_retries_are_bounded_and_durable() {
        let (_directory, service) = queue();
        let job = service.enqueue(JobKind::Enrich, json!({})).unwrap();
        let claim = service.claim_next(1).unwrap().unwrap();
        service
            .wait_for_configuration(
                &claim,
                public_error(
                    ErrorCode::ProviderNotConfigured,
                    "Configure a provider",
                    true,
                ),
            )
            .unwrap();
        assert!(service.claim_next(1).unwrap().is_none());
        assert_eq!(service.retry_network_due().unwrap(), 0);
        assert_eq!(service.wake_configuration().unwrap(), 1);
        let mut claim = service.claim_next(1).unwrap().unwrap();
        for index in 0..=MAX_NETWORK_RETRIES {
            let waiting = service.wait_for_network(&claim, network_error()).unwrap();
            if index == MAX_NETWORK_RETRIES {
                assert_eq!(waiting.status, JobStatus::Failed);
                assert_eq!(waiting.error.unwrap().code, ErrorCode::NetworkUnavailable);
                break;
            }
            assert_eq!(waiting.status, JobStatus::WaitingForNetwork);
            assert_eq!(service.retry_network_due().unwrap(), 0);
            let connection = service.database.connect().unwrap();
            let (_, mut envelope) = decode(load(&connection, &job.id).unwrap()).unwrap();
            let due =
                DateTime::parse_from_rfc3339(envelope.next_retry_at.as_ref().unwrap()).unwrap();
            assert!((29..=60).contains(&(due.with_timezone(&Utc) - Utc::now()).num_seconds()));
            envelope.next_retry_at = Some((Utc::now() - Duration::seconds(1)).to_rfc3339());
            connection
                .execute(
                    "UPDATE jobs SET payload_json=?2 WHERE id=?1",
                    params![job.id, serde_json::to_string(&envelope).unwrap()],
                )
                .unwrap();
            let restarted = JobService::new(service.database.clone());
            assert_eq!(restarted.retry_network_due().unwrap(), 1);
            assert_eq!(restarted.retry_network_due().unwrap(), 0);
            claim = restarted.claim_next(1).unwrap().unwrap();
        }
        assert_eq!(service.retry_network_due().unwrap(), 0);
        assert!(service.claim_next(1).unwrap().is_none());
    }

    #[test]
    fn rejects_malformed_payloads_secrets_results_and_backwards_progress() {
        let (_directory, service) = queue();
        for payload in [
            json!([]),
            json!({"bookIds":"bad"}),
            json!({"bookId":"../bad"}),
            json!({"apiKey":"secret"}),
            json!({"nested":{"access_token":"secret"}}),
            json!({"text":"x".repeat(MAX_PAYLOAD_BYTES)}),
        ] {
            assert!(service.enqueue(JobKind::Import, payload).is_err());
        }
        let mut nested = json!(null);
        for _ in 0..34 {
            nested = json!({"value":nested});
        }
        assert!(service.enqueue(JobKind::Import, nested).is_err());
        let job = service.enqueue(JobKind::Import, json!({})).unwrap();
        let claim = service.claim_next(1).unwrap().unwrap();
        service.update_progress(&claim, 0.5, "Half").unwrap();
        for progress in [-1.0, 1.1, f64::NAN, 0.4] {
            assert!(service.update_progress(&claim, progress, "Bad").is_err());
        }
        assert!(
            service
                .update_result(&claim, json!({"text":"x".repeat(MAX_RESULT_BYTES)}))
                .is_err()
        );
        assert!(
            service
                .update_progress(&claim, 0.6, "bad\0control")
                .is_err()
        );
        service
            .fail(
                &claim,
                PublicError {
                    detail: Some("Sensitive internal trace".into()),
                    ..network_error()
                },
            )
            .unwrap();
        assert!(
            service
                .get(&job.id)
                .unwrap()
                .error
                .unwrap()
                .detail
                .is_none()
        );
        assert_eq!(service.cancel(&job.id).unwrap().status, JobStatus::Failed);
        assert!(service.complete(&claim, json!({})).is_err());
    }

    #[test]
    fn failed_provider_diagnostics_survive_persistence_listing_and_restart_recovery() {
        let (_directory, service) = queue();
        for detail in [
            "providerDiagnostics.authenticationRejected",
            "providerDiagnostics.requestRejected",
            "providerDiagnostics.requestFailed",
            "providerDiagnostics.responseIncomplete",
            "providerDiagnostics.responseMalformed",
            "providerDiagnostics.noFinalAnswer",
            "providerDiagnostics.metadataInvalid",
        ] {
            let job = service
                .enqueue(JobKind::Enrich, json!({"bookId":"book"}))
                .unwrap();
            let claim = service.claim_next(1).unwrap().unwrap();
            let error = PublicError {
                code: ErrorCode::ProviderError,
                message: "Provider request failed".into(),
                retryable: false,
                detail: Some(detail.into()),
            };
            let failed = service.fail(&claim, error.clone()).unwrap();
            assert_eq!(failed.status, JobStatus::Failed);
            assert_eq!(failed.error, Some(error.clone()));
            assert_eq!(service.get(&job.id).unwrap(), failed);
            assert!(service.list().unwrap().contains(&failed));
            let stored: String = service
                .database
                .connect()
                .unwrap()
                .query_row(
                    "SELECT error_json FROM jobs WHERE id=?1",
                    [&job.id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(serde_json::from_str::<PublicError>(&stored).unwrap(), error);

            let reopened = JobService::new(service.database.clone());
            assert_eq!(reopened.recover().unwrap(), 0);
            assert_eq!(reopened.get(&job.id).unwrap(), failed);
            assert!(reopened.list().unwrap().contains(&failed));
            assert!(reopened.claim_next(1).unwrap().is_none());
        }
    }

    #[test]
    fn unsafe_provider_details_are_removed_before_storage_and_when_reading_legacy_rows() {
        let (_directory, service) = queue();
        for (code, detail) in [
            (
                ErrorCode::ProviderError,
                "Authorization: Bearer private-token",
            ),
            (ErrorCode::ProviderError, "/home/private/library/book.epub"),
            (
                ErrorCode::ProviderError,
                "{\"content\":\"PRIVATE_RESPONSE\"}",
            ),
            (ErrorCode::ProviderError, "providerDiagnostics.unknown"),
            (
                ErrorCode::ProviderError,
                " providerDiagnostics.metadataInvalid",
            ),
            (
                ErrorCode::ProviderError,
                "providerDiagnostics.metadataInvalid\n",
            ),
            (
                ErrorCode::ProviderError,
                "providerDiagnostics.metadataInvalid\0",
            ),
            (
                ErrorCode::InvalidInput,
                "providerDiagnostics.metadataInvalid",
            ),
            (
                ErrorCode::NetworkUnavailable,
                "providerDiagnostics.requestFailed",
            ),
        ] {
            let job = service.enqueue(JobKind::Enrich, json!({})).unwrap();
            let claim = service.claim_next(1).unwrap().unwrap();
            let original = PublicError {
                code,
                message: "Request failed".into(),
                retryable: true,
                detail: Some(detail.into()),
            };
            let expected = PublicError {
                detail: None,
                ..original.clone()
            };
            let failed = service.fail(&claim, original.clone()).unwrap();
            assert_eq!(failed.status, JobStatus::Failed);
            assert_eq!(failed.error, Some(expected.clone()));
            let connection = service.database.connect().unwrap();
            let stored: String = connection
                .query_row(
                    "SELECT error_json FROM jobs WHERE id=?1",
                    [&job.id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                serde_json::from_str::<PublicError>(&stored).unwrap(),
                expected
            );
            assert_eq!(service.get(&job.id).unwrap(), failed);
            assert!(service.list().unwrap().contains(&failed));

            // Legacy persisted records must cross the same sanitization boundary on every read.
            connection
                .execute(
                    "UPDATE jobs SET error_json=?2 WHERE id=?1",
                    params![job.id, serde_json::to_string(&original).unwrap()],
                )
                .unwrap();
            let reopened = JobService::new(service.database.clone());
            assert_eq!(reopened.get(&job.id).unwrap().error, Some(expected));
            assert!(reopened.list().unwrap().contains(&failed));
            assert_eq!(reopened.recover().unwrap(), 0);
            assert_eq!(reopened.get(&job.id).unwrap(), failed);
        }
    }

    #[test]
    fn corrupt_queued_records_are_quarantined_without_blocking_following_work() {
        let (_directory, service) = queue();
        let broken = service.enqueue(JobKind::Convert, json!({})).unwrap();
        let good = service
            .enqueue(JobKind::Import, json!({"paths":["/synthetic/book.epub"]}))
            .unwrap();
        service
            .database
            .connect()
            .unwrap()
            .execute(
                "UPDATE jobs SET payload_json='{}' WHERE id=?1",
                [&broken.id],
            )
            .unwrap();
        assert!(service.get(&broken.id).is_err());
        let claim = service.claim_next(1).unwrap().unwrap();
        assert_eq!(claim.job.id, good.id);
        assert_eq!(service.get(&broken.id).unwrap().status, JobStatus::Failed);
        assert_eq!(service.get(&broken.id).unwrap().kind, JobKind::Convert);
        assert_eq!(
            service.get(&broken.id).unwrap().error.unwrap().code,
            ErrorCode::InvalidInput
        );
        assert!(service.get("../invalid").is_err());
        assert!(service.get("unknown").is_err());
    }
}
