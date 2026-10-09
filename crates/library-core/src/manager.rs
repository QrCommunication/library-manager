//! Application lifecycle and background orchestration over focused core services.

use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use rustix::fs::{FlockOperation, Mode, OFlags};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    sync::Mutex as AsyncMutex,
    task::{JoinHandle, JoinSet},
};

use crate::{
    AppError, Book, BookFormat, BookPage, BookPatch, BookQuery, BookRepository, CalibreService,
    ChatService, ClaimedJob, ConversionCapabilities, Converter, Database, Device, DeviceService,
    DeviceTransport, EnrichmentService, ErrorCode, FileVariant, IndexedDeviceBook, Job, JobKind,
    JobService, JobStatus, LibraryFacets, LibraryService, MetadataStatus, Operation, PreparedChat,
    ProviderId, ProviderService, ProviderStatus, PublicError, ReaderService, Result, Settings,
    SettingsService, Storage, TransferService, WebClient, providers::public_provider_error,
};

pub type ManagerEventSink = Arc<dyn Fn(&str, Value) + Send + Sync>;
const WORKER_TICK: Duration = Duration::from_millis(250);
const DEVICE_SCAN_INTERVAL: Duration = Duration::from_secs(5);
const DEVICE_SCAN_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BATCH: usize = 200;
const MAX_INDEX_BOOKS: usize = 500;
const MAX_INDEX_RESULT_BYTES: usize = 768 * 1024;

#[derive(Clone)]
pub struct LibraryManager {
    pub library: LibraryService,
    pub settings: SettingsService,
    pub providers: ProviderService,
    pub reader: ReaderService,
    pub chat: ChatService,
    pub jobs: JobService,
    pub devices: DeviceService,
    pub transfers: TransferService,
    pub calibre: CalibreService,
    pub converter: Converter,
    enrichment: EnrichmentService,
    repository: BookRepository,
    event_sink: ManagerEventSink,
    cached_devices: Arc<Mutex<Vec<Device>>>,
    indexed_connections: Arc<Mutex<HashSet<String>>>,
    scan_gate: Arc<AsyncMutex<()>>,
    _profile_lock: Arc<File>,
}

impl LibraryManager {
    pub fn new(root: &Path, mobitool: PathBuf, event_sink: ManagerEventSink) -> Result<Self> {
        if !root.is_absolute() {
            return Err(invalid(
                "The library profile requires an absolute directory",
            ));
        }
        let database = Database::new(root)?;
        let profile_lock = Arc::new(lock_profile(
            database
                .path()
                .parent()
                .ok_or_else(|| invalid("Missing profile directory"))?,
        )?);
        let storage = Storage::new(&root.join("books"))?;
        let repository = BookRepository::new(database.clone());
        let settings = SettingsService::new(database.clone(), storage.root())?;
        settings.get()?;
        let providers = ProviderService::new(database.clone())?;
        let web = WebClient::new()?;
        let converter = Converter::new(mobitool);
        let library = LibraryService::new(storage.clone(), repository.clone(), converter.clone());
        let reader = ReaderService::new(repository.clone(), storage.clone());
        let chat = ChatService::new(
            database.clone(),
            repository.clone(),
            providers.clone(),
            web.clone(),
        );
        let enrichment =
            EnrichmentService::new(repository.clone(), storage.clone(), providers.clone(), web);
        let callback = event_sink.clone();
        let jobs = JobService::new(database.clone()).with_event_callback(Arc::new(move |job| {
            callback("job:updated", json!({"job":job}))
        }));
        let devices = DeviceService::new(database.clone());
        let transfers = TransferService::new(
            database.clone(),
            devices.clone(),
            storage.clone(),
            repository.clone(),
        );
        let calibre = CalibreService::new(database, devices.clone(), storage, repository.clone());
        let manager = Self {
            library,
            settings,
            providers,
            reader,
            chat,
            jobs,
            devices,
            transfers,
            calibre,
            converter,
            enrichment,
            repository,
            event_sink,
            cached_devices: Arc::new(Mutex::new(Vec::new())),
            indexed_connections: Arc::new(Mutex::new(HashSet::new())),
            scan_gate: Arc::new(AsyncMutex::new(())),
            _profile_lock: profile_lock,
        };
        // Recovery is exclusively performed by the process holding runtime.lock.
        manager.jobs.recover()?;
        Ok(manager)
    }

