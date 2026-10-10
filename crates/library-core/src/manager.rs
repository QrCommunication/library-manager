//! Application lifecycle and background orchestration over focused core services.

use std::{
    collections::{BTreeMap, HashSet},
    fs::{File, TryLockError},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

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
    SettingsService, Storage, TransferService, WebClient,
    chat::ChatProgress,
    providers::public_provider_error,
    secure_fs::{self, AccessPolicy, SecureDir},
};

pub type ManagerEventSink = Arc<dyn Fn(&str, Value) + Send + Sync>;
const WORKER_TICK: Duration = Duration::from_millis(250);
const DEVICE_SCAN_INTERVAL: Duration = Duration::from_secs(5);
const DEVICE_SCAN_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BATCH: usize = 200;
const MAX_DEVICE_IMPORT_BOOKS: usize = 20_000;
const INDEX_PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
const MAX_INDEX_BOOKS: usize = 500;
const MAX_INDEX_RESULT_BYTES: usize = 768 * 1024;

#[derive(Debug)]
enum DispatchOutcome {
    PendingCompletion(Value),
    Committed(Box<Job>),
}

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
        // Acquire ownership before SQLite initialization or interrupted-job
        // recovery can mutate the profile from a second process.
        let profile_lock = Arc::new(lock_profile(root)?);
        let database = Database::new(root)?;
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
        let chat = chat.with_tools(library.clone(), storage.clone(), jobs.clone());
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
    pub fn book_review(
        &self,
        id: &str,
        job_id: &str,
        patch: &BookPatch,
        revision: u64,
    ) -> Result<Book> {
        let book = self.library.review_book(id, job_id, patch, revision)?;
        self.library_changed(vec![id.to_owned()], "metadataReview");
        Ok(book)
    }
    pub fn books_remove(
        &self,
        request_id: &str,
        books: &[crate::models::RemoveBookSelection],
    ) -> Result<crate::models::RemoveBooksResult> {
        let selections: Vec<_> = books
            .iter()
            .map(|book| (book.book_id.clone(), book.expected_revision))
            .collect();
        let operations = self.library.remove_books(request_id, &selections)?;
        let removed_book_ids: Vec<_> = selections.into_iter().map(|(id, _)| id).collect();
        self.library_changed(removed_book_ids.clone(), "catalogueRemove");
        Ok(crate::models::RemoveBooksResult {
            removed_book_ids,
            operations,
        })
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
    pub fn device_inventory(
        &self,
        id: &str,
        offset: u64,
        limit: u64,
        unknown_only: bool,
    ) -> Result<crate::devices::DeviceInventoryPage> {
        if limit == 0 || limit > MAX_BATCH as u64 {
            return Err(invalid("Inventory page size must be between one and 200"));
        }
        self.devices.resolve_connected(id)?;
        self.devices.inventory_page(id, offset, limit, unknown_only)
    }
    pub fn device_import(&self, id: &str, relative_paths: Option<Vec<String>>) -> Result<Job> {
        self.devices.resolve_connected(id)?;
        let inventory =
            self.devices
                .inventory_page(id, 0, MAX_DEVICE_IMPORT_BOOKS as u64, false)?;
        if let Some(paths) = &relative_paths {
            validate_device_import_paths(paths)?;
            let known: HashSet<_> = inventory
                .items
                .iter()
                .map(|book| book.relative_path.as_str())
                .collect();
            if paths.iter().any(|path| !known.contains(path.as_str())) {
                return Err(invalid("Device imports require inventoried relative paths"));
            }
        }
        // A null selection stays small and selects unknown books in the worker.
        self.jobs.enqueue_with_result(
            JobKind::Import,
            json!({"deviceId":id,"relativePaths":relative_paths}),
            json!({"deviceId":id}),
        )
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
        self.chat_send_authorized(conversation_id, text, book_ids, false)
            .await
    }
    pub async fn chat_send_authorized(
        &self,
        conversation_id: Option<&str>,
        text: &str,
        book_ids: &[String],
        allow_changes: bool,
    ) -> Result<Job> {
        let service = self.chat.clone();
        let conversation_id = conversation_id.map(str::to_owned);
        let text = text.to_owned();
        let ids = book_ids.to_vec();
        let prepared = blocking(move || {
            service.prepare_authorized(conversation_id.as_deref(), &text, &ids, allow_changes)
        })
        .await?;
        self.jobs.enqueue_with_result(JobKind::Chat,
            json!({"conversationId":prepared.conversation_id,"userMessageId":prepared.user_message_id,"text":prepared.text,"bookIds":prepared.book_ids,"allowChanges":prepared.allow_changes}),
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
        let mut active_jobs = BTreeMap::new();
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
                            active_jobs.insert(id.clone(), claim.cancellation.clone());
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
        scans.abort_all();
        self.cancel_and_settle_workers(active_jobs, &mut workers)
            .await;
        while scans.join_next().await.is_some() {}
    }
    async fn cancel_and_settle_workers(
        &self,
        active_jobs: BTreeMap<String, Arc<AtomicBool>>,
        workers: &mut JoinSet<String>,
    ) {
        // Signal every live claim before notifications; cancellation must survive bounded history.
        for cancellation in active_jobs.values() {
            cancellation.store(true, Ordering::Release);
        }
        for id in active_jobs.keys() {
            let _ = self.job_cancel(id);
        }
        // A tool may be settling a durable write on a blocking thread. Keep the profile owned.
        while workers.join_next().await.is_some() {}
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
        let await_cleanup = claim_needs_cleanup(&claim);
        // Dropping a spawn_blocking future detaches its thread; cooperative cleanup must finish.
        let outcome = if await_cleanup {
            self.dispatch(&claim).await
        } else {
            tokio::select! {
                result = self.dispatch(&claim) => result,
                _ = cancelled(claim.cancellation.clone()) => Err(AppError::Cancelled),
            }
        };
        self.settle_dispatch(&claim, outcome);
    }

    fn settle_dispatch(&self, claim: &ClaimedJob, outcome: Result<DispatchOutcome>) {
        match outcome {
            Ok(DispatchOutcome::Committed(job)) => {
                // Atomic completion retires the claim token. Its true value means
                // terminal publication here, rather than cancellation of the commit.
                self.jobs.notify_committed(&job);
                if let Some(result) = &job.result {
                    let reason = if result["review"]["state"] == "applied" {
                        "enrichment"
                    } else {
                        "metadataReview"
                    };
                    if let Some(id) = result["proposal"]["bookId"].as_str() {
                        self.library_changed(vec![id.to_owned()], reason);
                    }
                }
            }
            Ok(DispatchOutcome::PendingCompletion(result))
                if !claim.cancellation.load(Ordering::Acquire) =>
            {
                let _ = self.jobs.complete(claim, result);
            }
            Ok(DispatchOutcome::PendingCompletion(_)) | Err(AppError::Cancelled) => {
                let _ = self.jobs.cancel(&claim.job.id);
            }
            Err(error) => {
                let public = job_public_error(claim.job.kind, &error);
                match public.code {
                    ErrorCode::ProviderNotConfigured | ErrorCode::SecretStoreUnavailable => {
                        let _ = self.jobs.wait_for_configuration(claim, public);
                    }
                    ErrorCode::NetworkUnavailable | ErrorCode::RateLimited => {
                        let _ = self.jobs.wait_for_network(claim, public);
                    }
                    _ => {
                        let _ = self.jobs.fail(claim, public);
                    }
                }
            }
        }
    }

    async fn dispatch(&self, claim: &ClaimedJob) -> Result<DispatchOutcome> {
        check_cancelled(claim)?;
        let result = match claim.job.kind {
            JobKind::Import => self.import_job(claim).await,
            JobKind::Enrich => return self.enrich_job(claim).await,
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
                    allow_changes: payload.allow_changes,
                };
                let manager = self.clone();
                let progress_claim = claim.clone();
                let conversation_id = prepared.conversation_id.clone();
                let changed_books = Arc::new(Mutex::new(HashSet::<String>::new()));
                let progress_books = changed_books.clone();
                let progress: ChatProgress = Arc::new(move |_, message, ids| {
                    let _ = manager.jobs.update_progress(&progress_claim, 0.0, message);
                    if !ids.is_empty() {
                        if let Ok(mut changed) = progress_books.lock() {
                            changed.extend(ids.iter().cloned());
                            let mut changed: Vec<_> = changed.iter().cloned().collect();
                            changed.sort();
                            let _ = manager.jobs.update_result(&progress_claim, json!({"conversationId":conversation_id,"actions":{"changedBooks":changed.len(),"changedBookIds":changed}}));
                        }
                        manager.library_changed(ids.to_vec(), "assistant");
                    }
                });
                let result = self
                    .chat
                    .respond_with_control(
                        &prepared,
                        &settings,
                        claim.cancellation.clone(),
                        progress,
                    )
                    .await?;
                check_cancelled(claim)?;
                (self.event_sink)(
                    "chat:delta",
                    json!({"conversationId":result.conversation_id,"messageId":result.id,"text":result.content,"finished":true}),
                );
                let mut result = serde_json::to_value(result)?;
                if let Ok(changed) = changed_books.lock()
                    && !changed.is_empty()
                {
                    let mut changed: Vec<_> = changed.iter().cloned().collect();
                    changed.sort();
                    result["actions"] =
                        json!({"changedBooks":changed.len(),"changedBookIds":changed});
                }
                Ok(result)
            }
        };
        result.map(DispatchOutcome::PendingCompletion)
    }
    async fn import_job(&self, claim: &ClaimedJob) -> Result<Value> {
        let payload: ImportPayload = decode(&claim.payload)?;
        let (device_id, paths, expected_hashes) = match payload {
            ImportPayload::Local { paths } => {
                validate_batch(&paths)?;
                (None, paths, BTreeMap::new())
            }
            ImportPayload::Device {
                device_id,
                relative_paths,
            } => {
                self.devices.resolve_connected(&device_id)?;
                let inventory = self.devices.inventory_page(
                    &device_id,
                    0,
                    MAX_DEVICE_IMPORT_BOOKS as u64,
                    relative_paths.is_none(),
                )?;
                let paths = match relative_paths {
                    Some(paths) => {
                        validate_device_import_paths(&paths)?;
                        paths
                    }
                    None => inventory
                        .items
                        .iter()
                        .map(|book| book.relative_path.clone())
                        .collect(),
                };
                let expected_hashes = inventory
                    .items
                    .into_iter()
                    .map(|book| (book.relative_path, book.sha256))
                    .collect();
                (Some(device_id), paths, expected_hashes)
            }
        };
        let settings = self.settings.get()?;
        let mut imported = 0;
        let mut duplicates = 0;
        let mut warnings = Vec::new();
        let mut warning_count = 0_usize;
        let mut ids = Vec::new();
        let mut errors = BTreeMap::<String, u64>::new();
        for (index, path) in paths.iter().enumerate() {
            check_cancelled(claim)?;
            let library = self
                .library
                .clone()
                .with_cancellation(claim.cancellation.clone());
            let path = path.clone();
            let devices = self.devices.clone();
            let source_device = device_id.clone();
            let expected_hash = expected_hashes.get(&path).cloned().flatten();
            let is_device_import = source_device.is_some();
            match blocking(move || {
                let path = match source_device {
                    Some(id) => devices.resolve_import_path(&id, &path)?,
                    None => PathBuf::from(path),
                };
                if is_device_import {
                    let hash = expected_hash.ok_or_else(|| {
                        AppError::Unsupported(
                            "This inventoried file has no verified content hash".into(),
                        )
                    })?;
                    library.import_expected(&path, &hash)
                } else {
                    library.import(&path)
                }
            })
            .await
            {
                Ok(outcome) => {
                    if outcome.duplicate {
                        duplicates += 1;
                    } else {
                        imported += 1;
                    }
                    warning_count += outcome.warnings.len();
                    warnings.extend(
                        outcome
                            .warnings
                            .into_iter()
                            .take(MAX_INDEX_BOOKS.saturating_sub(warnings.len())),
                    );
                    if let Some(id) = &device_id {
                        self.devices.reconcile_import(id, &outcome.book.id)?;
                    }
                    self.library_changed(vec![outcome.book.id.clone()], "import");
                    if settings.auto_enrich
                        && (!outcome.duplicate || self.needs_recovered_enrichment(&outcome.book)?)
                    {
                        self.enqueue_enrichment(&outcome.book, "import")?;
                    }
                    if ids.len() < MAX_INDEX_BOOKS {
                        ids.push(outcome.book.id);
                    }
                }
                Err(AppError::Cancelled) => return Err(AppError::Cancelled),
                Err(error) => {
                    *errors.entry(error.code().into()).or_default() += 1;
                }
            }
            self.jobs
                .update_progress(claim, (index + 1) as f64 / paths.len() as f64, "")?;
            self.jobs.update_result(claim, json!({"deviceId":device_id,"imported":imported,"duplicates":duplicates,"warnings":warnings,"warningCount":warning_count,"warningsTruncated":warning_count > warnings.len(),"bookIds":ids,"bookIdsTruncated":imported + duplicates > ids.len(),"total":paths.len(),"processed":index + 1,"errorsByCode":errors}))?;
        }
        check_cancelled(claim)?;
        self.library_changed(ids.clone(), "import");
        if imported + duplicates == 0 && !errors.is_empty() {
            return Err(invalid("No source could be imported"));
        }
        Ok(
            json!({"deviceId":device_id,"imported":imported,"duplicates":duplicates,"warnings":warnings,"warningCount":warning_count,"warningsTruncated":warning_count > warnings.len(),"bookIds":ids,"bookIdsTruncated":imported + duplicates > ids.len(),"total":paths.len(),"processed":paths.len(),"errorsByCode":errors}),
        )
    }
    fn needs_recovered_enrichment(&self, book: &Book) -> Result<bool> {
        if book.metadata_status == MetadataStatus::Verified || book.metadata_confidence.is_some() {
            return Ok(false);
        }
        Ok(!self.jobs.has_enrichment_for_book(&book.id)?)
    }
    async fn enrich_job(&self, claim: &ClaimedJob) -> Result<DispatchOutcome> {
        let payload: EnrichPayload = decode(&claim.payload)?;
        if !matches!(
            payload.origin.as_str(),
            "import" | "manual" | "assistantReview"
        ) {
            return Err(invalid("Invalid enrichment origin"));
        }
        let mut settings = self.settings.get()?;
        if payload.origin == "import" && !settings.auto_enrich {
            return Ok(DispatchOutcome::PendingCompletion(
                json!({"skipped":"autoEnrichmentDisabled","bookId":payload.id}),
            ));
        }
        settings = self.configured_settings()?;
        if matches!(payload.origin.as_str(), "manual" | "assistantReview") {
            settings.auto_enrich = true;
        }
        let before = self.repository.get(&payload.id, &[])?;
        let baseline = payload.baseline_revision.unwrap_or(before.revision);
        let automatic_allowed = enrichment_may_apply(&before, baseline, &payload.origin);
        // Provider I/O is cancellable; once publication starts, wait for its blocking write.
        let outcome = tokio::select! {
            result = self.enrichment.propose(&payload.id, &settings) => result?,
            _ = cancelled(claim.cancellation.clone()) => return Err(AppError::Cancelled),
        };
        self.publish_enrichment(claim, &payload, baseline, automatic_allowed, outcome)
            .await
    }
    async fn publish_enrichment(
        &self,
        claim: &ClaimedJob,
        payload: &EnrichPayload,
        baseline: u64,
        automatic_allowed: bool,
        outcome: crate::enrichment::EnrichmentOutcome,
    ) -> Result<DispatchOutcome> {
        check_cancelled(claim)?;
        let auto_apply = outcome.auto_applicable
            && automatic_allowed
            && outcome.expected_revision == baseline
            && enrichment_may_apply(
                &self.repository.get(&payload.id, &[])?,
                baseline,
                &payload.origin,
            );
        let library = self
            .library
            .clone()
            .with_cancellation(claim.cancellation.clone());
        let jobs = self.jobs.clone();
        let claim = claim.clone();
        let origin = payload.origin.clone();
        let completed = blocking(move || {
            library.publish_enrichment(
                &jobs,
                &claim,
                &outcome.proposal,
                outcome.expected_revision,
                auto_apply,
                &origin,
            )
        })
        .await?;
        Ok(DispatchOutcome::Committed(Box::new(completed)))
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
            let manager = self.clone();
            let claim = claim.clone();
            blocking(move || {
                let mut last_report = Instant::now();
                let mut last_library_report = Instant::now();
                let mut last_phase = String::new();
                let mut last_processed = 0_u64;
                let mut previous_progress = claim.job.progress;
                devices.index_with_progress(&id, claim.cancellation.clone(), |progress| {
                    check_cancelled(&claim)?;
                    let phase_changed = progress.phase != last_phase;
                    let processed_changed = progress.processed_books != last_processed;
                    if phase_changed
                        || processed_changed
                        || last_report.elapsed() >= INDEX_PROGRESS_INTERVAL
                    {
                        let measured = if progress.phase == "discovering" {
                            0.0
                        } else if progress.total_bytes > 0 {
                            0.99 * progress.bytes_read as f64 / progress.total_bytes as f64
                        } else if progress.total_books > 0 {
                            0.99 * progress.processed_books as f64 / progress.total_books as f64
                        } else {
                            0.0
                        };
                        previous_progress = previous_progress.max(measured.min(0.99));
                        manager.jobs.update_result(
                            &claim,
                            json!({"deviceId":id,"indexProgress":progress}),
                        )?;
                        manager
                            .jobs
                            .update_progress(&claim, previous_progress, "")?;
                        last_report = Instant::now();
                        last_phase = progress.phase.clone();
                        last_processed = progress.processed_books;
                    }
                    if phase_changed || last_library_report.elapsed() >= INDEX_PROGRESS_INTERVAL {
                        manager.library_changed(Vec::new(), "deviceIndexProgress");
                        last_library_report = Instant::now();
                    }
                    Ok(())
                })
            })
            .await?
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

fn claim_needs_cleanup(claim: &ClaimedJob) -> bool {
    matches!(
        claim.job.kind,
        JobKind::Transfer | JobKind::Chat | JobKind::Enrich
    ) || (claim.job.kind == JobKind::DeviceIndex
        && claim
            .payload
            .get("deviceId")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.starts_with("calibre-") && !id.starts_with("crosspoint-")))
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
    let root = secure_fs::absolute_path(root)?;
    if !root
        .components()
        .any(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return Err(invalid("The filesystem root cannot be a library profile"));
    }
    let directory = SecureDir::open(&root, true, AccessPolicy::Private)?;
    let name = std::ffi::OsStr::new("runtime.lock");
    let file = match directory.open_regular(name) {
        Ok(file) => file,
        Err(AppError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            match directory.create_new(name, AccessPolicy::Private) {
                Ok(file) => {
                    directory.sync()?;
                    file
                }
                Err(AppError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    directory.open_regular(name)?
                }
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    if !secure_fs::is_private_read_write(&file)? {
        return Err(invalid("Profile lock must be a private regular file"));
    }
    let identity = secure_fs::identity(&file)?;
    if secure_fs::identity(&directory.open_regular(name)?)? != identity {
        return Err(AppError::Conflict("Profile lock identity changed".into()));
    }
    file.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => AppError::ProfileInUse,
        TryLockError::Error(error) => AppError::Io(error),
    })?;
    if secure_fs::identity(&directory.open_regular(name)?)? != identity {
        return Err(AppError::Conflict(
            "Profile lock identity changed during acquisition".into(),
        ));
    }
    // Only the lock owner may set the profile policy. Preserve SQLite's
    // owner-only inheritance instead of replacing it with a non-inheritable ACL.
    secure_fs::make_private_inheritable(directory.as_file())?;
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
fn validate_device_import_paths(values: &[String]) -> Result<()> {
    if values.is_empty()
        || values.len() > MAX_DEVICE_IMPORT_BOOKS
        || values.iter().any(|value| {
            value.is_empty()
                || value.len() > 4096
                || value.contains('\0')
                || !Path::new(value)
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
        })
    {
        return Err(invalid(
            "Device import needs one to 20000 safe relative paths",
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
#[serde(untagged)]
enum ImportPayload {
    Local {
        paths: Vec<String>,
    },
    Device {
        #[serde(rename = "deviceId")]
        device_id: String,
        #[serde(rename = "relativePaths")]
        relative_paths: Option<Vec<String>>,
    },
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
    #[serde(default)]
    allow_changes: bool,
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
    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt, symlink};
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
        let directory = tempfile::Builder::new()
            .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
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
    fn catalogue_removal_returns_only_public_receipts_replays_and_notifies_restoration() {
        let (_directory, manager, events, source) = fixture();
        let book = manager.library.import(&source).unwrap().book;
        let completed = manager.enqueue_enrichment(&book, "manual").unwrap();
        let claim = manager.jobs.claim_next(1).unwrap().unwrap();
        manager
            .jobs
            .complete(&claim, json!({"bookId":book.id,"skipped":"fixture"}))
            .unwrap();
        let previous_operations = manager.library.operations().unwrap().len();
        events.lock().unwrap().clear();
        let selected = vec![crate::models::RemoveBookSelection {
            book_id: book.id.clone(),
            expected_revision: book.revision,
        }];
        let request = "aa5e42f6-8d44-4d32-a1d4-51dc32416821";
        let result = manager.books_remove(request, &selected).unwrap();
        assert_eq!(
            result.removed_book_ids.as_slice(),
            std::slice::from_ref(&book.id)
        );
        assert_eq!(result.operations.len(), 1);
        assert_eq!(result.operations[0].kind, "catalogueRemove");
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("relativePath"));
        assert!(!serialized.contains("sha256"));
        assert!(!serialized.contains(source.to_str().unwrap()));
        assert_eq!(
            manager.library_list(&BookQuery::default()).unwrap().total,
            0
        );
        assert_eq!(manager.books_remove(request, &selected).unwrap(), result);
        assert_eq!(
            manager.library.operations().unwrap().len(),
            previous_operations + 1
        );
        assert_eq!(
            manager.jobs.get(&completed.id).unwrap().status,
            JobStatus::Completed
        );
        let notifications = events.lock().unwrap().clone();
        assert_eq!(notifications.len(), 2);
        for (name, value) in notifications {
            assert_eq!(name, "library:changed");
            assert_eq!(
                value,
                json!({"bookIds":[book.id],"reason":"catalogueRemove"})
            );
        }
        manager.operation_undo(&result.operations[0].id).unwrap();
        assert_eq!(manager.book_get(&book.id).unwrap().title, book.title);
        assert_eq!(
            events.lock().unwrap().last().unwrap(),
            &(
                "library:changed".into(),
                json!({"bookIds":[],"reason":"undo"})
            )
        );
        assert_eq!(std::fs::read(source).unwrap(), b"Premier paragraphe.\n\nUn deuxi\xc3\xa8me chapitre : \xc3\xa9t\xc3\xa9, lumi\xc3\xa8re.");
    }

    #[test]
    fn catalogue_removal_keeps_active_job_guard_and_revisions_authoritative() {
        let (_directory, manager, events, source) = fixture();
        let book = manager.library.import(&source).unwrap().book;
        let job = manager.book_enrich(&book.id).unwrap();
        events.lock().unwrap().clear();
        let selected = vec![crate::models::RemoveBookSelection {
            book_id: book.id.clone(),
            expected_revision: book.revision,
        }];
        let request = "d2ac9aa5-d4c0-4a9d-8930-27c9f723dc76";
        let error = manager.books_remove(request, &selected).unwrap_err();
        assert_eq!(PublicError::from(&error).code, ErrorCode::OperationConflict);
        assert_eq!(manager.book_get(&book.id).unwrap().revision, book.revision);
        assert!(events.lock().unwrap().is_empty());
        manager.jobs.cancel(&job.id).unwrap();
        events.lock().unwrap().clear();
        let stale = vec![crate::models::RemoveBookSelection {
            book_id: book.id.clone(),
            expected_revision: book.revision + 1,
        }];
        assert!(matches!(
            manager.books_remove(request, &stale),
            Err(AppError::RevisionConflict)
        ));
        assert!(events.lock().unwrap().is_empty());
        assert!(matches!(
            manager.books_remove("not-a-uuid", &selected),
            Err(AppError::InvalidInput(_))
        ));
        assert!(events.lock().unwrap().is_empty());
        assert_eq!(
            manager
                .books_remove(request, &selected)
                .unwrap()
                .removed_book_ids,
            [book.id]
        );
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
        assert_eq!(
            manager.jobs.payload(&job.id).unwrap()["allowChanges"],
            false
        );
    }

    #[tokio::test]
    async fn chat_authorization_is_persisted_and_forged_payload_permissions_are_rejected() {
        let (directory, manager, events, source) = fixture();
        let book = manager.library.import(&source).unwrap().book;
        let job = manager
            .chat_send_authorized(
                None,
                "Corrige ce livre",
                std::slice::from_ref(&book.id),
                true,
            )
            .await
            .unwrap();
        let payload = manager.jobs.payload(&job.id).unwrap();
        assert_eq!(payload["allowChanges"], true);
        drop(manager);
        let manager = create(&directory.path().join("profile"), events).unwrap();
        assert_eq!(manager.jobs.payload(&job.id).unwrap(), payload);
        let stored: ChatPayload = decode(&payload).unwrap();
        let mut forged = PreparedChat {
            conversation_id: stored.conversation_id,
            user_message_id: stored.user_message_id,
            text: stored.text,
            book_ids: stored.book_ids,
            allow_changes: false,
        };
        assert!(matches!(
            manager.chat.respond(&forged, &Settings::default()).await,
            Err(AppError::Conflict(_))
        ));
        forged.allow_changes = true;
        forged.book_ids.clear();
        assert!(matches!(
            manager.chat.respond(&forged, &Settings::default()).await,
            Err(AppError::Conflict(_))
        ));
    }

    #[test]
    fn legacy_chat_jobs_are_read_only_and_permission_values_require_booleans() {
        let legacy = json!({"conversationId":"conversation","userMessageId":"message","text":"Question","bookIds":[]});
        assert!(!decode::<ChatPayload>(&legacy).unwrap().allow_changes);
        for invalid_permission in [json!(null), json!("true"), json!(1), json!({})] {
            let mut payload = legacy.clone();
            payload["allowChanges"] = invalid_permission;
            assert!(decode::<ChatPayload>(&payload).is_err());
        }
        let mut allowed = legacy;
        allowed["allowChanges"] = json!(true);
        assert!(decode::<ChatPayload>(&allowed).unwrap().allow_changes);
    }

    #[test]
    fn assistant_review_preserves_revision_guard_and_never_applies_automatically() {
        let (_directory, manager, _events, source) = fixture();
        let book = manager.library.import(&source).unwrap().book;
        assert!(enrichment_may_apply(&book, book.revision, "manual"));
        assert!(!enrichment_may_apply(
            &book,
            book.revision,
            "assistantReview"
        ));
        assert!(!enrichment_may_apply(&book, book.revision + 1, "manual"));
        let job = manager
            .enqueue_enrichment(&book, "assistantReview")
            .unwrap();
        let payload = manager.jobs.payload(&job.id).unwrap();
        assert_eq!(payload["origin"], "assistantReview");
        assert_eq!(payload["baselineRevision"], book.revision);
    }

    fn proposed_outcome(
        book: &Book,
        auto_applicable: bool,
    ) -> crate::enrichment::EnrichmentOutcome {
        crate::enrichment::EnrichmentOutcome {
            proposal: crate::MetadataProposal {
                book_id: book.id.clone(),
                patch: BookPatch {
                    title: Some("Proposed title".into()),
                    ..Default::default()
                },
                confidence: 0.95,
                evidence: vec![crate::MetadataEvidence {
                    field: "title".into(),
                    value: "Proposed title".into(),
                    confidence: 0.95,
                    source_urls: vec!["https://example.org/edition".into()],
                }],
                warnings: vec!["Synthetic manual review".into()],
                provider_id: ProviderId::Minimax,
                model_id: "synthetic-model".into(),
            },
            expected_revision: book.revision,
            auto_applicable,
        }
    }

    fn settle_test_publication(
        manager: &LibraryManager,
        claim: &ClaimedJob,
        outcome: DispatchOutcome,
    ) -> Value {
        let DispatchOutcome::Committed(job) = outcome else {
            panic!("Expected an atomic publication");
        };
        let result = job.result.clone().unwrap();
        manager.settle_dispatch(claim, Ok(DispatchOutcome::Committed(job)));
        result
    }

    #[tokio::test]
    async fn committed_enrichment_notifies_once_with_durable_book_and_job_and_survives_callback_cancel()
     {
        let (_directory, mut manager, events, source) = fixture();
        let book = manager.library.import(&source).unwrap().book;
        let job = manager.enqueue_enrichment(&book, "manual").unwrap();
        let claim = manager.jobs.claim_next(1).unwrap().unwrap();
        let payload: EnrichPayload = decode(&claim.payload).unwrap();
        let callbacks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let callback_count = callbacks.clone();
        let repository = manager.repository.clone();
        let jobs = manager.jobs.clone();
        manager.jobs = manager
            .jobs
            .clone()
            .with_event_callback(Arc::new(move |job| {
                assert_eq!(job.status, JobStatus::Completed);
                let current = repository.get(&job.book_ids[0], &[]).unwrap();
                assert_eq!(current.title, "Proposed title");
                assert_eq!(current.metadata_status, MetadataStatus::Verified);
                assert_eq!(jobs.get(&job.id).unwrap(), *job);
                assert_eq!(
                    job.result.as_ref().unwrap()["review"]["reviewRevision"],
                    current.revision
                );
                assert_eq!(jobs.cancel(&job.id).unwrap().status, JobStatus::Completed);
                callback_count.fetch_add(1, Ordering::Relaxed);
            }));
        events.lock().unwrap().clear();
        let outcome = manager
            .publish_enrichment(
                &claim,
                &payload,
                book.revision,
                true,
                proposed_outcome(&book, true),
            )
            .await
            .unwrap();
        assert!(claim.cancellation.load(Ordering::Acquire));
        assert_eq!(callbacks.load(Ordering::Relaxed), 0);
        assert!(events.lock().unwrap().is_empty());
        manager.settle_dispatch(&claim, Ok(outcome));
        assert_eq!(callbacks.load(Ordering::Relaxed), 1);
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().status,
            JobStatus::Completed
        );
        let changes: Vec<_> = events
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name == "library:changed")
            .cloned()
            .collect();
        assert_eq!(
            changes,
            vec![(
                "library:changed".to_owned(),
                json!({"bookIds":[book.id],"reason":"enrichment"})
            )]
        );
    }

    #[tokio::test]
    async fn personal_progress_between_atomic_commit_and_notification_keeps_pending_review_rebased()
    {
        let (_directory, manager, events, source) = fixture();
        let book = manager.library.import(&source).unwrap().book;
        let job = manager.enqueue_enrichment(&book, "manual").unwrap();
        let claim = manager.jobs.claim_next(1).unwrap().unwrap();
        let payload: EnrichPayload = decode(&claim.payload).unwrap();
        let outcome = manager
            .publish_enrichment(
                &claim,
                &payload,
                book.revision,
                true,
                proposed_outcome(&book, false),
            )
            .await
            .unwrap();
        let pending = manager.book_get(&book.id).unwrap();
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().status,
            JobStatus::Completed
        );
        manager
            .repository
            .save_progress(&book.id, "section:0", 0.4)
            .unwrap();
        let current = manager.book_get(&book.id).unwrap();
        assert!(current.revision > pending.revision);
        let rebased = manager.jobs.get(&job.id).unwrap();
        assert_eq!(
            rebased.result.as_ref().unwrap()["review"]["state"],
            "pending"
        );
        assert_eq!(
            rebased.result.as_ref().unwrap()["review"]["sourceRevision"],
            book.revision
        );
        assert_eq!(
            rebased.result.as_ref().unwrap()["review"]["reviewRevision"],
            current.revision
        );
        events.lock().unwrap().clear();
        manager.settle_dispatch(&claim, Ok(outcome));
        assert_eq!(manager.jobs.get(&job.id).unwrap(), rebased);
        assert_eq!(manager.book_get(&book.id).unwrap(), current);
        let events = events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|(name, _)| name == "job:updated")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|(name, _)| name == "library:changed")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn cancelled_or_stale_enrichment_claim_cannot_publish_a_book_or_completion() {
        let (_directory, manager, _events, source) = fixture();
        let book = manager.library.import(&source).unwrap().book;
        let job = manager.enqueue_enrichment(&book, "manual").unwrap();
        let claim = manager.jobs.claim_next(1).unwrap().unwrap();
        let payload: EnrichPayload = decode(&claim.payload).unwrap();
        let mut stale = claim.clone();
        stale.attempt += 1;
        assert!(matches!(
            manager
                .publish_enrichment(
                    &stale,
                    &payload,
                    book.revision,
                    true,
                    proposed_outcome(&book, true)
                )
                .await,
            Err(AppError::Conflict(_))
        ));
        assert_eq!(manager.book_get(&book.id).unwrap(), book);
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().status,
            JobStatus::Running
        );
        manager.jobs.cancel(&job.id).unwrap();
        let outcome = manager
            .publish_enrichment(
                &claim,
                &payload,
                book.revision,
                true,
                proposed_outcome(&book, true),
            )
            .await;
        assert!(matches!(&outcome, Err(AppError::Cancelled)));
        manager.settle_dispatch(&claim, outcome);
        assert_eq!(manager.book_get(&book.id).unwrap(), book);
        let cancelled = manager.jobs.get(&job.id).unwrap();
        assert_eq!(cancelled.status, JobStatus::Cancelled);
        assert!(cancelled.result.is_none());
    }

    #[tokio::test]
    async fn manual_analysis_of_verified_book_publishes_pending_review_and_can_be_consumed() {
        let (_directory, manager, events, source) = fixture();
        let imported = manager.library.import(&source).unwrap().book;
        let book = manager
            .book_update(
                &imported.id,
                &BookPatch {
                    title: Some("Human title".into()),
                    ..Default::default()
                },
                imported.revision,
            )
            .unwrap();
        assert_eq!(book.metadata_status, MetadataStatus::Verified);
        let job = manager.enqueue_enrichment(&book, "manual").unwrap();
        let claim = manager.jobs.claim_next(1).unwrap().unwrap();
        let payload: EnrichPayload = decode(&claim.payload).unwrap();
        let outcome = proposed_outcome(&book, false);
        let original_proposal = serde_json::to_value(&outcome.proposal).unwrap();
        let result = manager
            .publish_enrichment(&claim, &payload, book.revision, true, outcome)
            .await
            .unwrap();
        let result = settle_test_publication(&manager, &claim, result);
        let published = manager.book_get(&book.id).unwrap();
        assert_eq!(published.title, "Human title");
        assert_eq!(published.metadata_status, MetadataStatus::NeedsReview);
        assert_eq!(result["proposal"], original_proposal);
        assert_eq!(result["review"]["state"], "pending");
        assert_eq!(result["review"]["sourceRevision"], book.revision);
        assert_eq!(result["review"]["reviewRevision"], published.revision);
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().result,
            Some(result.clone())
        );
        let reviewed = manager
            .book_review(&book.id, &job.id, &BookPatch::default(), published.revision)
            .unwrap();
        assert_eq!(reviewed.metadata_status, MetadataStatus::Verified);
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().result.unwrap()["review"]["state"],
            "applied"
        );
        assert!(
            events
                .lock()
                .unwrap()
                .iter()
                .any(|(name, value)| name == "library:changed"
                    && value["reason"] == "metadataReview")
        );
    }

    #[tokio::test]
    async fn automatic_publication_preserves_proofs_and_records_the_committed_revision() {
        let (_directory, manager, _events, source) = fixture();
        let book = manager.library.import(&source).unwrap().book;
        let job = manager.enqueue_enrichment(&book, "manual").unwrap();
        let claim = manager.jobs.claim_next(1).unwrap().unwrap();
        let payload: EnrichPayload = decode(&claim.payload).unwrap();
        let outcome = proposed_outcome(&book, true);
        let proposal = serde_json::to_value(&outcome.proposal).unwrap();
        let result = manager
            .publish_enrichment(&claim, &payload, book.revision, true, outcome)
            .await
            .unwrap();
        let result = settle_test_publication(&manager, &claim, result);
        let applied = manager.book_get(&book.id).unwrap();
        assert_eq!(applied.title, "Proposed title");
        assert_eq!(applied.metadata_status, MetadataStatus::Verified);
        assert_eq!(result["proposal"], proposal);
        assert_eq!(result["review"]["state"], "applied");
        assert_eq!(result["review"]["sourceRevision"], book.revision);
        assert_eq!(result["review"]["reviewRevision"], applied.revision);
        assert_eq!(result["review"]["resolvedRevision"], applied.revision);
        assert_eq!(manager.jobs.get(&job.id).unwrap().result, Some(result));
    }

    #[tokio::test]
    async fn deferred_import_does_not_reopen_a_book_verified_by_its_owner() {
        let (_directory, manager, _events, source) = fixture();
        let imported = manager.library.import(&source).unwrap().book;
        let job = manager.enqueue_enrichment(&imported, "import").unwrap();
        let claim = manager.jobs.claim_next(1).unwrap().unwrap();
        let payload: EnrichPayload = decode(&claim.payload).unwrap();
        let verified = manager
            .book_update(
                &imported.id,
                &BookPatch {
                    title: Some("Owner verified edition".into()),
                    ..Default::default()
                },
                imported.revision,
            )
            .unwrap();
        let result = manager
            .publish_enrichment(
                &claim,
                &payload,
                imported.revision,
                false,
                proposed_outcome(&verified, true),
            )
            .await
            .unwrap();
        let result = settle_test_publication(&manager, &claim, result);
        assert_eq!(manager.book_get(&verified.id).unwrap(), verified);
        assert_eq!(result["review"]["state"], "obsolete");
        assert_eq!(result["review"]["sourceRevision"], verified.revision);
        assert_eq!(result["review"]["resolvedRevision"], verified.revision);
        assert_eq!(manager.jobs.get(&job.id).unwrap().result, Some(result));
    }

    #[tokio::test]
    async fn stale_or_cancelled_analysis_cannot_publish_review_and_assistant_stays_manual() {
        let (_directory, manager, _events, source) = fixture();
        let book = manager.library.import(&source).unwrap().book;
        let job = manager
            .enqueue_enrichment(&book, "assistantReview")
            .unwrap();
        let claim = manager.jobs.claim_next(1).unwrap().unwrap();
        let payload: EnrichPayload = decode(&claim.payload).unwrap();
        let personal = manager
            .book_update(
                &book.id,
                &BookPatch {
                    favorite: Some(true),
                    ..Default::default()
                },
                book.revision,
            )
            .unwrap();
        assert!(matches!(
            manager
                .publish_enrichment(
                    &claim,
                    &payload,
                    book.revision,
                    false,
                    proposed_outcome(&book, true)
                )
                .await,
            Err(AppError::RevisionConflict)
        ));
        assert!(manager.jobs.get(&job.id).unwrap().result.is_none());
        let result = manager
            .publish_enrichment(
                &claim,
                &payload,
                book.revision,
                false,
                proposed_outcome(&personal, true),
            )
            .await
            .unwrap();
        let result = settle_test_publication(&manager, &claim, result);
        assert_eq!(result["review"]["state"], "pending");
        assert_eq!(manager.book_get(&book.id).unwrap().title, book.title);
        let current = manager.book_get(&book.id).unwrap();
        manager.jobs.cancel(&job.id).unwrap();
        assert!(matches!(
            manager
                .publish_enrichment(
                    &claim,
                    &payload,
                    current.revision,
                    true,
                    proposed_outcome(&current, true)
                )
                .await,
            Err(AppError::Cancelled)
        ));
        assert_eq!(
            manager.book_get(&book.id).unwrap().revision,
            current.revision
        );
    }

    #[test]
    fn cleanup_covers_chat_tools_and_keeps_network_index_cancellation() {
        let (_directory, manager, _events, _source) = fixture();
        for (kind, payload, expected) in [
            (JobKind::Chat, json!({}), true),
            (JobKind::Transfer, json!({}), true),
            (JobKind::DeviceIndex, json!({"deviceId":"usb-card"}), true),
            (
                JobKind::DeviceIndex,
                json!({"deviceId":"calibre-host"}),
                false,
            ),
            (
                JobKind::DeviceIndex,
                json!({"deviceId":"crosspoint-host"}),
                false,
            ),
            (JobKind::Enrich, json!({}), true),
        ] {
            let job = manager.jobs.enqueue(kind, payload).unwrap();
            let claim = manager.jobs.claim_next(1).unwrap().unwrap();
            assert_eq!(claim.job.id, job.id);
            assert_eq!(claim_needs_cleanup(&claim), expected);
            manager.jobs.cancel(&job.id).unwrap();
        }
    }

    #[tokio::test]
    async fn shutdown_signals_chat_and_waits_for_blocking_tool_cleanup() {
        let (_directory, manager, _events, _source) = fixture();
        let job = manager.jobs.enqueue(JobKind::Chat, json!({})).unwrap();
        let claim = manager.jobs.claim_next(1).unwrap().unwrap();
        let cancellation = claim.cancellation.clone();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let worker_barrier = barrier.clone();
        let (started, entered) = tokio::sync::oneshot::channel();
        let mut workers = JoinSet::new();
        let id = job.id.clone();
        workers.spawn(async move {
            blocking(move || {
                started.send(()).unwrap();
                worker_barrier.wait();
                Ok(())
            })
            .await
            .unwrap();
            id
        });
        entered.await.unwrap();
        let active = BTreeMap::from([(job.id.clone(), cancellation.clone())]);
        let shutting_down = manager.clone();
        let mut shutdown = tokio::spawn(async move {
            shutting_down
                .cancel_and_settle_workers(active, &mut workers)
                .await;
        });
        let early = tokio::time::timeout(Duration::from_millis(150), &mut shutdown).await;
        let cleanup_waited = early.is_err();
        barrier.wait();
        match early {
            Ok(result) => result.unwrap(),
            Err(_) => shutdown.await.unwrap(),
        }
        assert!(cleanup_waited, "Shutdown detached a blocking tool write");
        assert!(cancellation.load(Ordering::Acquire));
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().status,
            JobStatus::Cancelled
        );
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
        #[cfg(unix)]
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
        #[cfg(windows)]
        let original_permissions = std::fs::metadata(&lock).unwrap().permissions();
        #[cfg(windows)]
        {
            let mut permissions = original_permissions.clone();
            permissions.set_readonly(true);
            std::fs::set_permissions(&lock, permissions).unwrap();
        }
        assert!(create(&root, events.clone()).is_err());
        #[cfg(windows)]
        {
            std::fs::set_permissions(&lock, original_permissions).unwrap();
        }
        std::fs::remove_file(&lock).unwrap();
        let unrelated = directory.path().join("unrelated");
        std::fs::write(&unrelated, b"unchanged").unwrap();
        #[cfg(unix)]
        symlink(&unrelated, &lock).unwrap();
        #[cfg(windows)]
        {
            let target = directory.path().join("unrelated-directory");
            std::fs::create_dir(&target).unwrap();
            std::fs::write(target.join("preserved"), b"unchanged").unwrap();
            let output = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&lock)
                .arg(&target)
                .output()
                .unwrap();
            assert!(output.status.success(), "junction fixture creation failed");
        }
        assert!(create(&root, events).is_err());
        assert_eq!(std::fs::read(unrelated).unwrap(), b"unchanged");
    }

    #[test]
    fn concurrent_profile_openers_have_only_one_owner() {
        let directory = tempfile::Builder::new()
            .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
        let root = directory.path().join("profile");
        let start = Arc::new(std::sync::Barrier::new(2));
        let opened = Arc::new(std::sync::Barrier::new(2));
        let workers = (0..2)
            .map(|_| {
                let root = root.clone();
                let start = start.clone();
                let opened = opened.clone();
                std::thread::spawn(move || {
                    start.wait();
                    let result = create(&root, Arc::new(Mutex::new(Vec::new())));
                    opened.wait();
                    result
                })
            })
            .collect::<Vec<_>>();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(AppError::ProfileInUse)))
                .count(),
            1
        );
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
        let profile = directory.path().join("profile");
        let profile_directory = SecureDir::open(&profile, false, AccessPolicy::Private).unwrap();
        let profile_identity = profile_directory.identity().unwrap();
        assert!(secure_fs::is_private_inheritable(profile_directory.as_file()).unwrap());
        assert!(matches!(
            create(&profile, events.clone()),
            Err(AppError::ProfileInUse)
        ));
        let retained_directory = SecureDir::open(&profile, false, AccessPolicy::Private).unwrap();
        assert_eq!(retained_directory.identity().unwrap(), profile_identity);
        assert!(secure_fs::is_private_inheritable(retained_directory.as_file()).unwrap());
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().status,
            JobStatus::Running
        );
        drop(claim);
        drop(manager);
        let manager = create(&profile, events).unwrap();
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

    fn encode_mountinfo_field(value: &str) -> String {
        let mut encoded = String::new();
        for character in value.chars() {
            match character {
                '\\' => encoded.push_str("\\134"),
                ' ' => encoded.push_str("\\040"),
                '\t' => encoded.push_str("\\011"),
                '\n' => encoded.push_str("\\012"),
                _ => encoded.push(character),
            }
        }
        encoded
    }

    #[test]
    fn mountinfo_fixture_encodes_special_characters_once() {
        assert_eq!(
            encode_mountinfo_field("C:\\Books\\été \t\n"),
            "C:\\134Books\\134été\\040\\011\\012"
        );
        assert_eq!(encode_mountinfo_field(r"\040"), r"\134040");
    }

    fn device_fixture(count: usize) -> (TempDir, LibraryManager, Events, PathBuf, PathBuf, String) {
        let directory = tempfile::Builder::new()
            .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let profile = directory.path().join("profile");
        let mut manager = create(&profile, events.clone()).unwrap();
        let mut settings = manager.settings.get().unwrap();
        settings.auto_enrich = false;
        manager.settings_save(&settings).unwrap();
        let media = directory.path().join("media");
        let card = media.join("Xteink fixture");
        let gvfs = directory.path().join("gvfs");
        let mountinfo = directory.path().join("mountinfo");
        std::fs::create_dir_all(&card).unwrap();
        std::fs::create_dir_all(&gvfs).unwrap();
        for index in 0..count {
            std::fs::write(
                card.join(format!("book-{index}.pdf")),
                format!("%PDF synthetic fixture {index}"),
            )
            .unwrap();
        }
        std::fs::write(
            &mountinfo,
            format!(
                "42 1 8:1 / {} rw,nosuid - vfat UUID=TEST-CARD rw\n",
                encode_mountinfo_field(&card.to_string_lossy())
            ),
        )
        .unwrap();
        manager.devices =
            DeviceService::scan_at(Database::new(&profile).unwrap(), &mountinfo, &media, &gvfs);
        let devices = manager.devices.scan().unwrap();
        let id = devices
            .iter()
            .find(|device| device.connected)
            .unwrap()
            .id
            .clone();
        *manager.cached_devices.lock().unwrap() = devices;
        (directory, manager, events, card, mountinfo, id)
    }

    #[tokio::test]
    async fn device_import_preserves_source_reconciles_presence_and_deduplicates() {
        let (_directory, manager, _events, card, _mountinfo, id) = device_fixture(1);
        manager.devices.index(&id).unwrap();
        let original = std::fs::read(card.join("book-0.pdf")).unwrap();
        let job = manager
            .device_import(&id, Some(vec!["book-0.pdf".into()]))
            .unwrap();
        assert_eq!(job.result.unwrap()["deviceId"], id);
        assert!(manager.run_once().await.unwrap());
        let result = manager.jobs.get(&job.id).unwrap();
        assert_eq!(result.status, JobStatus::Completed);
        assert_eq!(result.result.unwrap()["imported"], 1);
        assert_eq!(std::fs::read(card.join("book-0.pdf")).unwrap(), original);
        let local = manager.library_list(&BookQuery::default()).unwrap();
        assert_eq!(local.total, 1);
        assert_eq!(local.items[0].on_device_ids, vec![id.clone()]);
        assert_eq!(
            manager.device_inventory(&id, 0, 200, true).unwrap().total,
            0
        );
        let repeated = manager
            .device_import(&id, Some(vec!["book-0.pdf".into()]))
            .unwrap();
        assert!(manager.run_once().await.unwrap());
        assert_eq!(
            manager.jobs.get(&repeated.id).unwrap().result.unwrap()["duplicates"],
            1
        );
        assert_eq!(
            manager.library_list(&BookQuery::default()).unwrap().total,
            1
        );
    }

    #[tokio::test]
    async fn device_import_all_uses_full_backend_inventory_and_continues_after_file_changes() {
        let (_directory, manager, _events, card, _mountinfo, id) = device_fixture(202);
        manager.devices.index(&id).unwrap();
        let job = manager.device_import(&id, None).unwrap();
        std::fs::write(card.join("book-0.pdf"), b"changed after inventory").unwrap();
        assert!(manager.run_once().await.unwrap());
        let finished = manager.jobs.get(&job.id).unwrap();
        assert_eq!(finished.status, JobStatus::Completed);
        let result = finished.result.unwrap();
        assert_eq!(result["deviceId"], id);
        assert_eq!(result["total"], 202);
        assert_eq!(result["processed"], 202);
        assert_eq!(result["imported"], 201);
        assert_eq!(result["errorsByCode"]["conflict"], 1);
        assert_eq!(
            manager.library_list(&BookQuery::default()).unwrap().total,
            201
        );
        assert_eq!(
            manager.device_inventory(&id, 0, 200, true).unwrap().total,
            1
        );
    }

    #[tokio::test]
    async fn device_import_revalidates_connection_in_worker_and_rejects_noninventoried_paths() {
        let (_directory, manager, _events, _card, mountinfo, id) = device_fixture(1);
        manager.devices.index(&id).unwrap();
        assert!(
            manager
                .device_import(&id, Some(vec!["../outside.pdf".into()]))
                .is_err()
        );
        assert!(
            manager
                .device_import(&id, Some(vec!["absent.pdf".into()]))
                .is_err()
        );
        let job = manager.device_import(&id, None).unwrap();
        std::fs::write(mountinfo, "").unwrap();
        assert!(manager.run_once().await.unwrap());
        assert_eq!(manager.jobs.get(&job.id).unwrap().status, JobStatus::Failed);
        assert_eq!(
            manager.library_list(&BookQuery::default()).unwrap().total,
            0
        );
    }

    #[tokio::test]
    async fn device_index_jobs_report_measured_monotone_progress_and_honor_cancellation() {
        let (_directory, mut manager, events, _card, _mountinfo, id) = device_fixture(2);
        let job = manager.device_index(&id).unwrap();
        assert!(manager.run_once().await.unwrap());
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().status,
            JobStatus::Completed
        );
        let updates: Vec<_> = events
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, value)| name == "job:updated" && value["job"]["id"] == job.id)
            .map(|(_, value)| value["job"].clone())
            .collect();
        assert!(
            updates
                .iter()
                .any(|job| job["result"]["indexProgress"]["phase"] == "discovering")
        );
        assert!(
            updates
                .iter()
                .any(|job| job["result"]["indexProgress"]["phase"] == "reading")
        );
        let fractions: Vec<_> = updates
            .iter()
            .map(|job| job["progress"].as_f64().unwrap())
            .collect();
        assert!(fractions.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(
            fractions
                .iter()
                .any(|fraction| *fraction > 0.0 && *fraction < 0.99)
        );
        assert_eq!(fractions.last(), Some(&1.0));
        let original_jobs = manager.jobs.clone();
        manager.jobs = manager
            .jobs
            .clone()
            .with_event_callback(Arc::new(move |job| {
                if job.kind == JobKind::DeviceIndex
                    && job
                        .result
                        .as_ref()
                        .and_then(|result| result["indexProgress"]["processedBooks"].as_u64())
                        .is_some_and(|count| count > 0)
                {
                    original_jobs.cancel(&job.id).unwrap();
                }
            }));
        let cancelled = manager.device_index(&id).unwrap();
        assert!(manager.run_once().await.unwrap());
        assert_eq!(
            manager.jobs.get(&cancelled.id).unwrap().status,
            JobStatus::Cancelled
        );
        assert_eq!(manager.devices.inventory(&id).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn cancelled_device_index_worker_waits_for_staged_inventory_cleanup() {
        let (_directory, mut manager, _events, _card, _mountinfo, id) = device_fixture(2);
        manager.devices.index(&id).unwrap();
        let original_jobs = manager.jobs.clone();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let worker_barrier = barrier.clone();
        let (cancelled_sender, cancelled_receiver) = tokio::sync::oneshot::channel();
        let cancelled_sender = Mutex::new(Some(cancelled_sender));
        manager.jobs = manager
            .jobs
            .clone()
            .with_event_callback(Arc::new(move |job| {
                if job.kind == JobKind::DeviceIndex
                    && job
                        .result
                        .as_ref()
                        .and_then(|result| result["indexProgress"]["processedBooks"].as_u64())
                        .is_some_and(|count| count > 0)
                {
                    let sender = cancelled_sender.lock().unwrap().take();
                    if let Some(sender) = sender {
                        original_jobs.cancel(&job.id).unwrap();
                        sender.send(()).unwrap();
                        worker_barrier.wait();
                    }
                }
            }));
        let job = manager.device_index(&id).unwrap();
        let worker_manager = manager.clone();
        let mut worker = tokio::spawn(async move { worker_manager.run_once().await });
        cancelled_receiver.await.unwrap();
        let early_result = tokio::time::timeout(Duration::from_millis(150), &mut worker).await;
        let waited_for_cleanup = early_result.is_err();
        // Release even on failure so the old implementation cannot strand its blocking thread.
        barrier.wait();
        let finished = match early_result {
            Ok(result) => result,
            Err(_) => worker.await,
        };
        assert!(finished.unwrap().unwrap());
        assert!(
            waited_for_cleanup,
            "Cancelled worker returned before cleanup"
        );
        assert_eq!(
            manager.jobs.get(&job.id).unwrap().status,
            JobStatus::Cancelled
        );
        assert_eq!(manager.devices.inventory(&id).unwrap().len(), 2);
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
