//! Standalone Calibre smart-device TCP server, compatible with KOReader.
//!
//! The protocol has no atomic rename or abort while receiving binary content.
//! Transfers use fresh owned names, durable intentions, SHA-256 readback, and
//! finish the current binary frame before honouring user cancellation.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::time::{Instant, interval_at, timeout};
use uuid::Uuid;

use crate::book_repository::{BookRepository, StoredFile};
use crate::database::Database;
use crate::devices::{DeviceService, IndexedDeviceBook};
use crate::error::{AppError, Result};
use crate::models::{Book, BookFile, BookFormat, Device, DeviceTransport, FileVariant};
use crate::optimizer;
use crate::storage::{MAX_FILE_BYTES, Storage};
use crate::transfer::{TransferItem, TransferItemStatus, TransferReport};

const MAX_SERVERS: usize = 16;
const MAX_BOOKS: usize = 20_000;
const MAX_BATCH: usize = 200;
const MAX_JSON: usize = 2 * 1024 * 1024;
const MAX_METADATA_TOTAL: usize = 64 * 1024 * 1024;
const MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_INTENTIONS: usize = 1000;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(8);
const CONTENT_TIMEOUT: Duration = Duration::from_secs(300);
const METADATA_TIMEOUT: Duration = Duration::from_secs(1200);
const INDEX_TIMEOUT: Duration = Duration::from_secs(1800);
const KEEPALIVE: Duration = Duration::from_secs(10);
const SERVER_PREFIX: &str = "calibreServer:";
const INTENT_PREFIX: &str = "calibreIntent:";
const OWNED_DIRECTORY: &str = "LibraryManager";
const OK: u8 = 0;
const SET_DEVICE_INFO: u8 = 1;
const INFO: u8 = 3;
const FREE_SPACE: u8 = 5;
const BOOK_COUNT: u8 = 6;
const SEND_BOOK: u8 = 8;
const INITIALIZE: u8 = 9;
const NOOP: u8 = 12;
const DELETE_BOOK: u8 = 13;
const GET_FILE: u8 = 14;
const DISPLAY_MESSAGE: u8 = 17;
const SET_LIBRARY_INFO: u8 = 19;
// Compatibility declaration enables KOReader's explicit ERROR responses.
// It is a protocol feature level, not an installed Calibre application.
const CALIBRE_COMPATIBILITY_VERSION: [u32; 3] = [4, 18, 0];

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerConfig {
    id: String,
    address: String,
    label: String,
    reader_uuid: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Intention {
    id: String,
    device_id: String,
    reader_uuid: String,
    relative_path: String,
    sha256: String,
    #[serde(default)]
    size_bytes: Option<u64>,
}
struct Peer {
    stream: TcpStream,
    uuid: String,
    extensions: BTreeSet<String>,
    packet_size: usize,
    path_limits: BTreeMap<String, usize>,
}
struct Endpoint {
    device: Mutex<Device>,
    peer: AsyncMutex<Option<Peer>>,
    stopped: AtomicBool,
    wake: Notify,
}
#[derive(Clone)]
pub struct CalibreService {
    database: Database,
    _devices: DeviceService,
    storage: Storage,
    books: BookRepository,
    endpoints: Arc<Mutex<BTreeMap<String, Arc<Endpoint>>>>,
}

impl CalibreService {
    pub fn new(
        database: Database,
        devices: DeviceService,
        storage: Storage,
        books: BookRepository,
    ) -> Self {
        Self {
            database,
            _devices: devices,
            storage,
            books,
            endpoints: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Opens a listener; a reader is connected only after a valid handshake.
    pub async fn start(
        &self,
        address: &str,
        label: &str,
        password: Option<&str>,
    ) -> Result<Device> {
        let socket = validate_address(address)?;
        let label = bounded_text(label, 120)?;
        if label.trim().is_empty() {
            return Err(AppError::InvalidInput("Reader label is empty".into()));
        }
        let password = password
            .filter(|value| !value.is_empty())
            .map(|value| bounded_text(value, 256))
            .transpose()?;
        let configs = self.configurations()?;
        let previous = configs
            .iter()
            .find(|config| config.address == socket.to_string());
        let id = previous
            .map(|config| config.id.clone())
            .unwrap_or_else(|| format!("calibre-{}", Uuid::new_v4()));
        if let Some(endpoint) = self.endpoint_optional(&id)?
            && !endpoint.stopped.load(Ordering::Acquire)
        {
            return self.snapshot(&endpoint);
        }
        if previous.is_none() && configs.len() >= MAX_SERVERS {
            return Err(AppError::InvalidInput(
                "Calibre server limit reached".into(),
            ));
        }
        let listener = TcpListener::bind(socket)
            .await
            .map_err(|_| AppError::Network("Cannot open the Calibre listening address".into()))?;
        let actual = listener.local_addr()?;
        let config = ServerConfig {
            id: id.clone(),
            address: actual.to_string(),
            label,
            reader_uuid: previous.and_then(|config| config.reader_uuid.clone()),
        };
        self.save_configuration(&config)?;
        self.clear_presence(&id)?;
        let device = device_from_config(&config);
        let endpoint = Arc::new(Endpoint {
            device: Mutex::new(device.clone()),
            peer: AsyncMutex::new(None),
            stopped: AtomicBool::new(false),
            wake: Notify::new(),
        });
        self.lock_endpoints()?.insert(id, endpoint.clone());
        let service = self.clone();
        tokio::spawn(async move {
            service.listen(listener, endpoint, config, password).await;
        });
        Ok(device)
    }

    pub async fn scan(&self) -> Result<Vec<Device>> {
        let endpoints = self.lock_endpoints()?.clone();
        let mut result = Vec::new();
        for config in self.configurations()? {
            let mut device = if let Some(endpoint) = endpoints.get(&config.id) {
                self.snapshot(endpoint)?
            } else {
                device_from_config(&config)
            };
            if !device.connected {
                device.writable = false;
                device.book_count = 0;
                device.matched_book_count = 0;
            }
            result.push(device);
        }
        Ok(result)
    }
    pub async fn connected_ids(&self) -> Result<Vec<String>> {
        Ok(self
            .scan()
            .await?
            .into_iter()
            .filter(|device| device.connected)
            .map(|device| device.id)
            .collect())
    }
    pub fn warnings(&self, id: &str) -> Result<Vec<String>> {
        self.configuration(id)?;
        Ok(if self.intentions(id)?.is_empty() {
            Vec::new()
        } else {
            vec!["calibrePartialTransferPossible".into()]
        })
    }
    pub async fn disconnect(&self, id: &str) -> Result<()> {
        self.configuration(id)?;
        if let Some(endpoint) = self.endpoint_optional(id)? {
            endpoint.stopped.store(true, Ordering::Release);
            endpoint.wake.notify_one();
            self.mark_disconnected(&endpoint)?;
        }
        self.clear_presence(id)
    }

    pub async fn index(&self, id: &str) -> Result<Vec<IndexedDeviceBook>> {
        let endpoint = self.endpoint(id)?;
        let mut guard = endpoint.peer.lock().await;
        let peer = guard.as_mut().ok_or_else(disconnected)?;
        let result = timeout(INDEX_TIMEOUT, self.index_peer(id, peer, &endpoint))
            .await
            .unwrap_or_else(|_| {
                Err(AppError::Network(
                    "Calibre inventory exceeded its time budget".into(),
                ))
            });
        if result.is_err() {
            *guard = None;
            self.mark_disconnected(&endpoint)?;
            self.clear_presence(id)?;
        }
        result
    }

    pub async fn transfer(
        &self,
        id: &str,
        book_ids: &[String],
        profile_id: Option<&str>,
        cancellation: &AtomicBool,
    ) -> Result<TransferReport> {
        if book_ids.is_empty() || book_ids.len() > MAX_BATCH {
            return Err(AppError::InvalidInput(
                "Transfer batch must contain 1 to 200 books".into(),
            ));
        }
        if let Some(profile) = profile_id {
            optimizer::profile(profile)?;
        }
        let endpoint = self.endpoint(id)?;
        let mut guard = endpoint.peer.lock().await;
        let peer = guard.as_mut().ok_or_else(disconnected)?;
        let result = self
            .transfer_peer(id, book_ids, profile_id, cancellation, peer, &endpoint)
            .await;
        if result.is_err() && !matches!(result, Err(AppError::Cancelled)) {
            *guard = None;
            self.mark_disconnected(&endpoint)?;
            self.clear_presence(id)?;
        }
        result
    }

    async fn listen(
        &self,
        listener: TcpListener,
        endpoint: Arc<Endpoint>,
        mut config: ServerConfig,
        password: Option<String>,
    ) {
        let mut keepalive = interval_at(Instant::now() + KEEPALIVE, KEEPALIVE);
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if endpoint.stopped.load(Ordering::Acquire) {
                break;
            }
            tokio::select! {
                _ = endpoint.wake.notified() => break,
                _ = keepalive.tick() => {
                    if let Ok(mut guard) = endpoint.peer.try_lock() && let Some(peer) = guard.as_mut()
                        && call(peer, NOOP, json!({})).await.is_err() {
                        *guard = None;
                        let _ = self.mark_disconnected(&endpoint);
                        let _ = self.clear_presence(&config.id);
                    }
                }
                accepted = listener.accept() => {
                    let Ok((stream, address)) = accepted else { break; };
                    if !private_peer(address.ip()) { continue; }
                    let Ok(mut guard) = endpoint.peer.try_lock() else { continue; };
                    if guard.is_some() { continue; }
                    if let Ok(Ok((mut peer, free))) = timeout(Duration::from_secs(30), handshake(stream, password.as_deref(), &config)).await {
                        if endpoint.stopped.load(Ordering::Acquire) { break; }
                        // A different reader cannot inherit old presence or cleanup intentions.
                        if config.reader_uuid.as_deref() != Some(peer.uuid.as_str()) {
                            let _ = self.clear_presence(&config.id);
                        }
                        config.reader_uuid = Some(peer.uuid.clone());
                        if self.save_configuration(&config).is_err() { continue; }
                        if let Ok(mut device) = endpoint.device.lock() {
                            device.connected = true; device.writable = true; device.free_bytes = free;
                            device.last_seen_at = Utc::now().to_rfc3339();
                        }
                        // Reconnection cleanup also verifies the intended size and bytes; paths alone are insufficient.
                        if let Ok(metadata) = metadata_list(&mut peer).await {
                            match self.cleanup_intentions(&config.id, &mut peer, &metadata).await {
                                Ok(removed) => { if let Ok(mut device) = endpoint.device.lock() { device.book_count = metadata.len().saturating_sub(removed.len()) as u64; } }
                                Err(_) => { let _ = self.mark_disconnected(&endpoint); continue; }
                            }
                        } else { let _ = self.mark_disconnected(&endpoint); continue; }
                        *guard = Some(peer);
                    }
                }
            }
        }
        if let Ok(mut guard) = timeout(CONTROL_TIMEOUT, endpoint.peer.lock()).await {
            if let Some(peer) = guard.as_mut() {
                let _ = call(peer, NOOP, json!({"ejecting":true})).await;
                let _ = peer.stream.shutdown().await;
            }
            *guard = None;
        }
        let _ = self.mark_disconnected(&endpoint);
        let _ = self.clear_presence(&config.id);
    }

    async fn index_peer(
        &self,
        id: &str,
        peer: &mut Peer,
        endpoint: &Endpoint,
    ) -> Result<Vec<IndexedDeviceBook>> {
        check_stop(endpoint)?;
        let mut metadata = metadata_list(peer).await?;
        let removed = self.cleanup_intentions(id, peer, &metadata).await?;
        metadata.retain(|item| metadata_path(item).is_ok_and(|path| !removed.contains(&path)));
        let timestamp = Utc::now().to_rfc3339();
        let mut records = Vec::new();
        let mut total = 0_u64;
        let mut paths = BTreeSet::new();
        for item in metadata {
            check_stop(endpoint)?;
            let path = metadata_path(&item)?;
            if !paths.insert(path.clone()) {
                return Err(AppError::Network("Duplicate remote book path".into()));
            }
            let (hash, size) =
                readback_bounded(peer, &path, MAX_INDEX_BYTES.saturating_sub(total)).await?;
            total = total
                .checked_add(size)
                .ok_or_else(|| AppError::Unsupported("Remote index size overflow".into()))?;
            if total > MAX_INDEX_BYTES {
                return Err(AppError::Unsupported(
                    "Remote index exceeds the byte budget".into(),
                ));
            }
            let format = path_format(&path)?;
            let title: String = item
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(&path)
                .chars()
                .take(1000)
                .collect();
            let authors: Vec<String> = item
                .get("authors")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .take(32)
                        .map(|value| value.chars().take(200).collect())
                        .collect()
                })
                .unwrap_or_default();
            let matched = self.books.find_file_by_hash(&hash)?;
            records.push(IndexedDeviceBook {
                device_id: id.into(),
                relative_path: path,
                book_id: matched.map(|file| file.file.book_id),
                sha256: Some(hash),
                title,
                authors,
                format,
                size_bytes: size,
                last_seen_at: timestamp.clone(),
                warnings: Vec::new(),
            });
        }
        check_stop(endpoint)?;
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("DELETE FROM device_books WHERE device_id=?1", [id])?;
        for record in &records {
            insert_presence(&transaction, record)?;
        }
        transaction.commit()?;
        if let Ok(mut device) = endpoint.device.lock() {
            device.book_count = records.len() as u64;
            device.matched_book_count =
                records.iter().filter(|book| book.book_id.is_some()).count() as u64;
        }
        Ok(records)
    }

    async fn transfer_peer(
        &self,
        id: &str,
        book_ids: &[String],
        profile: Option<&str>,
        cancellation: &AtomicBool,
        peer: &mut Peer,
        endpoint: &Endpoint,
    ) -> Result<TransferReport> {
        let mut report = TransferReport {
            device_id: id.into(),
            ..Default::default()
        };
        let mut existing = metadata_list(peer).await?;
        let unique: BTreeSet<String> = book_ids.iter().cloned().collect();
        for (batch_index, book_id) in unique.iter().enumerate() {
            check_cancel(endpoint, cancellation)?;
            let storage = self.storage.clone();
            let books = self.books.clone();
            let owned_id = book_id.clone();
            let profile = profile.map(str::to_owned);
            let formats = peer.extensions.clone();
            let prepared = tokio::task::spawn_blocking(move || {
                prepare_book(&storage, &books, &owned_id, profile.as_deref(), &formats)
            })
            .await
            .map_err(|_| AppError::Conflict("Book preparation worker failed".into()))?;
            let prepared = match prepared {
                Ok(book) => book,
                Err(error) => {
                    report.failed += 1;
                    report.items.push(TransferItem {
                        book_id: book_id.clone(),
                        file_id: String::new(),
                        relative_path: String::new(),
                        size_bytes: 0,
                        status: TransferItemStatus::Failed,
                        error_code: Some(error.code().into()),
                    });
                    continue;
                }
            };
            check_cancel(endpoint, cancellation)?;
            // Cached claims are revalidated by reading bytes before skipping a copy.
            if let Some(path) = self.cached_path(id, &prepared.file.file.sha256)?
                && existing
                    .iter()
                    .any(|item| metadata_path(item).ok().as_deref() == Some(path.as_str()))
            {
                let (hash, size) = readback(peer, &path).await?;
                if hash == prepared.file.file.sha256 && size == prepared.bytes.len() as u64 {
                    report.skipped += 1;
                    report.items.push(transfer_item(
                        &prepared,
                        path,
                        TransferItemStatus::AlreadyPresent,
                    ));
                    continue;
                }
            }
            let path = format!(
                "{OWNED_DIRECTORY}/{}-{}.{}",
                Uuid::new_v4(),
                prepared.file.file.sha256,
                prepared.file.file.format.as_str()
            );
            validate_path(&path)?;
            if path.len()
                > peer
                    .path_limits
                    .get(prepared.file.file.format.as_str())
                    .copied()
                    .unwrap_or(1024)
            {
                return Err(AppError::Unsupported(
                    "Reader path length limit is too small for a unique owned transfer".into(),
                ));
            }
            if existing
                .iter()
                .any(|item| metadata_path(item).ok().as_deref() == Some(path.as_str()))
            {
                return Err(AppError::Conflict(
                    "Fresh remote transfer path already exists".into(),
                ));
            }
            let intention = Intention {
                id: Uuid::new_v4().to_string(),
                device_id: id.into(),
                reader_uuid: peer.uuid.clone(),
                relative_path: path.clone(),
                sha256: prepared.file.file.sha256.clone(),
                size_bytes: Some(prepared.bytes.len() as u64),
            };
            self.save_intention(&intention)?;
            let metadata = book_metadata(&prepared.book, &path, prepared.bytes.len() as u64);
            let started = Instant::now();
            let reply = call(peer, SEND_BOOK, json!({"lpath":path,"length":prepared.bytes.len(),"metadata":metadata,
                "thisBook":batch_index,"totalBooks":unique.len(),"willStreamBooks":true,"willStreamBinary":true,"wantsSendOkToSendbook":true,"canSupportLpathChanges":false})).await;
            if let Err(error) = reply {
                return Err(interrupted(error));
            }
            // Raw content must reach its declared length before another opcode.
            for chunk in prepared.bytes.chunks(peer.packet_size) {
                if started.elapsed() > CONTENT_TIMEOUT {
                    return Err(interrupted(AppError::Network(
                        "Calibre binary transfer timed out".into(),
                    )));
                }
                timeout(CONTROL_TIMEOUT, peer.stream.write_all(chunk))
                    .await
                    .map_err(|_| interrupted(disconnected()))?
                    .map_err(|error| interrupted(error.into()))?;
            }
            timeout(CONTROL_TIMEOUT, peer.stream.flush())
                .await
                .map_err(|_| interrupted(disconnected()))??;
            if cancellation.load(Ordering::Acquire) || endpoint.stopped.load(Ordering::Acquire) {
                // This fresh owned path is now registered by KOReader and can be deleted.
                delete_owned(peer, &intention.relative_path)
                    .await
                    .map_err(interrupted)?;
                self.remove_intention(&intention.id)?;
                return Err(AppError::Cancelled);
            }
            let verification = readback(peer, &path).await;
            let (hash, size) = match verification {
                Ok(result) => result,
                Err(error) => return Err(interrupted(error)),
            };
            if hash != intention.sha256 || size != prepared.bytes.len() as u64 {
                delete_owned(peer, &path).await.map_err(interrupted)?;
                self.remove_intention(&intention.id)?;
                return Err(AppError::Conflict(
                    "Calibre transfer failed SHA-256 readback verification".into(),
                ));
            }
            if check_cancel(endpoint, cancellation).is_err() {
                delete_owned(peer, &path).await.map_err(interrupted)?;
                self.remove_intention(&intention.id)?;
                return Err(AppError::Cancelled);
            }
            let record = IndexedDeviceBook {
                device_id: id.into(),
                relative_path: path.clone(),
                book_id: Some(prepared.book.id.clone()),
                sha256: Some(hash),
                title: prepared.book.title.clone(),
                authors: prepared.book.authors.clone(),
                format: prepared.file.file.format,
                size_bytes: size,
                last_seen_at: Utc::now().to_rfc3339(),
                warnings: Vec::new(),
            };
            insert_presence(&*self.database.connect()?, &record)?;
            self.remove_intention(&intention.id)?;
            existing.push(metadata);
            report.warnings.extend(prepared.warnings.iter().cloned());
            report.copied += 1;
            report
                .items
                .push(transfer_item(&prepared, path, TransferItemStatus::Copied));
        }
        report.warnings.extend(self.warnings(id)?);
        report.warnings.sort();
        report.warnings.dedup();
        if let Ok(mut device) = endpoint.device.lock() {
            device.book_count = existing.len() as u64;
            device.matched_book_count += report.copied;
        }
        Ok(report)
    }

    async fn cleanup_intentions(
        &self,
        id: &str,
        peer: &mut Peer,
        metadata: &[Value],
    ) -> Result<BTreeSet<String>> {
        let mut removed = BTreeSet::new();
        for intention in self.intentions(id)? {
            if intention.reader_uuid != peer.uuid || !owned_path(&intention.relative_path) {
                continue;
            }
            if metadata.iter().any(|book| {
                metadata_path(book).ok().as_deref() == Some(intention.relative_path.as_str())
            }) {
                // Missing size in an older journal cannot authorize deleting a remote file.
                let Some(expected_size) = intention.size_bytes else {
                    continue;
                };
                let (hash, size) = readback(peer, &intention.relative_path).await?;
                if hash != intention.sha256 || size != expected_size {
                    // A partial or user-modified file stays intact and its intention warns on reconnect.
                    continue;
                }
                delete_owned(peer, &intention.relative_path).await?;
                self.database.connect()?.execute(
                    "DELETE FROM device_books WHERE device_id=?1 AND relative_path=?2",
                    params![id, intention.relative_path],
                )?;
                self.remove_intention(&intention.id)?;
                removed.insert(intention.relative_path);
            }
        }
        Ok(removed)
    }
    fn lock_endpoints(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, Arc<Endpoint>>>> {
        self.endpoints
            .lock()
            .map_err(|_| AppError::Conflict("Calibre session state is unavailable".into()))
    }
    fn endpoint_optional(&self, id: &str) -> Result<Option<Arc<Endpoint>>> {
        Ok(self.lock_endpoints()?.get(id).cloned())
    }
    fn endpoint(&self, id: &str) -> Result<Arc<Endpoint>> {
        self.endpoint_optional(id)?
            .filter(|endpoint| !endpoint.stopped.load(Ordering::Acquire))
            .ok_or_else(disconnected)
    }
    fn snapshot(&self, endpoint: &Endpoint) -> Result<Device> {
        endpoint
            .device
            .lock()
            .map(|device| device.clone())
            .map_err(|_| AppError::Conflict("Reader state is unavailable".into()))
    }
    fn mark_disconnected(&self, endpoint: &Endpoint) -> Result<()> {
        let mut device = endpoint
            .device
            .lock()
            .map_err(|_| AppError::Conflict("Reader state is unavailable".into()))?;
        device.connected = false;
        device.writable = false;
        device.book_count = 0;
        device.matched_book_count = 0;
        Ok(())
    }
    fn configurations(&self) -> Result<Vec<ServerConfig>> {
        let connection = self.database.connect()?;
        let mut query = connection.prepare("SELECT value_json FROM settings WHERE key LIKE 'calibreServer:%' ORDER BY key LIMIT 17")?;
        let mut configs = Vec::new();
        for value in query.query_map([], |row| row.get::<_, String>(0))? {
            configs.push(serde_json::from_str::<ServerConfig>(&value?)?);
        }
        if configs.len() > MAX_SERVERS {
            return Err(AppError::InvalidInput(
                "Calibre server limit exceeded".into(),
            ));
        }
        Ok(configs)
    }
    fn configuration(&self, id: &str) -> Result<ServerConfig> {
        self.configurations()?
            .into_iter()
            .find(|config| config.id == id)
            .ok_or_else(|| AppError::NotFound("Calibre server is unknown".into()))
    }
    fn save_configuration(&self, config: &ServerConfig) -> Result<()> {
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("INSERT INTO settings(key,value_json) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json", params![format!("{SERVER_PREFIX}{}",config.id),serde_json::to_string(config)?])?;
        transaction.execute("INSERT INTO devices(id,label,transport,profile,mount_identity,last_seen_at) VALUES(?1,?2,'calibreWireless','generic',?3,?4) ON CONFLICT(id) DO UPDATE SET label=excluded.label,mount_identity=excluded.mount_identity,last_seen_at=excluded.last_seen_at",
            params![config.id,config.label,config.reader_uuid,Utc::now().to_rfc3339()])?;
        transaction.commit()?;
        Ok(())
    }
    fn clear_presence(&self, id: &str) -> Result<()> {
        self.database
            .connect()?
            .execute("DELETE FROM device_books WHERE device_id=?1", [id])?;
        Ok(())
    }
    fn cached_path(&self, id: &str, hash: &str) -> Result<Option<String>> {
        Ok(self
            .database
            .connect()?
            .query_row(
                "SELECT relative_path FROM device_books WHERE device_id=?1 AND sha256=?2 LIMIT 1",
                params![id, hash],
                |row| row.get(0),
            )
            .optional()?)
    }
    fn intentions(&self, id: &str) -> Result<Vec<Intention>> {
        let connection = self.database.connect()?;
        let mut query = connection.prepare(
            "SELECT value_json FROM settings WHERE key LIKE 'calibreIntent:%' LIMIT 1001",
        )?;
        let mut result = Vec::new();
        let mut total = 0;
        for row in query.query_map([], |row| row.get::<_, String>(0))? {
            total += 1;
            if total > MAX_INTENTIONS {
                return Err(AppError::Unsupported(
                    "Calibre intention journal is full".into(),
                ));
            }
            let intention: Intention = serde_json::from_str(&row?)?;
            if intention.device_id == id {
                result.push(intention);
            }
        }
        Ok(result)
    }
    fn save_intention(&self, intention: &Intention) -> Result<()> {
        if self.intentions(&intention.device_id)?.len() >= MAX_INTENTIONS {
            return Err(AppError::Unsupported(
                "Calibre intention journal is full".into(),
            ));
        }
        self.database.connect()?.execute(
            "INSERT INTO settings(key,value_json) VALUES(?1,?2)",
            params![
                format!("{INTENT_PREFIX}{}", intention.id),
                serde_json::to_string(intention)?
            ],
        )?;
        Ok(())
    }
    fn remove_intention(&self, id: &str) -> Result<()> {
        self.database.connect()?.execute(
            "DELETE FROM settings WHERE key=?1",
            [format!("{INTENT_PREFIX}{id}")],
        )?;
        Ok(())
    }
}

async fn handshake(
    mut stream: TcpStream,
    password: Option<&str>,
    config: &ServerConfig,
) -> Result<(Peer, Option<u64>)> {
    let challenge = if password.is_some() {
        Uuid::new_v4().to_string()
    } else {
        String::new()
    };
    write_frame(&mut stream, INITIALIZE, json!({"serverProtocolVersion":1,"validExtensions":["epub","txt","mobi","azw3","fb2","pdf","cbz","html"],"passwordChallenge":challenge,
        "currentLibraryName":"Library Manager","currentLibraryUUID":config.id.strip_prefix("calibre-").unwrap_or(&config.id),"pubdateFormat":"yyyy-MM-dd","timestampFormat":"yyyy-MM-dd","lastModifiedFormat":"yyyy-MM-dd",
        "calibre_version":CALIBRE_COMPATIBILITY_VERSION,"canSupportUpdateBooks":false,"canSupportLpathChanges":false})).await?;
    let (_, info) = read_ok(&mut stream).await?;
    for capability in [
        "versionOK",
        "canReceiveBookBinary",
        "canSendOkToSendbook",
        "canStreamBooks",
        "canStreamMetadata",
        "canUseCachedMetadata",
        "cacheUsesLpaths",
        "canDeleteMultipleBooks",
    ] {
        if info.get(capability).and_then(Value::as_bool) != Some(true) {
            return Err(AppError::Unsupported(
                "Reader lacks required Calibre protocol capabilities".into(),
            ));
        }
    }
    if let Some(password) = password {
        let expected = hex(&Sha1::digest(format!("{password}{challenge}").as_bytes()));
        let returned = info
            .get("passwordHash")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !constant_time_equal(expected.as_bytes(), returned.as_bytes()) {
            let _ = write_frame(&mut stream, DISPLAY_MESSAGE, json!({"messageKind":1,"currentLibraryName":"Library Manager","currentLibraryUUID":config.id.strip_prefix("calibre-").unwrap_or(&config.id)})).await;
            return Err(AppError::Network(
                "Calibre reader authentication failed".into(),
            ));
        }
    }
    let packet_size = info
        .get("maxBookContentPacketLen")
        .and_then(Value::as_u64)
        .filter(|length| (1..=1024 * 1024).contains(length))
        .ok_or_else(|| AppError::Network("Invalid Calibre content packet size".into()))?
        .min(64 * 1024) as usize;
    let extensions: BTreeSet<String> = info
        .get("acceptedExtensions")
        .and_then(Value::as_array)
        .filter(|values| !values.is_empty() && values.len() <= 64)
        .ok_or_else(|| AppError::Network("Invalid Calibre accepted extensions".into()))?
        .iter()
        .filter_map(Value::as_str)
        .filter(|extension| BookFormat::from_str(extension).is_ok())
        .map(str::to_owned)
        .collect();
    if extensions.is_empty() {
        return Err(AppError::Unsupported(
            "Reader accepts no supported ebook formats".into(),
        ));
    }
    let can_accept_library_info =
        info.get("canAcceptLibraryInfo").and_then(Value::as_bool) == Some(true);
    let mut path_limits = BTreeMap::new();
    if let Some(limits) = info.get("extensionPathLengths").and_then(Value::as_object) {
        if limits.len() > 64 {
            return Err(AppError::Network("Invalid reader path limits".into()));
        }
        for (extension, value) in limits {
            let length = value
                .as_u64()
                .filter(|length| (1..=1024).contains(length))
                .ok_or_else(|| AppError::Network("Invalid reader path limit".into()))?;
            path_limits.insert(extension.clone(), length as usize);
        }
    }
    let mut peer = Peer {
        stream,
        uuid: String::new(),
        extensions,
        packet_size,
        path_limits,
    };
    let info = call(&mut peer, INFO, json!({})).await?;
    let drive = info
        .get("device_info")
        .and_then(Value::as_object)
        .ok_or_else(|| AppError::Network("Missing reader identity".into()))?;
    let uuid = match drive.get("device_store_uuid").and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Uuid::parse_str(value)
            .map_err(|_| AppError::Network("Invalid reader UUID".into()))?
            .to_string(),
        _ => Uuid::new_v4().to_string(),
    };
    call(
        &mut peer,
        SET_DEVICE_INFO,
        json!({"device_store_uuid":uuid,"device_name":config.label,"last_library_uuid":config.id}),
    )
    .await?;
    peer.uuid = uuid;
    let free = call(&mut peer, FREE_SPACE, json!({}))
        .await?
        .get("free_space_on_device")
        .and_then(Value::as_u64);
    if can_accept_library_info {
        call(
            &mut peer,
            SET_LIBRARY_INFO,
            json!({"libraryName":"Library Manager","libraryUuid":config.id.strip_prefix("calibre-").unwrap_or(&config.id),"fieldMetadata":{}}),
        )
        .await?;
    }
    Ok((peer, free))
}