    pub fn bootstrap(&self) -> Result<Value> {
        let language = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .find_map(|key| {
                std::env::var(key)
                    .ok()
                    .filter(|value| !value.is_empty() && value.len() <= 64)
            })
            .unwrap_or_else(|| "en".into());
        Ok(
            json!({"version":env!("CARGO_PKG_VERSION"), "systemLanguage":language,
            "settings":self.settings.get()?, "devices":self.cached_devices()?, "pendingJobs":self.jobs.list()?}),
        )
    }
    pub fn library_list(&self, query: &BookQuery) -> Result<BookPage> {
        self.library.list(query, &self.connected_ids()?)
    }
    pub fn library_facets(&self) -> Result<LibraryFacets> {
        self.library.facets(&self.connected_ids()?)
    }
    pub fn book_get(&self, id: &str) -> Result<Book> {
        self.library.get(id, &self.connected_ids()?)
    }
    pub fn book_update(&self, id: &str, patch: &BookPatch, revision: u64) -> Result<Book> {
        let book = self.library.update(id, patch, revision)?;
        self.library_changed(vec![id.to_owned()], "metadataUpdate");
        Ok(book)
    }
    pub fn import_books(&self, paths: &[String]) -> Result<Job> {
        validate_batch(paths)?;
        for path in paths {
            let path = Path::new(path);
            if !path.is_absolute() || !std::fs::symlink_metadata(path)?.file_type().is_file() {
                return Err(invalid("Import sources must be absolute regular files"));
            }
        }
        self.jobs.enqueue(JobKind::Import, json!({"paths":paths}))
    }
    pub fn book_enrich(&self, id: &str) -> Result<Job> {
        let book = self.repository.get(id, &[])?;
        self.enqueue_enrichment(&book, "manual")
    }
    fn enqueue_enrichment(&self, book: &Book, origin: &str) -> Result<Job> {
        self.jobs.enqueue(JobKind::Enrich, json!({"id":book.id,"bookIds":[book.id],"baselineRevision":book.revision,"origin":origin}))
    }
    pub fn book_optimize(&self, id: &str, profile_id: &str) -> Result<Job> {
        self.repository.get(id, &[])?;
        crate::optimizer::profile(profile_id)?;
        self.jobs.enqueue(
            JobKind::Optimize,
            json!({"id":id,"profileId":profile_id,"bookIds":[id]}),
        )
    }
    pub fn book_convert(&self, id: &str, format: BookFormat) -> Result<Job> {
        let book = self.repository.get(id, &[])?;
        let source_format = self
            .library
            .files(id)?
            .iter()
            .find(|file| file.format == BookFormat::Epub && file.variant == FileVariant::Normalized)
            .map_or(book.format, |file| file.format);
        if !self.converter.capabilities().iter().any(|capability| {
            capability.inputs.contains(&source_format) && capability.outputs.contains(&format)
        }) {
            return Err(AppError::Unsupported(
                "Unsupported conversion output".into(),
            ));
        }
        self.jobs.enqueue(
            JobKind::Convert,
            json!({"id":id,"format":format,"bookIds":[id]}),
        )
    }
    /// The IPC advertises supported formats while conversion validation uses the full matrix.
    pub fn conversion_capabilities(&self) -> ConversionCapabilities {
        let mut result = ConversionCapabilities {
            inputs: Vec::new(),
            outputs: Vec::new(),
            warnings: Vec::new(),
        };
        for capability in self.converter.capabilities() {
            if !capability.outputs.is_empty() {
                for input in capability.inputs {
                    if !result.inputs.contains(&input) {
                        result.inputs.push(input);
                    }
                }
                for output in capability.outputs {
                    if !result.outputs.contains(&output) {
                        result.outputs.push(output);
                    }
                }
            }
            result.warnings.extend(capability.warnings);
        }
        result
    }
    pub fn device_index(&self, id: &str) -> Result<Job> {
        self.jobs
            .enqueue(JobKind::DeviceIndex, json!({"deviceId":id}))
    }
    pub fn device_transfer(
        &self,
        id: &str,
        book_ids: &[String],
        profile_id: Option<&str>,
    ) -> Result<Job> {
        validate_batch(book_ids)?;
        for book in book_ids {
            self.repository.get(book, &[])?;
        }
        if let Some(profile) = profile_id {
            crate::optimizer::profile(profile)?;
        }
        self.jobs.enqueue(
            JobKind::Transfer,
            json!({"deviceId":id,"bookIds":book_ids,"profileId":profile_id}),
        )
    }
    pub async fn chat_send(
        &self,
        conversation_id: Option<&str>,
        text: &str,
        book_ids: &[String],
    ) -> Result<Job> {
        let service = self.chat.clone();
        let conversation_id = conversation_id.map(str::to_owned);
        let text = text.to_owned();
        let ids = book_ids.to_vec();
        let prepared =
            blocking(move || service.prepare(conversation_id.as_deref(), &text, &ids)).await?;
        self.jobs.enqueue_with_result(JobKind::Chat,
            json!({"conversationId":prepared.conversation_id,"userMessageId":prepared.user_message_id,"text":prepared.text,"bookIds":prepared.book_ids}),
            json!({"conversationId":prepared.conversation_id}))
    }
    pub fn reader_save_progress(&self, id: &str, location: &str, progress: f64) -> Result<()> {
        self.reader.save_progress(id, location, progress)?;
        (self.event_sink)(
            "reader:progress",
            json!({"bookId":id,"location":location,"progress":progress}),
        );
        self.library_changed(vec![id.to_owned()], "readingProgress");
        Ok(())
    }
    pub fn settings_save(&self, settings: &Settings) -> Result<Settings> {
        let saved = self.settings.save(settings)?;
        self.jobs.wake_configuration()?;
        Ok(saved)
    }
    pub fn provider_set_secret(&self, id: ProviderId, secret: &str, persist: bool) -> Result<()> {
        self.providers.set_secret(id, secret, persist)?;
        self.jobs.wake_configuration()?;
        Ok(())
    }
    pub fn provider_clear_secret(&self, id: ProviderId) -> Result<()> {
        self.providers.clear_secret(id)
    }
    pub fn operation_undo(&self, id: &str) -> Result<Operation> {
        let operation = self.library.undo(id)?;
        self.library_changed(Vec::new(), "undo");
        Ok(operation)
    }
    pub fn job_cancel(&self, id: &str) -> Result<Job> {
        let was_running = self.jobs.get(id)?.status == JobStatus::Running;
        let job = self.jobs.cancel(id)?;
        if was_running
            && job.status == JobStatus::Cancelled
            && job.kind == JobKind::Transfer
            && let Some(device) = self
                .jobs
                .payload(id)?
                .get("deviceId")
                .and_then(Value::as_str)
            && !device.starts_with("calibre-")
        {
            self.transfers.cancel(device)?;
        }
        Ok(job)
    }

    pub fn connected_ids(&self) -> Result<Vec<String>> {
        Ok(self
            .cached_devices()?
            .into_iter()
            .filter(|device| device.connected)
            .map(|device| device.id)
            .collect())
    }
    fn cached_devices(&self) -> Result<Vec<Device>> {
        self.cached_devices
            .lock()
            .map(|devices| devices.clone())
            .map_err(|_| invalid("Device cache is unavailable"))
    }
    pub async fn devices_scan(&self) -> Result<Vec<Device>> {
        let Ok(guard) = self.scan_gate.clone().try_lock_owned() else {
            return self.cached_devices();
        };
        let devices = self.devices.clone();
        // Keep ownership in the blocking task if its async caller is timed out or dropped.
        let (usb, _guard) = blocking(move || Ok((devices.scan(), guard))).await?;
        let (wireless, calibre) = tokio::join!(self.transfers.scan_wireless(), self.calibre.scan());
        let previous = self.cached_devices()?;
        let mut result = BTreeMap::new();
        for (scan, group) in [
            (usb, DeviceTransport::Usb),
            (wireless, DeviceTransport::Crosspoint),
            (calibre, DeviceTransport::CalibreWireless),
        ] {
            match scan {
                Ok(devices) => {
                    for device in devices
                        .into_iter()
                        .filter(|device| device.transport == group)
                    {
                        result.insert(device.id.clone(), device);
                    }
                }
                Err(_) => {
                    for mut device in previous
                        .iter()
                        .filter(|device| device.transport == group)
                        .cloned()
                    {
                        device.connected = false;
                        result.insert(device.id.clone(), device);
                    }
                }
            }
        }
        let mut current: Vec<_> = result.into_values().collect();
        current.sort_by(|left, right| left.label.cmp(&right.label).then(left.id.cmp(&right.id)));
        *self
            .cached_devices
            .lock()
            .map_err(|_| invalid("Device cache is unavailable"))? = current.clone();
        if current != previous {
            (self.event_sink)("devices:changed", json!({"devices":current}));
            self.library_changed(Vec::new(), "devicePresence");
        }
        let connected: HashSet<_> = current
            .iter()
            .filter(|device| device.connected)
            .map(|device| device.id.clone())
            .collect();
        let new_connections = {
            let mut indexed = self
                .indexed_connections
                .lock()
                .map_err(|_| invalid("Device indexing state is unavailable"))?;
            indexed.retain(|id| connected.contains(id));
            let new: Vec<_> = connected
                .iter()
                .filter(|id| !indexed.contains(*id))
                .cloned()
                .collect();
            indexed.extend(new.iter().cloned());
            new
        };
        for id in new_connections {
            self.device_index(&id)?;
        }
        Ok(current)
    }
    pub async fn device_connect_wireless(
        &self,
        address: &str,
        transport: DeviceTransport,
        label: &str,
        password: Option<&str>,
    ) -> Result<Device> {
        let device = match transport {
            DeviceTransport::CalibreWireless => {
                self.calibre.start(address, label, password).await?
            }
            DeviceTransport::Crosspoint => {
                self.transfers
                    .connect_wireless(address, transport, label, password)
                    .await?
            }
            DeviceTransport::Usb => {
                return Err(invalid(
                    "USB connections are discovered from mounted devices",
                ));
            }
        };
        self.devices_scan().await?;
        Ok(device)
    }
    pub async fn device_disconnect(&self, id: &str) -> Result<()> {
        if id.starts_with("calibre-") {
            self.calibre.disconnect(id).await?;
        } else {
            self.transfers.disconnect(id)?;
        }
        self.devices_scan().await?;
        Ok(())
    }