async fn metadata_list(peer: &mut Peer) -> Result<Vec<Value>> {
    timeout(METADATA_TIMEOUT, metadata_list_inner(peer))
        .await
        .map_err(|_| AppError::Network("Calibre metadata exceeded its time budget".into()))?
}
async fn metadata_list_inner(peer: &mut Peer) -> Result<Vec<Value>> {
    let info = call(
        peer,
        BOOK_COUNT,
        json!({"canStream":true,"canScan":true,"willUseCachedMetadata":true,"supportsSync":false}),
    )
    .await?;
    let count = info
        .get("count")
        .and_then(Value::as_u64)
        .filter(|count| *count <= MAX_BOOKS as u64)
        .ok_or_else(|| AppError::Unsupported("Remote book count exceeds limits".into()))?
        as usize;
    if info.get("willStream").and_then(Value::as_bool) != Some(true) {
        return Err(AppError::Unsupported(
            "Reader cannot stream its metadata".into(),
        ));
    }
    let mut ids = Vec::with_capacity(count);
    let mut bytes = 0_usize;
    for _ in 0..count {
        let (_, entry) = read_ok(&mut peer.stream).await?;
        bytes += serde_json::to_vec(&entry)?.len();
        if bytes > MAX_METADATA_TOTAL {
            return Err(AppError::Unsupported(
                "Remote metadata exceeds limits".into(),
            ));
        }
        let key = entry
            .get("priKey")
            .and_then(Value::as_u64)
            .filter(|key| *key <= MAX_BOOKS as u64)
            .ok_or_else(|| AppError::Network("Invalid remote metadata key".into()))?;
        ids.push(key);
    }
    write_frame(&mut peer.stream, NOOP, json!({"count":count})).await?;
    let mut metadata = Vec::with_capacity(count);
    for key in ids {
        write_frame(&mut peer.stream, NOOP, json!({"priKey":key})).await?;
        let (_, entry) = read_ok(&mut peer.stream).await?;
        bytes += serde_json::to_vec(&entry)?.len();
        if bytes > MAX_METADATA_TOTAL {
            return Err(AppError::Unsupported(
                "Remote metadata exceeds limits".into(),
            ));
        }
        metadata_path(&entry)?;
        metadata.push(entry);
    }
    Ok(metadata)
}
async fn readback(peer: &mut Peer, path: &str) -> Result<(String, u64)> {
    readback_bounded(peer, path, MAX_FILE_BYTES).await
}
async fn readback_bounded(peer: &mut Peer, path: &str, budget: u64) -> Result<(String, u64)> {
    validate_path(path)?;
    let header = call(
        peer,
        GET_FILE,
        json!({"lpath":path,"position":0,"canStream":true,"canStreamBinary":true}),
    )
    .await?;
    let length = header
        .get("fileLength")
        .and_then(Value::as_u64)
        .filter(|length| *length > 0 && *length <= MAX_FILE_BYTES && *length <= budget)
        .ok_or_else(|| AppError::Unsupported("Remote file size exceeds limits".into()))?;
    timeout(CONTENT_TIMEOUT, async {
        let mut remaining = length;
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut hash = Sha256::new();
        while remaining > 0 {
            let wanted = remaining.min(buffer.len() as u64) as usize;
            timeout(
                CONTROL_TIMEOUT,
                peer.stream.read_exact(&mut buffer[..wanted]),
            )
            .await
            .map_err(|_| disconnected())??;
            hash.update(&buffer[..wanted]);
            remaining -= wanted as u64;
        }
        Ok::<_, AppError>((hex(&hash.finalize()), length))
    })
    .await
    .map_err(|_| AppError::Network("Remote file readback timed out".into()))?
}
async fn delete_owned(peer: &mut Peer, path: &str) -> Result<()> {
    if !owned_path(path) {
        return Err(AppError::InvalidInput("Cleanup path is not owned".into()));
    }
    call(peer, DELETE_BOOK, json!({"lpaths":[path]})).await?;
    read_ok(&mut peer.stream).await?;
    // KOReader emits its acknowledgement after removal; metadata must no longer contain it.
    if metadata_list(peer)
        .await?
        .iter()
        .any(|item| metadata_path(item).ok().as_deref() == Some(path))
    {
        return Err(AppError::Conflict(
            "Reader did not remove the owned transfer".into(),
        ));
    }
    Ok(())
}
async fn call(peer: &mut Peer, opcode: u8, args: Value) -> Result<Value> {
    write_frame(&mut peer.stream, opcode, args).await?;
    Ok(read_ok(&mut peer.stream).await?.1)
}
async fn read_ok<R: AsyncRead + Unpin>(reader: &mut R) -> Result<(u8, Value)> {
    let response = read_frame(reader).await?;
    if response.0 != OK {
        return Err(AppError::Network(
            "Calibre reader refused the operation".into(),
        ));
    }
    Ok(response)
}
async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, opcode: u8, args: Value) -> Result<()> {
    let bytes = serde_json::to_vec(&json!([opcode, args]))?;
    if bytes.len() > MAX_JSON {
        return Err(AppError::InvalidInput(
            "Calibre JSON frame exceeds limits".into(),
        ));
    }
    timeout(CONTROL_TIMEOUT, async {
        writer.write_all(bytes.len().to_string().as_bytes()).await?;
        writer.write_all(&bytes).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| disconnected())??;
    Ok(())
}
async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<(u8, Value)> {
    timeout(CONTROL_TIMEOUT, async {
        let mut prefix = String::new();
        loop {
            let byte = reader.read_u8().await?;
            if byte == b'[' {
                break;
            }
            if !byte.is_ascii_digit() || prefix.len() >= 7 {
                return Err(AppError::Network("Invalid Calibre frame length".into()));
            }
            prefix.push(byte as char);
        }
        if prefix.is_empty() || prefix.starts_with('0') {
            return Err(AppError::Network("Invalid Calibre frame prefix".into()));
        }
        let length = prefix.parse::<usize>().map_err(|_| disconnected())?;
        if !(4..=MAX_JSON).contains(&length) {
            return Err(AppError::Unsupported(
                "Calibre JSON frame exceeds limits".into(),
            ));
        }
        let mut data = vec![0; length];
        data[0] = b'[';
        reader.read_exact(&mut data[1..]).await?;
        let value: Value = serde_json::from_slice(&data)?;
        let array = value
            .as_array()
            .filter(|array| array.len() == 2)
            .ok_or_else(|| AppError::Network("Invalid Calibre frame shape".into()))?;
        let opcode = array[0]
            .as_u64()
            .filter(|opcode| *opcode <= 22)
            .ok_or_else(|| AppError::Network("Invalid Calibre opcode".into()))?
            as u8;
        if !array[1].is_object() {
            return Err(AppError::Network("Invalid Calibre arguments".into()));
        }
        Ok((opcode, array[1].clone()))
    })
    .await
    .map_err(|_| disconnected())?
}

struct PreparedBook {
    book: Book,
    file: StoredFile,
    bytes: Vec<u8>,
    warnings: Vec<String>,
}
fn prepare_book(
    storage: &Storage,
    books: &BookRepository,
    id: &str,
    profile_id: Option<&str>,
    formats: &BTreeSet<String>,
) -> Result<PreparedBook> {
    Uuid::parse_str(id).map_err(|_| AppError::InvalidInput("Invalid book ID".into()))?;
    let book = books.get(id, &[])?;
    let files = books.files(id)?;
    let mut compatible: Vec<&StoredFile> = files
        .iter()
        .filter(|file| formats.contains(file.file.format.as_str()))
        .collect();
    compatible.sort_by_key(|file| match file.file.variant {
        FileVariant::Normalized => 0,
        FileVariant::Original => 1,
        FileVariant::Converted => 2,
        FileVariant::Optimized => 3,
    });
    let mut file = compatible.first().copied().cloned().ok_or_else(|| {
        AppError::Unsupported("Convert this book to a format accepted by the reader".into())
    })?;
    let mut warnings = Vec::new();
    if let Some(profile_id) = profile_id {
        if file.file.format != BookFormat::Epub {
            return Err(AppError::Unsupported(
                "Optimization requires an EPUB".into(),
            ));
        }
        let relative = format!(
            "variants/{id}/calibre-{profile_id}-{}.epub",
            file.file.sha256
        );
        if let Some(cached) = files.iter().find(|existing| {
            existing.relative_path == relative
                && existing.file.profile.as_deref() == Some(profile_id)
        }) {
            file = cached.clone();
        } else {
            let work = WorkDirectory::new(storage)?;
            let source = storage.resolve(&file.relative_path)?;
            if Storage::hash_file(&source)? != file.file.sha256 {
                return Err(AppError::Conflict("Managed ebook hash changed".into()));
            }
            let output = work.path.join("optimized.epub");
            let report = optimizer::optimize(
                &source,
                &output,
                &optimizer::profile(profile_id)?,
                id,
                &file.file.id,
            )?;
            let artifact = storage.publish_file(&relative, &output)?;
            file = StoredFile {
                file: BookFile {
                    id: Uuid::new_v4().to_string(),
                    book_id: id.into(),
                    format: BookFormat::Epub,
                    variant: FileVariant::Optimized,
                    profile: Some(profile_id.into()),
                    size_bytes: artifact.size_bytes,
                    sha256: artifact.sha256,
                    created_at: Utc::now().to_rfc3339(),
                },
                relative_path: artifact.relative_path,
            };
            books.add_file(file.clone())?;
            warnings = report.warnings;
        }
    }
    let bytes = storage.read(&file.relative_path)?;
    if bytes.is_empty()
        || hex(&Sha256::digest(&bytes)) != file.file.sha256
        || bytes.len() as u64 != file.file.size_bytes
    {
        return Err(AppError::Conflict("Managed transfer bytes changed".into()));
    }
    Ok(PreparedBook {
        book,
        file,
        bytes,
        warnings,
    })
}
struct WorkDirectory {
    path: std::path::PathBuf,
}
impl WorkDirectory {
    fn new(storage: &Storage) -> Result<Self> {
        let relative = format!("cache/calibre/{}/reservation", Uuid::new_v4());
        storage.write_new(&relative, &[])?;
        Ok(Self {
            path: storage
                .resolve(&relative)?
                .parent()
                .ok_or_else(|| AppError::InvalidInput("Missing work directory".into()))?
                .to_path_buf(),
        })
    }
}
impl Drop for WorkDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.path.join("optimized.epub"));
        let _ = std::fs::remove_file(self.path.join("reservation"));
        let _ = std::fs::remove_dir(&self.path);
    }
}
fn book_metadata(book: &Book, path: &str, size: u64) -> Value {
    json!({"uuid":book.id,"lpath":path,"title":book.title,"authors":book.authors,"author_sort":book.author_sort,"series":book.series,"series_index":book.series_index,"tags":book.tags,"languages":[book.language],"comments":book.description,"publisher":book.publisher,"isbn":book.isbn,"size":size,"last_modified":Utc::now().to_rfc3339(),"timestamp":book.added_at,"pubdate":book.published,"identifiers":{}})
}
fn transfer_item(book: &PreparedBook, path: String, status: TransferItemStatus) -> TransferItem {
    TransferItem {
        book_id: book.book.id.clone(),
        file_id: book.file.file.id.clone(),
        relative_path: path,
        size_bytes: book.bytes.len() as u64,
        status,
        error_code: None,
    }
}
fn insert_presence(connection: &rusqlite::Connection, record: &IndexedDeviceBook) -> Result<()> {
    connection.execute("INSERT INTO device_books(device_id,relative_path,book_id,sha256,title,authors_json,format,size_bytes,last_seen_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(device_id,relative_path) DO UPDATE SET book_id=excluded.book_id,sha256=excluded.sha256,title=excluded.title,authors_json=excluded.authors_json,format=excluded.format,size_bytes=excluded.size_bytes,last_seen_at=excluded.last_seen_at",
        params![record.device_id,record.relative_path,record.book_id,record.sha256,record.title,serde_json::to_string(&record.authors)?,record.format.as_str(),record.size_bytes as i64,record.last_seen_at])?;
    Ok(())
}
fn device_from_config(config: &ServerConfig) -> Device {
    Device {
        id: config.id.clone(),
        label: config.label.clone(),
        transport: DeviceTransport::CalibreWireless,
        connected: false,
        writable: false,
        profile: "generic".into(),
        mount_path: None,
        address: Some(config.address.clone()),
        total_bytes: None,
        free_bytes: None,
        book_count: 0,
        matched_book_count: 0,
        last_seen_at: Utc::now().to_rfc3339(),
    }
}
fn check_stop(endpoint: &Endpoint) -> Result<()> {
    if endpoint.stopped.load(Ordering::Acquire) {
        Err(AppError::Cancelled)
    } else {
        Ok(())
    }
}
fn check_cancel(endpoint: &Endpoint, flag: &AtomicBool) -> Result<()> {
    check_stop(endpoint)?;
    if flag.load(Ordering::Acquire) {
        Err(AppError::Cancelled)
    } else {
        Ok(())
    }
}
fn disconnected() -> AppError {
    AppError::Network("Calibre reader is disconnected or timed out".into())
}
fn interrupted(_error: AppError) -> AppError {
    AppError::Network("calibrePartialTransferPossible: an owned file may be incomplete; reconnect this reader to check cleanup".into())
}
fn bounded_text(value: &str, limit: usize) -> Result<String> {
    if value.len() > limit || value.chars().any(char::is_control) {
        Err(AppError::InvalidInput(
            "Text exceeds limits or contains controls".into(),
        ))
    } else {
        Ok(value.to_owned())
    }
}
fn validate_address(value: &str) -> Result<SocketAddr> {
    let address = SocketAddr::from_str(value).map_err(|_| {
        AppError::InvalidInput("Use a numeric listening IP address and port".into())
    })?;
    if !address.ip().is_unspecified() && !private_peer(address.ip()) {
        return Err(AppError::InvalidInput(
            "Listening address must be local or private".into(),
        ));
    }
    Ok(address)
}
fn private_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|ip| private_peer(IpAddr::V4(ip)))
        }
    }
}
fn validate_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 1024
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(AppError::InvalidInput("Unsafe remote ebook path".into()));
    }
    Ok(())
}
fn metadata_path(value: &Value) -> Result<String> {
    let path = value
        .get("lpath")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Network("Missing remote ebook path".into()))?;
    validate_path(path)?;
    Ok(path.into())
}
fn path_format(path: &str) -> Result<BookFormat> {
    BookFormat::from_str(path.rsplit('.').next().unwrap_or(""))
        .map_err(|_| AppError::Unsupported("Unknown remote ebook format".into()))
}
fn owned_path(path: &str) -> bool {
    validate_path(path).is_ok()
        && path
            .strip_prefix(&format!("{OWNED_DIRECTORY}/"))
            .and_then(|name| name.get(..36))
            .is_some_and(|id| Uuid::parse_str(id).is_ok())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn constant_time_equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_handle_partial_reads_and_reject_unsafe_lengths() {
        let (mut writer, mut reader) = tokio::io::duplex(128);
        let sender = tokio::spawn(async move {
            for byte in b"15[0,{\"count\":2}]" {
                writer.write_all(&[*byte]).await.unwrap();
            }
        });
        let (_, value) = read_frame(&mut reader).await.unwrap();
        assert_eq!(value["count"], 2);
        sender.await.unwrap();
        for input in ["0[0,{}]", "9999999[", "-1[", "6[0,[]]", "6[0,{}"] {
            let (mut writer, mut reader) = tokio::io::duplex(64);
            writer.write_all(input.as_bytes()).await.unwrap();
            drop(writer);
            assert!(read_frame(&mut reader).await.is_err());
        }
    }
    #[test]
    fn paths_peers_and_legacy_authentication_are_bounded() {
        for path in [
            "../secret.epub",
            "/root/a.epub",
            "a\\b.epub",
            "a//b.epub",
            "a/./b.epub",
            "C:a.epub",
            "a\0.epub",
        ] {
            assert!(validate_path(path).is_err());
        }
        assert!(validate_path("Auteurs/Série/Livre.epub").is_ok());
        assert!(!owned_path("LibraryManager/existing.epub"));
        assert!(owned_path(&format!(
            "LibraryManager/{}-hash.epub",
            Uuid::new_v4()
        )));
        assert!(private_peer("127.0.0.1".parse().unwrap()));
        assert!(!private_peer("8.8.8.8".parse().unwrap()));
        assert!(constant_time_equal(b"same", b"same"));
        assert!(!constant_time_equal(b"same", b"evil"));
        assert!(!constant_time_equal(b"same", b"same!"));
    }
    #[tokio::test]
    async fn metadata_stream_and_binary_readback_match_the_wire_protocol() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let device = tokio::spawn(async move {
            let mut socket = TcpStream::connect(address).await.unwrap();
            assert_eq!(read_frame(&mut socket).await.unwrap().0, BOOK_COUNT);
            write_frame(&mut socket, OK, json!({"count":1,"willStream":true}))
                .await
                .unwrap();
            write_frame(&mut socket, OK, json!({"priKey":1,"lpath":"sample.txt"}))
                .await
                .unwrap();
            assert_eq!(read_frame(&mut socket).await.unwrap().1["count"], 1);
            assert_eq!(read_frame(&mut socket).await.unwrap().1["priKey"], 1);
            write_frame(
                &mut socket,
                OK,
                json!({"lpath":"sample.txt","title":"Synthetic sample","authors":["Test"]}),
            )
            .await
            .unwrap();
            assert_eq!(read_frame(&mut socket).await.unwrap().0, GET_FILE);
            write_frame(&mut socket, OK, json!({"fileLength":9}))
                .await
                .unwrap();
            socket.write_all(b"test book").await.unwrap();
        });
        let (socket, _) = listener.accept().await.unwrap();
        let mut peer = Peer {
            stream: socket,
            uuid: Uuid::new_v4().to_string(),
            extensions: BTreeSet::from(["txt".into()]),
            packet_size: 4096,
            path_limits: BTreeMap::new(),
        };
        let books = metadata_list(&mut peer).await.unwrap();
        assert_eq!(books[0]["title"], "Synthetic sample");
        let (hash, length) = readback(&mut peer, "sample.txt").await.unwrap();
        assert_eq!(length, 9);
        assert_eq!(hash, hex(&Sha256::digest(b"test book")));
        device.await.unwrap();
    }
    #[tokio::test]
    async fn reconnect_cleanup_requires_exact_intended_bytes_and_size() {
        let original = b"Original synthetic book";
        let modified = b"Replaced synthetic book";
        let cases: Vec<(&[u8], Option<u64>, bool)> = vec![
            (modified, Some(original.len() as u64), false),
            (&original[..5], Some(original.len() as u64), false),
            (original, Some(original.len() as u64 + 1), false),
            (original, None, false),
            (original, Some(original.len() as u64), true),
        ];
        for (bytes, expected_size, should_delete) in cases {
            let directory = tempfile::tempdir().unwrap();
            let database = Database::new(&directory.path().join("data")).unwrap();
            let storage = Storage::new(&directory.path().join("storage")).unwrap();
            let service = CalibreService::new(
                database.clone(),
                DeviceService::new(database.clone()),
                storage,
                BookRepository::new(database.clone()),
            );
            let id = format!("calibre-{}", Uuid::new_v4());
            let uuid = Uuid::new_v4().to_string();
            let path = format!("LibraryManager/{}-hash.txt", Uuid::new_v4());
            service
                .save_configuration(&ServerConfig {
                    id: id.clone(),
                    address: "127.0.0.1:9090".into(),
                    label: "Synthetic reader".into(),
                    reader_uuid: Some(uuid.clone()),
                })
                .unwrap();
            let intention = Intention {
                id: Uuid::new_v4().to_string(),
                device_id: id.clone(),
                reader_uuid: uuid.clone(),
                relative_path: path.clone(),
                sha256: hex(&Sha256::digest(original)),
                size_bytes: expected_size,
            };
            let mut journal = serde_json::to_value(&intention).unwrap();
            if expected_size.is_none() {
                journal.as_object_mut().unwrap().remove("sizeBytes");
            }
            service
                .save_intention(&serde_json::from_value(journal).unwrap())
                .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let remote_bytes = bytes.to_vec();
            let expected_path = path.clone();
            let client = tokio::spawn(async move {
                let mut socket = TcpStream::connect(address).await.unwrap();
                let mut current = Some(remote_bytes);
                let mut deletions = 0;
                let mut reads = 0;
                while let Ok((opcode, args)) = read_frame(&mut socket).await {
                    match opcode {
                        GET_FILE => {
                            assert_eq!(args["lpath"], expected_path);
                            reads += 1;
                            let contents = current.as_ref().unwrap();
                            write_frame(&mut socket, OK, json!({"fileLength":contents.len()}))
                                .await
                                .unwrap();
                            socket.write_all(contents).await.unwrap();
                        }
                        DELETE_BOOK => {
                            assert!(
                                should_delete,
                                "Modified, partial or unknown content must never be deleted"
                            );
                            assert_eq!(args["lpaths"][0], expected_path);
                            assert_eq!(current.as_deref(), Some(original.as_slice()));
                            deletions += 1;
                            write_frame(&mut socket, OK, json!({})).await.unwrap();
                            current = None;
                            write_frame(&mut socket, OK, json!({"uuid":"synthetic"}))
                                .await
                                .unwrap();
                        }
                        BOOK_COUNT => {
                            assert!(current.is_none());
                            write_frame(&mut socket, OK, json!({"count":0,"willStream":true}))
                                .await
                                .unwrap();
                        }
                        NOOP => {
                            assert_eq!(args["count"], 0);
                        }
                        _ => panic!("Unexpected cleanup opcode {opcode}"),
                    }
                }
                (current, deletions, reads)
            });
            let (socket, _) = listener.accept().await.unwrap();
            let mut peer = Peer {
                stream: socket,
                uuid,
                extensions: BTreeSet::from(["txt".into()]),
                packet_size: 4096,
                path_limits: BTreeMap::new(),
            };
            let removed = service
                .cleanup_intentions(&id, &mut peer, &[json!({"lpath":path})])
                .await
                .unwrap();
            assert_eq!(removed.contains(&path), should_delete);
            assert_eq!(service.intentions(&id).unwrap().is_empty(), should_delete);
            assert_eq!(service.warnings(&id).unwrap().is_empty(), should_delete);
            let presence: i64 = database
                .connect()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM device_books WHERE device_id=?1",
                    [&id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(presence, 0);
            drop(peer);
            let (remaining, deletions, reads) = timeout(Duration::from_secs(5), client)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(deletions, usize::from(should_delete));
            assert_eq!(reads, usize::from(expected_size.is_some()));
            if !should_delete {
                assert_eq!(remaining.as_deref(), Some(bytes));
            }
        }
    }

    #[tokio::test]
    async fn standalone_server_authenticates_indexes_transfers_cancels_and_disconnects() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::new(&directory.path().join("data")).unwrap();
        let storage = Storage::new(&directory.path().join("storage")).unwrap();
        let books = BookRepository::new(database.clone());
        let service = CalibreService::new(
            database.clone(),
            DeviceService::new(database.clone()),
            storage.clone(),
            books.clone(),
        );
        let add = |text: &[u8], title: &str| {
            let id = Uuid::new_v4().to_string();
            let source = directory.path().join(format!("{id}.txt"));
            std::fs::write(&source, text).unwrap();
            let artifact = storage.import_original(&source, BookFormat::Txt).unwrap();
            let file = StoredFile {
                relative_path: artifact.relative_path,
                file: BookFile {
                    id: Uuid::new_v4().to_string(),
                    book_id: id.clone(),
                    format: BookFormat::Txt,
                    variant: FileVariant::Original,
                    profile: None,
                    size_bytes: artifact.size_bytes,
                    sha256: artifact.sha256,
                    created_at: Utc::now().to_rfc3339(),
                },
            };
            books
                .insert(
                    &id,
                    crate::models::BookMetadata {
                        title: title.into(),
                        authors: vec!["Synthetic author".into()],
                        ..Default::default()
                    },
                    &[file],
                    None,
                )
                .unwrap();
            id
        };
        let book_id = add(b"Native standalone book text", "Standalone sample");
        let cancelled_id = add(b"Cancelled synthetic book text", "Cancellation sample");
        let pending = service
            .start("127.0.0.1:0", "Synthetic KOReader", Some("test-password"))
            .await
            .unwrap();
        assert!(!pending.connected);
        let cancellation = Arc::new(AtomicBool::new(false));
        let cancel_verification = Arc::new(AtomicBool::new(false));
        let cancel_on_send = Arc::new(AtomicBool::new(false));
        let device_send_cancel = cancel_on_send.clone();
        let device_cancel = cancellation.clone();
        let device_mode = cancel_verification.clone();
        let address = pending.address.clone().unwrap();
        let client = tokio::spawn(async move {
            let mut socket = TcpStream::connect(address).await.unwrap();
            let reader_uuid = Uuid::new_v4().to_string();
            let mut files: BTreeMap<String, (Vec<u8>, Value)> = BTreeMap::from([(
                "preexisting.txt".into(),
                (
                    b"Leave unchanged".to_vec(),
                    json!({"lpath":"preexisting.txt","title":"Existing reader book","authors":["Synthetic device author"]}),
                ),
            )]);
            loop {
                let (opcode, args) = read_frame(&mut socket).await.unwrap();
                match opcode {
                    INITIALIZE=> {
                        let challenge=args["passwordChallenge"].as_str().unwrap();
                        let hash=hex(&Sha1::digest(format!("test-password{challenge}").as_bytes()));
                        write_frame(&mut socket,OK,json!({"versionOK":true,"canReceiveBookBinary":true,"canSendOkToSendbook":true,"canStreamBooks":true,"canStreamMetadata":true,"canUseCachedMetadata":true,"cacheUsesLpaths":true,"canDeleteMultipleBooks":true,"canAcceptLibraryInfo":true,"maxBookContentPacketLen":4096,"acceptedExtensions":["txt","epub"],"passwordHash":hash})).await.unwrap();
                    }
                    INFO=>write_frame(&mut socket,OK,json!({"device_info":{"device_store_uuid":reader_uuid},"version":"synthetic"})).await.unwrap(),
                    1|19=>write_frame(&mut socket,OK,json!({})).await.unwrap(),
                    FREE_SPACE=>write_frame(&mut socket,OK,json!({"free_space_on_device":1024*1024})).await.unwrap(),
                    BOOK_COUNT=> {
                        write_frame(&mut socket,OK,json!({"count":files.len(),"willStream":true,"willScan":true})).await.unwrap();
                        for (index,path) in files.keys().enumerate() { write_frame(&mut socket,OK,json!({"priKey":index+1,"lpath":path})).await.unwrap(); }
                    }
                    NOOP=> {
                        if args.get("count").is_some() { continue; }
                        if let Some(key)=args.get("priKey").and_then(Value::as_u64) { let metadata=files.values().nth(key as usize-1).unwrap().1.clone();write_frame(&mut socket,OK,metadata).await.unwrap(); }
                        else { write_frame(&mut socket,OK,json!({})).await.unwrap();if args["ejecting"]==true { break; } }
                    }
                    SEND_BOOK=> {
                        let path=args["lpath"].as_str().unwrap().to_owned();assert!(owned_path(&path));assert!(!files.contains_key(&path));
                        if device_send_cancel.load(Ordering::Acquire) { device_cancel.store(true,Ordering::Release); }
                        write_frame(&mut socket,OK,json!({})).await.unwrap();let mut bytes=vec![0;args["length"].as_u64().unwrap() as usize];socket.read_exact(&mut bytes).await.unwrap();
                        files.insert(path,(bytes,args["metadata"].clone()));
                    }
                    GET_FILE=> {
                        let path=args["lpath"].as_str().unwrap();let bytes=&files.get(path).unwrap().0;
                        if device_mode.load(Ordering::Acquire) && owned_path(path) { device_cancel.store(true,Ordering::Release); }
                        write_frame(&mut socket,OK,json!({"fileLength":bytes.len()})).await.unwrap();socket.write_all(bytes).await.unwrap();
                    }
                    DELETE_BOOK=> {
                        let path=args["lpaths"][0].as_str().unwrap();assert!(owned_path(path));assert_ne!(path,"preexisting.txt");
                        write_frame(&mut socket,OK,json!({})).await.unwrap();files.remove(path);write_frame(&mut socket,OK,json!({"uuid":"synthetic"})).await.unwrap();
                    }
                    _=>panic!("Unexpected opcode {opcode}"),
                }
            }
            files
        });
        timeout(Duration::from_secs(5), async {
            loop {
                if !service.connected_ids().await.unwrap().is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let report = service
            .transfer(
                &pending.id,
                std::slice::from_ref(&book_id),
                None,
                &cancellation,
            )
            .await
            .unwrap();
        assert_eq!(report.copied, 1);
        assert!(report.warnings.is_empty());
        let indexed = service.index(&pending.id).await.unwrap();
        assert_eq!(indexed.len(), 2);
        assert_eq!(
            indexed
                .iter()
                .filter(|book| book.book_id.as_deref() == Some(&book_id))
                .count(),
            1
        );
        assert_eq!(
            books
                .get(&book_id, &service.connected_ids().await.unwrap())
                .unwrap()
                .on_device_ids,
            vec![pending.id.clone()]
        );
        let repeat = service
            .transfer(
                &pending.id,
                std::slice::from_ref(&book_id),
                None,
                &cancellation,
            )
            .await
            .unwrap();
        assert_eq!(repeat.copied, 0);
        assert_eq!(repeat.skipped, 1);
        cancel_verification.store(true, Ordering::Release);
        assert!(matches!(
            service
                .transfer(
                    &pending.id,
                    std::slice::from_ref(&cancelled_id),
                    None,
                    &cancellation
                )
                .await,
            Err(AppError::Cancelled)
        ));
        assert!(service.warnings(&pending.id).unwrap().is_empty());
        assert!(
            books
                .get(&cancelled_id, std::slice::from_ref(&pending.id))
                .unwrap()
                .on_device_ids
                .is_empty()
        );
        cancel_verification.store(false, Ordering::Release);
        cancel_on_send.store(true, Ordering::Release);
        cancellation.store(false, Ordering::Release);
        assert!(matches!(
            service
                .transfer(
                    &pending.id,
                    std::slice::from_ref(&cancelled_id),
                    None,
                    &cancellation
                )
                .await,
            Err(AppError::Cancelled)
        ));
        assert!(service.warnings(&pending.id).unwrap().is_empty());
        service.disconnect(&pending.id).await.unwrap();
        assert!(service.connected_ids().await.unwrap().is_empty());
        assert!(
            books
                .get(&book_id, &[pending.id])
                .unwrap()
                .on_device_ids
                .is_empty()
        );
        let files = timeout(Duration::from_secs(5), client)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files["preexisting.txt"].0, b"Leave unchanged");
    }
}