    pub fn start_background(&self, shutdown: Arc<AtomicBool>) -> JoinHandle<()> {
        let manager = self.clone();
        tokio::spawn(async move {
            manager.background(shutdown).await;
        })
    }
    async fn background(self, shutdown: Arc<AtomicBool>) {
        let mut workers = JoinSet::new();
        let mut active_jobs = HashSet::new();
        let mut scans = JoinSet::new();
        let mut tick = tokio::time::interval(WORKER_TICK);
        let mut scan_tick = tokio::time::interval(DEVICE_SCAN_INTERVAL);
        loop {
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            tokio::select! {
                _ = tick.tick() => {
                    let _ = self.jobs.retry_network_due();
                    if let Ok(settings) = self.settings.get() {
                        while let Ok(Some(claim)) = self.jobs.claim_next(settings.max_concurrent_jobs as usize) {
                            let id = claim.job.id.clone();
                            active_jobs.insert(id.clone());
                            let manager = self.clone(); workers.spawn(async move { manager.execute_claim(claim).await; id });
                        }
                    }
                }
                _ = scan_tick.tick(), if scans.is_empty() => {
                    let manager = self.clone(); scans.spawn(async move { let _ = tokio::time::timeout(DEVICE_SCAN_TIMEOUT, manager.devices_scan()).await; });
                }
                finished = workers.join_next(), if !workers.is_empty() => {
                    if let Some(Ok(id)) = finished { active_jobs.remove(&id); }
                }
                _ = scans.join_next(), if !scans.is_empty() => {}
            }
        }
        // Active workers are tracked independently of the bounded UI job history.
        for id in active_jobs {
            let _ = self.job_cancel(&id);
        }
        workers.abort_all();
        scans.abort_all();
        while workers.join_next().await.is_some() {}
        while scans.join_next().await.is_some() {}
    }
    /// Execute one durable job; useful for bounded integration tests and headless hosts.
    pub async fn run_once(&self) -> Result<bool> {
        let settings = self.settings.get()?;
        if let Some(claim) = self
            .jobs
            .claim_next(settings.max_concurrent_jobs as usize)?
        {
            self.execute_claim(claim).await;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    async fn execute_claim(&self, claim: ClaimedJob) {
        let outcome = if claim.job.kind == JobKind::Transfer {
            self.dispatch(&claim).await
        } else {
            tokio::select! {
                result = self.dispatch(&claim) => result,
                _ = cancelled(claim.cancellation.clone()) => Err(AppError::Cancelled),
            }
        };
        match outcome {
            Ok(result) if !claim.cancellation.load(Ordering::Acquire) => {
                let _ = self.jobs.complete(&claim, result);
            }
            Ok(_) | Err(AppError::Cancelled) => {
                let _ = self.jobs.cancel(&claim.job.id);
            }
            Err(error) => {
                let public = job_public_error(claim.job.kind, &error);
                match public.code {
                    ErrorCode::ProviderNotConfigured | ErrorCode::SecretStoreUnavailable => {
                        let _ = self.jobs.wait_for_configuration(&claim, public);
                    }
                    ErrorCode::NetworkUnavailable | ErrorCode::RateLimited => {
                        let _ = self.jobs.wait_for_network(&claim, public);
                    }
                    _ => {
                        let _ = self.jobs.fail(&claim, public);
                    }
                }
            }
        }
    }

    async fn dispatch(&self, claim: &ClaimedJob) -> Result<Value> {
        check_cancelled(claim)?;
        match claim.job.kind {
            JobKind::Import => self.import_job(claim).await,
            JobKind::Enrich => self.enrich_job(claim).await,
            JobKind::Optimize => {
                let payload: OptimizePayload = decode(&claim.payload)?;
                let library = self
                    .library
                    .clone()
                    .with_cancellation(claim.cancellation.clone());
                let id = payload.id.clone();
                let result = blocking(move || library.optimize(&id, &payload.profile_id)).await?;
                check_cancelled(claim)?;
                self.library_changed(vec![payload.id], "optimize");
                Ok(serde_json::to_value(result)?)
            }
            JobKind::Convert => {
                let payload: ConvertPayload = decode(&claim.payload)?;
                let library = self
                    .library
                    .clone()
                    .with_cancellation(claim.cancellation.clone());
                let id = payload.id.clone();
                let result = blocking(move || library.convert(&id, payload.format)).await?;
                check_cancelled(claim)?;
                self.library_changed(vec![payload.id], "convert");
                Ok(serde_json::to_value(result)?)
            }
            JobKind::DeviceIndex => self.index_job(claim).await,
            JobKind::Transfer => self.transfer_job(claim).await,
            JobKind::Chat => {
                let payload: ChatPayload = decode(&claim.payload)?;
                let settings = self.configured_settings()?;
                let prepared = PreparedChat {
                    conversation_id: payload.conversation_id,
                    user_message_id: payload.user_message_id,
                    text: payload.text,
                    book_ids: payload.book_ids,
                };
                let result = self.chat.respond(&prepared, &settings).await?;
                check_cancelled(claim)?;
                (self.event_sink)(
                    "chat:delta",
                    json!({"conversationId":result.conversation_id,"messageId":result.id,"text":result.content,"finished":true}),
                );
                Ok(serde_json::to_value(result)?)
            }
        }
    }
    async fn import_job(&self, claim: &ClaimedJob) -> Result<Value> {
        let payload: ImportPayload = decode(&claim.payload)?;
        validate_batch(&payload.paths)?;
        let settings = self.settings.get()?;
        let mut imported = 0;
        let mut duplicates = 0;
        let mut warnings = Vec::new();
        let mut ids = Vec::new();
        let mut errors = BTreeMap::<String, u64>::new();
        for (index, path) in payload.paths.iter().enumerate() {
            check_cancelled(claim)?;
            let library = self
                .library
                .clone()
                .with_cancellation(claim.cancellation.clone());
            let path = PathBuf::from(path);
            match blocking(move || library.import(&path)).await {
                Ok(outcome) => {
                    if outcome.duplicate {
                        duplicates += 1;
                    } else {
                        imported += 1;
                    }
                    warnings.extend(outcome.warnings);
                    if settings.auto_enrich
                        && (!outcome.duplicate || self.needs_recovered_enrichment(&outcome.book)?)
                    {
                        self.enqueue_enrichment(&outcome.book, "import")?;
                    }
                    ids.push(outcome.book.id);
                }
                Err(AppError::Cancelled) => return Err(AppError::Cancelled),
                Err(error) => {
                    *errors.entry(error.code().into()).or_default() += 1;
                }
            }
            self.jobs.update_progress(
                claim,
                (index + 1) as f64 / payload.paths.len() as f64,
                "",
            )?;
            self.jobs.update_result(claim, json!({"imported":imported,"duplicates":duplicates,"warnings":warnings,"bookIds":ids,"errorsByCode":errors}))?;
        }
        check_cancelled(claim)?;
        self.library_changed(ids.clone(), "import");
        if imported + duplicates == 0 && !errors.is_empty() {
            return Err(invalid("No source could be imported"));
        }
        Ok(
            json!({"imported":imported,"duplicates":duplicates,"warnings":warnings,"bookIds":ids,"errorsByCode":errors}),
        )
    }
    fn needs_recovered_enrichment(&self, book: &Book) -> Result<bool> {
        if book.metadata_status == MetadataStatus::Verified || book.metadata_confidence.is_some() {
            return Ok(false);
        }
        Ok(!self.jobs.has_enrichment_for_book(&book.id)?)
    }
    async fn enrich_job(&self, claim: &ClaimedJob) -> Result<Value> {
        let payload: EnrichPayload = decode(&claim.payload)?;
        if payload.origin != "import" && payload.origin != "manual" {
            return Err(invalid("Invalid enrichment origin"));
        }
        let mut settings = self.settings.get()?;
        if payload.origin == "import" && !settings.auto_enrich {
            return Ok(json!({"skipped":"autoEnrichmentDisabled","bookId":payload.id}));
        }
        settings = self.configured_settings()?;
        if payload.origin == "manual" {
            settings.auto_enrich = true;
        }
        let before = self.repository.get(&payload.id, &[])?;
        let baseline = payload.baseline_revision.unwrap_or(before.revision);
        let automatic_allowed = enrichment_may_apply(&before, baseline, &payload.origin);
        let outcome = self.enrichment.propose(&payload.id, &settings).await?;
        check_cancelled(claim)?;
        let result = serde_json::to_value(&outcome.proposal)?;
        self.jobs.update_result(claim, result.clone())?;
        if outcome.auto_applicable && automatic_allowed && outcome.expected_revision == baseline {
            let library = self
                .library
                .clone()
                .with_cancellation(claim.cancellation.clone());
            let id = payload.id.clone();
            let patch = outcome.proposal.patch;
            blocking(move || {
                library.apply_enrichment(
                    &id,
                    &patch,
                    outcome.expected_revision,
                    MetadataStatus::Verified,
                    outcome.proposal.confidence,
                )
            })
            .await?;
            self.library_changed(vec![payload.id], "enrichment");
        } else {
            let current = self.repository.get(&payload.id, &[])?;
            if current.metadata_status != MetadataStatus::Verified {
                check_cancelled(claim)?;
                self.repository.set_metadata_status_if_revision(
                    &payload.id,
                    MetadataStatus::NeedsReview,
                    Some(outcome.proposal.confidence),
                    outcome.expected_revision,
                )?;
                self.library_changed(vec![payload.id], "metadataReview");
            }
        }
        Ok(result)
    }
    async fn index_job(&self, claim: &ClaimedJob) -> Result<Value> {
        let payload: DevicePayload = decode(&claim.payload)?;
        let books = if payload.device_id.starts_with("calibre-") {
            self.calibre.index(&payload.device_id).await?
        } else if payload.device_id.starts_with("crosspoint-") {
            self.transfers.index_wireless(&payload.device_id).await?
        } else {
            let devices = self.devices.clone();
            let id = payload.device_id.clone();
            blocking(move || devices.index(&id)).await?
        };
        check_cancelled(claim)?;
        let warnings = if payload.device_id.starts_with("calibre-") {
            self.calibre.warnings(&payload.device_id)?
        } else {
            Vec::new()
        };
        self.library_changed(Vec::new(), "deviceIndex");
        limited_index_result(&payload.device_id, books, warnings)
    }
    async fn transfer_job(&self, claim: &ClaimedJob) -> Result<Value> {
        let payload: TransferPayload = decode(&claim.payload)?;
        let result = if payload.device_id.starts_with("calibre-") {
            self.calibre
                .transfer(
                    &payload.device_id,
                    &payload.book_ids,
                    payload.profile_id.as_deref(),
                    &claim.cancellation,
                )
                .await?
        } else {
            tokio::select! {
                result = self.transfers.transfer(&payload.device_id,&payload.book_ids,payload.profile_id.as_deref()) => result?,
                _ = cancelled(claim.cancellation.clone()) => { self.transfers.cancel(&payload.device_id)?; return Err(AppError::Cancelled); }
            }
        };
        check_cancelled(claim)?;
        self.library_changed(payload.book_ids, "transfer");
        Ok(serde_json::to_value(result)?)
    }
    fn configured_settings(&self) -> Result<Settings> {
        let settings = self.settings.get()?;
        let provider = settings
            .provider_id
            .ok_or_else(|| AppError::Provider("Configure an API key and provider first".into()))?;
        if settings.model_id.as_deref().is_none_or(str::is_empty)
            || !self.providers.list().iter().any(|candidate| {
                candidate.id == provider && candidate.status == ProviderStatus::Ready
            })
        {
            return Err(AppError::Provider(
                "Configure an API key and model first".into(),
            ));
        }
        Ok(settings)
    }
    fn library_changed(&self, ids: Vec<String>, reason: &str) {
        (self.event_sink)("library:changed", json!({"bookIds":ids,"reason":reason}));
    }
}

fn enrichment_may_apply(book: &Book, baseline: u64, origin: &str) -> bool {
    book.revision == baseline
        && (origin == "manual"
            || (origin == "import" && book.metadata_status != MetadataStatus::Verified))
}

fn limited_index_result(
    id: &str,
    books: Vec<IndexedDeviceBook>,
    mut warnings: Vec<String>,
) -> Result<Value> {
    let total = books.len();
    let mut visible = Vec::new();
    // Reserve room for the truncation diagnostic and final punctuation before adding rows.
    let mut size = serde_json::to_vec(
        &json!({"deviceId":id,"books":[],"total":total,"truncated":true,"warnings":warnings}),
    )?
    .len()
        + 64;
    for book in books {
        let value = serde_json::to_value(book)?;
        let additional = serde_json::to_vec(&value)?.len() + 1;
        if visible.len() == MAX_INDEX_BOOKS || size + additional > MAX_INDEX_RESULT_BYTES {
            break;
        }
        size += additional;
        visible.push(value);
    }
    let truncated = visible.len() != total;
    if truncated {
        warnings.push("deviceIndexResultTruncated".into());
    }
    let result = json!({"deviceId":id,"books":visible,"total":total,"truncated":truncated,"warnings":warnings});
    if serde_json::to_vec(&result)?.len() > MAX_INDEX_RESULT_BYTES {
        return Err(invalid("Device index diagnostics exceed the result limit"));
    }
    Ok(result)
}

fn lock_profile(root: &Path) -> Result<File> {
    let fd = rustix::fs::open(
        root.join("runtime.lock"),
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(std::io::Error::from)?;
    let file = File::from(fd);
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o777 != 0o600 {
        return Err(invalid("Profile lock must be a private regular file"));
    }
    let stat = rustix::fs::fstat(&file).map_err(std::io::Error::from)?;
    if stat.st_uid != rustix::process::getuid().as_raw() {
        return Err(invalid("Profile lock is owned by another user"));
    }
    rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
        if error == rustix::io::Errno::WOULDBLOCK {
            AppError::ProfileInUse
        } else {
            AppError::Io(error.into())
        }
    })?;
    Ok(file)
}
fn validate_batch(values: &[String]) -> Result<()> {
    if values.is_empty()
        || values.len() > MAX_BATCH
        || values
            .iter()
            .any(|value| value.is_empty() || value.len() > 4096 || value.contains('\0'))
    {
        return Err(invalid(
            "A batch needs one to 200 bounded identifiers or paths",
        ));
    }
    Ok(())
}
fn decode<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T> {
    serde_json::from_value(value.clone()).map_err(|_| invalid("Invalid background-job payload"))
}
fn invalid(message: &str) -> AppError {
    AppError::InvalidInput(message.into())
}
fn check_cancelled(claim: &ClaimedJob) -> Result<()> {
    if claim.cancellation.load(Ordering::Acquire) {
        Err(AppError::Cancelled)
    } else {
        Ok(())
    }
}
async fn cancelled(signal: Arc<AtomicBool>) {
    while !signal.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| invalid("Background operation did not finish"))?
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportPayload {
    paths: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OptimizePayload {
    id: String,
    profile_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConvertPayload {
    id: String,
    format: BookFormat,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevicePayload {
    device_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransferPayload {
    device_id: String,
    book_ids: Vec<String>,
    profile_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnrichPayload {
    id: String,
    baseline_revision: Option<u64>,
    #[serde(default = "manual_origin")]
    origin: String,
}
fn manual_origin() -> String {
    "manual".into()
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatPayload {
    conversation_id: String,
    user_message_id: String,
    text: String,
    book_ids: Vec<String>,
}

fn job_public_error(kind: JobKind, error: &AppError) -> PublicError {
    if matches!(kind, JobKind::Chat | JobKind::Enrich)
        && matches!(error, AppError::Provider(_) | AppError::Network(_))
    {
        public_provider_error(error)
    } else {
        PublicError::from(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::JobStatus;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    type Events = Arc<Mutex<Vec<(String, Value)>>>;

    fn create(root: &Path, events: Events) -> Result<LibraryManager> {
        LibraryManager::new(
            root,
            root.join("absent-mobitool"),
            Arc::new(move |name, value| {
                events.lock().unwrap().push((name.to_owned(), value));
            }),
        )
    }

    fn fixture() -> (TempDir, LibraryManager, Events, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let manager = create(&directory.path().join("profile"), events.clone()).unwrap();
        let source = directory.path().join("Synthetic story.txt");
        std::fs::write(
            &source,
            "Premier paragraphe.\n\nUn deuxième chapitre : été, lumière.",
        )
        .unwrap();
        (directory, manager, events, source)
    }

    #[test]
    fn job_errors_preserve_domain_conflicts_and_only_specialize_provider_requests() {
        for kind in [
            JobKind::Chat,
            JobKind::Enrich,
            JobKind::Import,
            JobKind::Convert,
            JobKind::Optimize,
            JobKind::DeviceIndex,
            JobKind::Transfer,
        ] {
            assert_eq!(
                job_public_error(kind, &AppError::RevisionConflict).code,
                ErrorCode::RevisionConflict
            );
            assert_eq!(
                job_public_error(kind, &AppError::ProfileInUse).code,
                ErrorCode::ProfileInUse
            );
            let collision =
                job_public_error(kind, &AppError::Conflict("private/path collision".into()));
            assert_eq!(collision.code, ErrorCode::OperationConflict);
            assert!(
                !serde_json::to_string(&collision)
                    .unwrap()
                    .contains("private/path")
            );
        }
        for kind in [JobKind::Chat, JobKind::Enrich] {
            let configuration = job_public_error(
                kind,
                &AppError::Provider("Configure an API key private-token".into()),
            );
            assert_eq!(configuration.code, ErrorCode::ProviderNotConfigured);
            assert!(
                !serde_json::to_string(&configuration)
                    .unwrap()
                    .contains("private-token")
            );
            assert_eq!(
                job_public_error(kind, &AppError::Network("private upstream URL".into())).code,
                ErrorCode::NetworkUnavailable
            );
        }
        assert_eq!(
            job_public_error(
                JobKind::Import,
                &AppError::Provider("Configure an API key".into())
            )
            .code,
            ErrorCode::ProviderError
        );
    }

    #[tokio::test]
    async fn import_queues_one_enrichment_and_waits_without_configuration() {
        let (_directory, manager, events, source) = fixture();
        let original = std::fs::read(&source).unwrap();
        let import = manager
            .import_books(&[source.to_str().unwrap().to_owned()])
            .unwrap();
        assert!(manager.run_once().await.unwrap());
        let completed = manager.jobs.get(&import.id).unwrap();
        assert_eq!(completed.status, JobStatus::Completed);
        let result = completed.result.unwrap();
        assert_eq!(result["imported"], 1);
        let id = result["bookIds"][0].as_str().unwrap();
        let book = manager.book_get(id).unwrap();
        let enrich = manager
            .jobs
            .list()
            .unwrap()
            .into_iter()
            .find(|job| job.kind == JobKind::Enrich)
            .unwrap();
        let payload = manager.jobs.payload(&enrich.id).unwrap();
        assert_eq!(payload["origin"], "import");
        assert_eq!(payload["baselineRevision"], book.revision);
        assert!(manager.run_once().await.unwrap());
        assert_eq!(
            manager.jobs.get(&enrich.id).unwrap().status,
            JobStatus::WaitingForConfiguration
        );
        assert!(!manager.run_once().await.unwrap());

        let reimport = manager
            .import_books(&[source.to_str().unwrap().to_owned()])
            .unwrap();
        assert!(manager.run_once().await.unwrap());
        assert_eq!(
            manager.jobs.get(&reimport.id).unwrap().result.unwrap()["duplicates"],
            1
        );
        assert_eq!(
            manager
                .jobs
                .list()
                .unwrap()
                .iter()
                .filter(|job| job.kind == JobKind::Enrich)
                .count(),
            1
        );
        assert_eq!(std::fs::read(source).unwrap(), original);
        assert_eq!(
            manager.library_list(&BookQuery::default()).unwrap().total,
            1
        );
        let events = events.lock().unwrap();
        assert!(events.iter().any(|(name, _)| name == "job:updated"));
        assert!(
            events
                .iter()
                .any(|(name, value)| name == "library:changed" && value["reason"] == "import")
        );
        assert!(
            events
                .iter()
                .filter(|(name, _)| name == "library:changed")
                .all(|(_, value)| value.get("paths").is_none())
        );
    }

    #[tokio::test]
    async fn deferred_enrichment_keeps_its_baseline_across_restart_and_human_edits() {
        let (directory, manager, events, source) = fixture();
        manager
            .import_books(&[source.to_str().unwrap().to_owned()])
            .unwrap();
        assert!(manager.run_once().await.unwrap());
        assert!(manager.run_once().await.unwrap());
        let job = manager
            .jobs
            .list()
            .unwrap()
            .into_iter()
            .find(|job| job.kind == JobKind::Enrich)
            .unwrap();
        let original_payload = manager.jobs.payload(&job.id).unwrap();
        let id = original_payload["id"].as_str().unwrap().to_owned();
        let baseline = original_payload["baselineRevision"].as_u64().unwrap();
        let book = manager.book_get(&id).unwrap();
        assert!(enrichment_may_apply(&book, baseline, "import"));
        let corrected = manager
            .book_update(
                &id,
                &BookPatch {
                    title: Some("Titre corrigé par son propriétaire".into()),
                    ..BookPatch::default()
                },
                book.revision,
            )
            .unwrap();
        assert_eq!(corrected.metadata_status, MetadataStatus::Verified);
        assert!(!enrichment_may_apply(&corrected, baseline, "import"));
        assert!(!enrichment_may_apply(
            &corrected,
            corrected.revision,
            "import"
        ));
        assert!(enrichment_may_apply(
            &corrected,
            corrected.revision,
            "manual"
        ));
        assert!(!enrichment_may_apply(
            &corrected,
            corrected.revision,
            "unknown"
        ));
        drop(manager);

        let manager = create(&directory.path().join("profile"), events).unwrap();
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().status,
            JobStatus::WaitingForConfiguration
        );
        manager
            .settings_save(&manager.settings.get().unwrap())
            .unwrap();
        assert_eq!(manager.jobs.get(&job.id).unwrap().status, JobStatus::Queued);
        assert_eq!(manager.jobs.payload(&job.id).unwrap(), original_payload);
        assert!(manager.run_once().await.unwrap());
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().status,
            JobStatus::WaitingForConfiguration
        );
        let book = manager.book_get(&id).unwrap();
        assert_eq!(book.title, "Titre corrigé par son propriétaire");
        assert_eq!(book.metadata_status, MetadataStatus::Verified);
        assert_eq!(book.revision, corrected.revision);
        let explicit = manager.book_enrich(&id).unwrap();
        let payload = manager.jobs.payload(&explicit.id).unwrap();
        assert_eq!(payload["origin"], "manual");
        assert_eq!(payload["baselineRevision"], corrected.revision);
    }

    #[tokio::test]
    async fn cancelled_claim_never_publishes_a_conversion_or_resurrects() {
        let (directory, manager, events, source) = fixture();
        let mut settings = manager.settings.get().unwrap();
        settings.auto_enrich = false;
        manager.settings_save(&settings).unwrap();
        let book = manager.library.import(&source).unwrap().book;
        let before = manager.library.files(&book.id).unwrap();
        let job = manager.book_convert(&book.id, BookFormat::Epub).unwrap();
        let claim = manager.jobs.claim_next(1).unwrap().unwrap();
        assert_eq!(claim.job.id, job.id);
        assert_eq!(
            manager.job_cancel(&job.id).unwrap().status,
            JobStatus::Cancelled
        );
        assert!(claim.cancellation.load(Ordering::Acquire));
        manager.execute_claim(claim).await;
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().status,
            JobStatus::Cancelled
        );
        assert_eq!(manager.library.files(&book.id).unwrap(), before);
        drop(manager);
        let manager = create(&directory.path().join("profile"), events).unwrap();
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().status,
            JobStatus::Cancelled
        );
        assert!(!manager.run_once().await.unwrap());
        assert_eq!(manager.library.files(&book.id).unwrap(), before);
    }

    #[tokio::test]
    async fn chat_conversation_is_durable_before_model_configuration() {
        let (directory, manager, events, _source) = fixture();
        let job = manager
            .chat_send(None, "Quels romans puis-je lire ?", &[])
            .await
            .unwrap();
        let initial = manager.jobs.get(&job.id).unwrap().result.unwrap();
        let conversation = initial["conversationId"].as_str().unwrap().to_owned();
        assert_eq!(manager.chat.messages(&conversation).unwrap().len(), 1);
        assert!(manager.run_once().await.unwrap());
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().status,
            JobStatus::WaitingForConfiguration
        );
        drop(manager);
        let manager = create(&directory.path().join("profile"), events).unwrap();
        assert_eq!(manager.jobs.get(&job.id).unwrap().result.unwrap(), initial);
        assert_eq!(manager.chat.messages(&conversation).unwrap().len(), 1);
    }

    #[test]
    fn profile_lock_follows_all_clones_and_rejects_unsafe_lock_paths() {
        let (directory, manager, events, _source) = fixture();
        let root = directory.path().join("profile");
        assert!(matches!(
            create(&root, events.clone()),
            Err(AppError::ProfileInUse)
        ));
        let clone = manager.clone();
        drop(manager);
        assert!(matches!(
            create(&root, events.clone()),
            Err(AppError::ProfileInUse)
        ));
        drop(clone);
        drop(create(&root, events.clone()).unwrap());
        assert!(create(Path::new("relative-profile"), events.clone()).is_err());
        let lock = root.join("runtime.lock");
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(create(&root, events.clone()).is_err());
        std::fs::remove_file(&lock).unwrap();
        let unrelated = directory.path().join("unrelated");
        std::fs::write(&unrelated, b"unchanged").unwrap();
        symlink(&unrelated, &lock).unwrap();
        assert!(create(&root, events).is_err());
        assert_eq!(std::fs::read(unrelated).unwrap(), b"unchanged");
    }

    #[tokio::test]
    async fn partial_import_and_conversion_capabilities_have_truthful_results() {
        let (_directory, manager, _events, source) = fixture();
        let mut settings = manager.settings.get().unwrap();
        settings.auto_enrich = false;
        manager.settings_save(&settings).unwrap();
        let invalid_source = source.with_extension("exe");
        std::fs::write(&invalid_source, b"unsupported book format").unwrap();
        let job = manager
            .import_books(&[
                invalid_source.to_str().unwrap().to_owned(),
                source.to_str().unwrap().to_owned(),
            ])
            .unwrap();
        assert!(manager.run_once().await.unwrap());
        let result = manager.jobs.get(&job.id).unwrap();
        assert_eq!(result.status, JobStatus::Completed);
        let result = result.result.unwrap();
        assert_eq!(result["imported"], 1);
        assert!(!result["errorsByCode"].as_object().unwrap().is_empty());
        assert!(
            manager
                .jobs
                .list()
                .unwrap()
                .iter()
                .all(|job| job.kind != JobKind::Enrich)
        );
        let caps = manager.conversion_capabilities();
        assert!(caps.inputs.contains(&BookFormat::Txt));
        assert!(caps.outputs.contains(&BookFormat::Mobi));
        assert!(!caps.inputs.contains(&BookFormat::Pdf));
        assert!(!caps.inputs.contains(&BookFormat::Cbz));
        assert!(!caps.inputs.contains(&BookFormat::Mobi));
        let failed = manager
            .import_books(&[invalid_source.to_str().unwrap().to_owned()])
            .unwrap();
        assert!(manager.run_once().await.unwrap());
        let failed = manager.jobs.get(&failed.id).unwrap();
        assert_eq!(failed.status, JobStatus::Failed);
        assert_eq!(failed.result.unwrap()["imported"], 0);
    }

    #[tokio::test]
    async fn only_a_new_profile_owner_recovers_an_interrupted_worker() {
        let (directory, manager, events, source) = fixture();
        let mut settings = manager.settings.get().unwrap();
        settings.auto_enrich = false;
        manager.settings_save(&settings).unwrap();
        let job = manager
            .import_books(&[source.to_str().unwrap().to_owned()])
            .unwrap();
        let claim = manager.jobs.claim_next(1).unwrap().unwrap();
        manager.jobs.update_progress(&claim, 0.6, "").unwrap();
        assert!(matches!(
            create(&directory.path().join("profile"), events.clone()),
            Err(AppError::ProfileInUse)
        ));
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().status,
            JobStatus::Running
        );
        drop(claim);
        drop(manager);
        let manager = create(&directory.path().join("profile"), events).unwrap();
        let recovered = manager.jobs.get(&job.id).unwrap();
        assert_eq!(recovered.status, JobStatus::Queued);
        assert_eq!(recovered.progress, 0.0);
        assert!(manager.run_once().await.unwrap());
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().status,
            JobStatus::Completed
        );
        assert_eq!(
            manager.library_list(&BookQuery::default()).unwrap().total,
            1
        );
    }

    fn indexed(index: usize, large: bool) -> IndexedDeviceBook {
        IndexedDeviceBook {
            device_id: "usb-synthetic".into(),
            relative_path: format!("book-{index}.epub"),
            book_id: None,
            sha256: None,
            title: if large {
                "X".repeat(4096)
            } else {
                format!("Book {index}")
            },
            authors: if large {
                vec!["Y".repeat(2000); 128]
            } else {
                vec!["Synthetic author".into()]
            },
            format: BookFormat::Epub,
            size_bytes: 1234,
            last_seen_at: "2026-10-09T00:00:00Z".into(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn device_index_result_is_bounded_without_claiming_a_complete_inventory() {
        let result = limited_index_result(
            "usb-synthetic",
            (0..600).map(|i| indexed(i, false)).collect(),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(result["total"], 600);
        assert_eq!(result["books"].as_array().unwrap().len(), 500);
        assert_eq!(result["truncated"], true);
        assert_eq!(result["warnings"][0], "deviceIndexResultTruncated");
        let large = limited_index_result(
            "usb-synthetic",
            (0..6).map(|i| indexed(i, true)).collect(),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(large["total"], 6);
        assert!(large["books"].as_array().unwrap().len() < 6);
        assert!(serde_json::to_vec(&large).unwrap().len() <= MAX_INDEX_RESULT_BYTES);
        let full =
            limited_index_result("usb-synthetic", vec![indexed(0, false)], Vec::new()).unwrap();
        assert_eq!(full["truncated"], false);
        assert_eq!(full["total"], 1);
        assert!(full["warnings"].as_array().unwrap().is_empty());
    }
}
