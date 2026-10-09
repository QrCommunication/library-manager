//! Bounded, non-overwriting USB and direct CrossPoint transfers.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use reqwest::{Client, Response, multipart};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use rustix::fd::OwnedFd;
use rustix::fs::{
    AtFlags, Mode, OFlags, RenameFlags, fstatvfs, mkdirat, open, openat, renameat_with, unlinkat,
};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use url::Url;
use uuid::Uuid;

use crate::book_repository::{BookRepository, StoredFile};
use crate::database::Database;
use crate::devices::{DeviceService, IndexedDeviceBook};
use crate::epub::EpubDocument;
use crate::error::{AppError, Result};
use crate::models::{
    Book, BookFile, BookFormat, BookMetadata, Device, DeviceTransport, FileVariant,
};
use crate::optimizer;
use crate::storage::{MAX_FILE_BYTES, Storage};

const MAX_BATCH_BOOKS: usize = 200;
const MAX_PAIRED_DEVICES: usize = 16;
const MAX_REMOTE_BOOKS: usize = 20_000;
const MAX_REMOTE_ENTRIES: usize = 100_000;
const MAX_REMOTE_DEPTH: usize = 32;
const MAX_REMOTE_INDEX_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_JSON_BYTES: usize = 2 * 1024 * 1024;
const MAX_REPLY_BYTES: usize = 32 * 1024;
const STATUS_TIMEOUT: Duration = Duration::from_secs(8);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(300);
const PAIR_PREFIX: &str = "wirelessDevice:";
const COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TransferItemStatus {
    Copied,
    AlreadyPresent,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferItem {
    pub book_id: String,
    pub file_id: String,
    pub relative_path: String,
    pub size_bytes: u64,
    pub status: TransferItemStatus,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferReport {
    pub device_id: String,
    pub copied: u64,
    pub skipped: u64,
    pub failed: u64,
    pub items: Vec<TransferItem>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pairing {
    id: String,
    address: String,
    label: String,
    expected_device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteStatus {
    version: String,
    mode: String,
    device: String,
    #[serde(default)]
    device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteEntry {
    name: String,
    size: u64,
    is_directory: bool,
}

#[derive(Clone)]
struct WirelessDevice {
    pairing: Pairing,
    base: Url,
    client: Client,
}

#[derive(Default)]
struct WirelessState {
    connected: BTreeMap<String, WirelessDevice>,
    suppressed: BTreeSet<String>,
    cancellations: BTreeMap<String, Arc<AtomicBool>>,
}

/// Device networking is deliberately separate from the Internet client for AI.
#[derive(Clone)]
pub struct TransferService {
    database: Database,
    devices: DeviceService,
    storage: Storage,
    books: BookRepository,
    state: Arc<Mutex<WirelessState>>,
    #[cfg(test)]
    allow_test_endpoints: bool,
}

impl TransferService {
    pub fn new(
        database: Database,
        devices: DeviceService,
        storage: Storage,
        books: BookRepository,
    ) -> Self {
        Self {
            database,
            devices,
            storage,
            books,
            state: Arc::new(Mutex::new(WirelessState::default())),
            #[cfg(test)]
            allow_test_endpoints: false,
        }
    }

    pub async fn connect_wireless(
        &self,
        address: &str,
        transport: DeviceTransport,
        label: &str,
        password: Option<&str>,
    ) -> Result<Device> {
        if transport != DeviceTransport::Crosspoint {
            return Err(AppError::Unsupported(
                "Direct wireless pairing requires CrossPoint HTTP".into(),
            ));
        }
        if password.is_some_and(|value| !value.is_empty()) {
            return Err(AppError::Unsupported(
                "CrossPoint HTTP does not support password authentication".into(),
            ));
        }
        let (base, client) = self.device_client(address).await?;
        let status = read_status(&base, &client, None).await?;
        let stable = status.device_id.as_deref().unwrap_or(base.as_str());
        let id = format!("crosspoint-{}", &digest_bytes(stable.as_bytes())[..32]);
        let label = clean_label(label, &format!("CrossPoint {}", status.device));
        let pairing = Pairing {
            id: id.clone(),
            address: base.to_string(),
            label,
            expected_device_id: status.device_id,
        };
        let paired = self.pairings()?;
        if paired.len() >= MAX_PAIRED_DEVICES && !paired.iter().any(|pair| pair.id == id) {
            return Err(AppError::InvalidInput(
                "Wireless device pairing limit exceeded".into(),
            ));
        }
        self.save_pairing(&pairing)?;
        let mut state = self.lock_state()?;
        state.suppressed.remove(&id);
        state.connected.insert(
            id.clone(),
            WirelessDevice {
                pairing: pairing.clone(),
                base,
                client,
            },
        );
        drop(state);
        self.wireless_dto(&pairing, true)
    }

    /// Disconnects for this application session. Pairing may reconnect next launch.
    pub fn disconnect(&self, id: &str) -> Result<()> {
        let mut state = self.lock_state()?;
        state.connected.remove(id);
        state.suppressed.insert(id.into());
        if let Some(cancel) = state.cancellations.get(id) {
            cancel.store(true, Ordering::Release);
        }
        Ok(())
    }

    pub fn cancel(&self, id: &str) -> Result<()> {
        if let Some(cancel) = self.lock_state()?.cancellations.get(id) {
            cancel.store(true, Ordering::Release);
        }
        Ok(())
    }

    pub async fn scan_wireless(&self) -> Result<Vec<Device>> {
        let mut result = Vec::new();
        for pairing in self.pairings()? {
            if self.lock_state()?.suppressed.contains(&pairing.id) {
                result.push(self.wireless_dto(&pairing, false)?);
                continue;
            }
            let previous = self.lock_state()?.connected.get(&pairing.id).cloned();
            let connection = match previous {
                Some(connection) => Ok(connection),
                None => self
                    .device_client(&pairing.address)
                    .await
                    .map(|(base, client)| WirelessDevice {
                        pairing: pairing.clone(),
                        base,
                        client,
                    }),
            };
            let connected = match connection {
                Ok(connection) if verify_wireless(&connection, None).await.is_ok() => {
                    let first = self
                        .lock_state()?
                        .connected
                        .insert(pairing.id.clone(), connection)
                        .is_none();
                    if first {
                        self.database.connect()?.execute(
                            "DELETE FROM device_books WHERE device_id = ?1",
                            [&pairing.id],
                        )?;
                    }
                    self.database.connect()?.execute(
                        "UPDATE devices SET last_seen_at = ?2 WHERE id = ?1",
                        params![pairing.id, Utc::now().to_rfc3339()],
                    )?;
                    true
                }
                _ => {
                    self.lock_state()?.connected.remove(&pairing.id);
                    false
                }
            };
            result.push(self.wireless_dto(&pairing, connected)?);
        }
        Ok(result)
    }

    pub async fn connected_wireless_ids(&self) -> Result<Vec<String>> {
        Ok(self
            .scan_wireless()
            .await?
            .into_iter()
            .filter(|device| device.connected)
            .map(|device| device.id)
            .collect())
    }

    pub async fn index_wireless(&self, id: &str) -> Result<Vec<IndexedDeviceBook>> {
        let device = self.connected_wireless(id).await?;
        let cancellation = self.begin_operation(id)?;
        let result = self.index_remote(&device, &cancellation.flag).await;
        drop(cancellation);
        result
    }

    pub async fn transfer(
        &self,
        id: &str,
        book_ids: &[String],
        profile_id: Option<&str>,
    ) -> Result<TransferReport> {
        if book_ids.is_empty() || book_ids.len() > MAX_BATCH_BOOKS {
            return Err(AppError::InvalidInput(
                "Transfer batch must contain 1 to 200 books".into(),
            ));
        }
        if let Some(profile_id) = profile_id {
            optimizer::profile(profile_id)?;
        }
        let cancellation = self.begin_operation(id)?;
        let wireless = if id.starts_with("crosspoint-") {
            Some(self.connected_wireless(id).await?)
        } else {
            None
        };
        let device_profile = if wireless.is_none() {
            let mounted = self
                .devices
                .scan()?
                .into_iter()
                .find(|device| device.id == id && device.connected)
                .ok_or_else(|| AppError::NotFound("Device is disconnected".into()))?;
            if !mounted.writable {
                return Err(AppError::Unsupported("Device is mounted read-only".into()));
            }
            mounted.profile
        } else {
            "xteink".into()
        };
        let mut report = TransferReport {
            device_id: id.into(),
            ..Default::default()
        };
        if wireless
            .as_ref()
            .is_some_and(|connection| connection.pairing.expected_device_id.is_none())
        {
            report.warnings.push("wirelessIdentityAddressOnly".into());
        }
        let mut unique = BTreeSet::new();
        for book_id in book_ids
            .iter()
            .filter(|book_id| unique.insert((*book_id).clone()))
        {
            check_cancel(&cancellation.flag)?;
            let storage = self.storage.clone();
            let books = self.books.clone();
            let book_id = book_id.clone();
            let preparation_id = book_id.clone();
            let profile_id = profile_id.map(str::to_owned);
            let device_profile = device_profile.clone();
            let cancel = cancellation.flag.clone();
            let prepared = tokio::task::spawn_blocking(move || {
                prepare_book(
                    &storage,
                    &books,
                    &book_id,
                    profile_id.as_deref(),
                    &device_profile,
                    &cancel,
                )
            })
            .await
            .map_err(|_| AppError::Conflict("Book preparation worker failed".into()))?;
            let prepared = match prepared {
                Ok(prepared) => prepared,
                Err(AppError::Cancelled) => return Err(AppError::Cancelled),
                Err(error) => {
                    report.failed += 1;
                    report.items.push(TransferItem {
                        book_id: preparation_id,
                        file_id: String::new(),
                        relative_path: String::new(),
                        size_bytes: 0,
                        status: TransferItemStatus::Failed,
                        error_code: Some(error.code().into()),
                    });
                    continue;
                }
            };
            report.warnings.extend(prepared.warnings.clone());
            let result = match &wireless {
                Some(device) => {
                    self.copy_wireless(device, &prepared, &cancellation.flag)
                        .await
                }
                None => {
                    let devices = self.devices.clone();
                    let id = id.to_owned();
                    let prepared = prepared.clone();
                    let cancel = cancellation.flag.clone();
                    tokio::task::spawn_blocking(move || copy_usb(&devices, &id, &prepared, &cancel))
                        .await
                        .map_err(|_| AppError::Conflict("USB transfer worker failed".into()))?
                }
            };
            let (status, relative_path, error_code) = match result {
                Ok((status, path)) => {
                    match status {
                        TransferItemStatus::Copied => report.copied += 1,
                        TransferItemStatus::AlreadyPresent => report.skipped += 1,
                        TransferItemStatus::Failed => report.failed += 1,
                    }
                    if self.record_presence(id, &prepared, &path).is_err() {
                        report.warnings.push("inventoryUpdateFailed".into());
                    }
                    (status, path, None)
                }
                Err(AppError::Cancelled) => return Err(AppError::Cancelled),
                Err(error) => {
                    report.failed += 1;
                    (
                        TransferItemStatus::Failed,
                        prepared.relative_path.clone(),
                        Some(error.code().into()),
                    )
                }
            };
            report.items.push(TransferItem {
                book_id: prepared.book.id.clone(),
                file_id: prepared.file.file.id.clone(),
                relative_path,
                size_bytes: prepared.file.file.size_bytes,
                status,
                error_code,
            });
        }
        report.warnings.sort();
        report.warnings.dedup();
        Ok(report)
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, WirelessState>> {
        self.state
            .lock()
            .map_err(|_| AppError::Conflict("Wireless state is unavailable".into()))
    }

    fn begin_operation(&self, id: &str) -> Result<OperationGuard> {
        let mut state = self.lock_state()?;
        if state.cancellations.contains_key(id) {
            return Err(AppError::Conflict(
                "An operation is already running on this device".into(),
            ));
        }
        let flag = Arc::new(AtomicBool::new(false));
        state.cancellations.insert(id.into(), flag.clone());
        Ok(OperationGuard {
            id: id.into(),
            flag,
            state: self.state.clone(),
        })
    }

    async fn device_client(&self, address: &str) -> Result<(Url, Client)> {
        #[cfg(test)]
        let allow_loopback = self.allow_test_endpoints;
        #[cfg(not(test))]
        let allow_loopback = false;
        let base = validate_address(address, allow_loopback)?;
        let hostname = base
            .host_str()
            .ok_or_else(|| AppError::InvalidInput("Missing device host".into()))?
            .trim_matches(['[', ']']);
        let port = base.port_or_known_default().unwrap_or(80);
        let addresses: Vec<SocketAddr> =
            tokio::time::timeout(STATUS_TIMEOUT, tokio::net::lookup_host((hostname, port)))
                .await
                .map_err(|_| AppError::Network("Device name resolution timed out".into()))?
                .map_err(|_| AppError::Network("Device name could not be resolved".into()))?
                .take(16)
                .collect();
        if addresses.is_empty()
            || addresses
                .iter()
                .any(|address| !allowed_device_ip(address.ip(), allow_loopback))
        {
            return Err(AppError::InvalidInput(
                "Device must resolve exclusively to a private LAN address".into(),
            ));
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(STATUS_TIMEOUT)
            .timeout(TRANSFER_TIMEOUT)
            .resolve_to_addrs(hostname, &addresses)
            .build()
            .map_err(|_| AppError::Network("Cannot initialize device HTTP client".into()))?;
        Ok((base, client))
    }

    fn pairings(&self) -> Result<Vec<Pairing>> {
        let connection = self.database.connect()?;
        let mut statement = connection.prepare("SELECT value_json FROM settings WHERE key LIKE 'wirelessDevice:%' ORDER BY key LIMIT 17")?;
        let records = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut result = Vec::new();
        for record in records {
            result.push(serde_json::from_str::<Pairing>(&record?)?);
        }
        if result.len() > MAX_PAIRED_DEVICES {
            return Err(AppError::InvalidInput(
                "Wireless pairing limit exceeded".into(),
            ));
        }
        Ok(result)
    }

    fn save_pairing(&self, pairing: &Pairing) -> Result<()> {
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("INSERT INTO devices(id,label,transport,profile,mount_identity,last_seen_at)
            VALUES (?1,?2,'crosspoint','xteink',?3,?4) ON CONFLICT(id) DO UPDATE SET label=excluded.label,
            mount_identity=excluded.mount_identity,last_seen_at=excluded.last_seen_at",
            params![pairing.id, pairing.label, pairing.address, Utc::now().to_rfc3339()])?;
        transaction.execute("INSERT INTO settings(key,value_json) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json",
            params![format!("{PAIR_PREFIX}{}", pairing.id), serde_json::to_string(pairing)?])?;
        transaction.execute("DELETE FROM device_books WHERE device_id=?1", [&pairing.id])?;
        transaction.commit()?;
        Ok(())
    }

    fn wireless_dto(&self, pairing: &Pairing, connected: bool) -> Result<Device> {
        let connection = self.database.connect()?;
        let (last_seen_at, book_count, matched_book_count): (String, i64, i64) = connection.query_row(
            "SELECT last_seen_at, (SELECT count(*) FROM device_books WHERE device_id=?1),
             (SELECT count(*) FROM device_books WHERE device_id=?1 AND book_id IS NOT NULL) FROM devices WHERE id=?1",
            [&pairing.id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        Ok(Device {
            id: pairing.id.clone(),
            label: pairing.label.clone(),
            transport: DeviceTransport::Crosspoint,
            connected,
            writable: connected,
            profile: "xteink".into(),
            mount_path: None,
            address: Some(pairing.address.clone()),
            total_bytes: None,
            free_bytes: None,
            book_count: nonnegative(book_count)?,
            matched_book_count: nonnegative(matched_book_count)?,
            last_seen_at,
        })
    }

    async fn connected_wireless(&self, id: &str) -> Result<WirelessDevice> {
        self.scan_wireless().await?;
        self.lock_state()?
            .connected
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::NotFound("Wireless device is disconnected".into()))
    }

    async fn index_remote(
        &self,
        device: &WirelessDevice,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Vec<IndexedDeviceBook>> {
        let mut pending = vec![(String::from("/"), 0_usize)];
        let mut seen = BTreeSet::new();
        let mut records = Vec::new();
        let mut visited = 0;
        let mut total_bytes = 0_u64;
        let timestamp = Utc::now().to_rfc3339();
        while let Some((directory, depth)) = pending.pop() {
            check_cancel(cancel)?;
            if !seen.insert(directory.clone()) {
                return Err(AppError::InvalidInput("Remote directory cycle".into()));
            }
            for entry in remote_files(device, &directory, Some(cancel)).await? {
                visited += 1;
                if visited > MAX_REMOTE_ENTRIES {
                    return Err(AppError::Unsupported(
                        "Remote inventory entry limit exceeded".into(),
                    ));
                }
                if ignored_remote_name(&entry.name) {
                    continue;
                }
                validate_component(&entry.name)?;
                let path = join_remote(&directory, &entry.name);
                if entry.is_directory {
                    if depth == MAX_REMOTE_DEPTH {
                        return Err(AppError::Unsupported(
                            "Remote inventory depth limit exceeded".into(),
                        ));
                    }
                    pending.push((path, depth + 1));
                    continue;
                }
                let Some(format) = format_for_name(&entry.name) else {
                    continue;
                };
                if records.len() == MAX_REMOTE_BOOKS {
                    return Err(AppError::Unsupported(
                        "Remote inventory book limit exceeded".into(),
                    ));
                }
                total_bytes = total_bytes.checked_add(entry.size).ok_or_else(|| {
                    AppError::Unsupported("Remote inventory byte limit exceeded".into())
                })?;
                if entry.size > MAX_FILE_BYTES || total_bytes > MAX_REMOTE_INDEX_BYTES {
                    return Err(AppError::Unsupported(
                        "Remote inventory byte limit exceeded".into(),
                    ));
                }
                let temporary = WorkingDirectory::new(&self.storage)?;
                let local = temporary.path.join("download.epub");
                let hash = download_to(device, &path, &local, entry.size, Some(cancel)).await?;
                let mut title = Path::new(&entry.name)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let mut authors = Vec::new();
                let mut warnings = Vec::new();
                if format == BookFormat::Epub {
                    let document = tokio::task::spawn_blocking(move || EpubDocument::open(&local))
                        .await
                        .map_err(|_| {
                            AppError::Conflict("Remote EPUB inspection worker failed".into())
                        })?;
                    match document {
                        Ok(document) => {
                            title = document.metadata.title.chars().take(4096).collect();
                            authors = document
                                .metadata
                                .authors
                                .into_iter()
                                .take(128)
                                .map(|author| author.chars().take(4096).collect())
                                .collect();
                        }
                        Err(_) => warnings.push("metadataUnavailable".into()),
                    }
                }
                records.push(IndexedDeviceBook {
                    device_id: device.pairing.id.clone(),
                    relative_path: path.trim_start_matches('/').into(),
                    book_id: None,
                    sha256: Some(hash),
                    title,
                    authors,
                    format,
                    size_bytes: entry.size,
                    last_seen_at: timestamp.clone(),
                    warnings,
                });
            }
        }
        verify_wireless(device, Some(cancel)).await?;
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM device_books WHERE device_id=?1",
            [&device.pairing.id],
        )?;
        for record in &mut records {
            record.book_id = transaction
                .query_row(
                    "SELECT book_id FROM book_files WHERE sha256=?1",
                    [&record.sha256],
                    |row| row.get(0),
                )
                .optional()?;
            transaction.execute("INSERT INTO device_books(device_id,relative_path,book_id,sha256,title,authors_json,format,size_bytes,last_seen_at)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![record.device_id, record.relative_path, record.book_id, record.sha256, record.title,
                serde_json::to_string(&record.authors)?, record.format.as_str(), signed_size(record.size_bytes)?, timestamp])?;
        }
        check_cancel(cancel)?;
        transaction.commit()?;
        Ok(records)
    }

    async fn copy_wireless(
        &self,
        device: &WirelessDevice,
        book: &PreparedBook,
        cancel: &Arc<AtomicBool>,
    ) -> Result<(TransferItemStatus, String)> {
        if !matches!(book.file.file.format, BookFormat::Epub | BookFormat::Txt) {
            return Err(AppError::Unsupported(
                "CrossPoint reads EPUB and plain text; convert this format first".into(),
            ));
        }
        verify_wireless(device, Some(cancel)).await?;
        let connection = self.database.connect()?;
        let previous: Option<(String, i64)> = connection.query_row(
            "SELECT relative_path,size_bytes FROM device_books WHERE device_id=?1 AND sha256=?2 LIMIT 1",
            params![device.pairing.id, book.file.file.sha256], |row| Ok((row.get(0)?,row.get(1)?)),
        ).optional()?;
        drop(connection);
        if let Some((previous, size)) = previous {
            validate_relative(&previous)?;
            let path = format!("/{previous}");
            let (parent, name) = path
                .rsplit_once('/')
                .ok_or_else(|| AppError::InvalidInput("Invalid cached device path".into()))?;
            if let Ok(entries) = remote_files(
                device,
                if parent.is_empty() { "/" } else { parent },
                Some(cancel),
            )
            .await
                && entries.iter().any(|entry| {
                    entry.name == name
                        && !entry.is_directory
                        && entry.size == book.file.file.size_bytes
                })
            {
                let temporary = WorkingDirectory::new(&self.storage)?;
                if download_to(
                    device,
                    &path,
                    &temporary.path.join("download.epub"),
                    nonnegative(size)?,
                    Some(cancel),
                )
                .await?
                    == book.file.file.sha256
                {
                    return Ok((TransferItemStatus::AlreadyPresent, previous));
                }
            }
        }
        let remote = format!("/{}", book.relative_path);
        let (parent, filename) = remote
            .rsplit_once('/')
            .ok_or_else(|| AppError::InvalidInput("Missing remote filename".into()))?;
        ensure_remote_directories(device, parent, cancel).await?;
        if let Some(existing) = remote_files(device, parent, Some(cancel))
            .await?
            .into_iter()
            .find(|entry| entry.name.to_lowercase() == filename.to_lowercase())
        {
            if existing.is_directory || existing.size != book.file.file.size_bytes {
                return Err(AppError::Conflict(
                    "Device destination already contains different content".into(),
                ));
            }
            let temporary = WorkingDirectory::new(&self.storage)?;
            let hash = download_to(
                device,
                &join_remote(parent, &existing.name),
                &temporary.path.join("download.epub"),
                existing.size,
                Some(cancel),
            )
            .await?;
            if hash != book.file.file.sha256 {
                return Err(AppError::Conflict(
                    "Device destination already contains different content".into(),
                ));
            }
            return Ok((
                TransferItemStatus::AlreadyPresent,
                join_remote(parent, &existing.name)
                    .trim_start_matches('/')
                    .into(),
            ));
        }
        let temporary_name = format!(
            "LibraryManagerUpload-{}.{}",
            Uuid::new_v4(),
            book.file.file.format.as_str()
        );
        let temporary_path = join_remote(parent, &temporary_name);
        let mut staging_started = false;
        let result = async {
            verify_wireless(device, Some(cancel)).await?;
            let file = File::from(open_absolute_regular(&book.path)?);
            let file = tokio::fs::File::from_std(file);
            let part = multipart::Part::stream_with_length(file, book.file.file.size_bytes)
                .file_name(temporary_name.clone());
            staging_started = true;
            let response = send(
                device
                    .client
                    .post(endpoint(&device.base, "/upload", &[("path", parent)])?)
                    .multipart(multipart::Form::new().part("file", part)),
                Some(cancel),
            )
            .await?;
            bounded_reply(response, MAX_REPLY_BYTES, Some(cancel)).await?;
            let local = WorkingDirectory::new(&self.storage)?;
            let received = download_to(
                device,
                &temporary_path,
                &local.path.join("download.epub"),
                book.file.file.size_bytes,
                Some(cancel),
            )
            .await?;
            if received != book.file.file.sha256 {
                return Err(AppError::Conflict(
                    "Uploaded device file failed hash verification".into(),
                ));
            }
            if remote_files(device, parent, Some(cancel))
                .await?
                .iter()
                .any(|entry| entry.name.to_lowercase() == filename.to_lowercase())
            {
                return Err(AppError::Conflict(
                    "Destination appeared while uploading".into(),
                ));
            }
            verify_wireless(device, Some(cancel)).await?;
            let response = send(
                device
                    .client
                    .post(endpoint(&device.base, "/rename", &[])?)
                    .form(&[("path", temporary_path.as_str()), ("name", filename)]),
                Some(cancel),
            )
            .await?;
            bounded_reply(response, MAX_REPLY_BYTES, Some(cancel)).await?;
            Ok((TransferItemStatus::Copied, book.relative_path.clone()))
        }
        .await;
        if result.is_err() && staging_started && verify_wireless(device, None).await.is_ok() {
            // Only this operation's unguessable staging name may be removed.
            if let Ok(response) = send(
                device
                    .client
                    .post(endpoint(&device.base, "/delete", &[])?)
                    .timeout(STATUS_TIMEOUT)
                    .form(&[("path", temporary_path.as_str())]),
                None,
            )
            .await
            {
                let _ = bounded_reply(response, MAX_REPLY_BYTES, None).await;
            }
        }
        result
    }

    fn record_presence(&self, id: &str, book: &PreparedBook, relative: &str) -> Result<()> {
        validate_relative(relative)?;
        self.database.connect()?.execute("INSERT INTO device_books(device_id,relative_path,book_id,sha256,title,authors_json,format,size_bytes,last_seen_at)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(device_id,relative_path) DO UPDATE SET
            book_id=excluded.book_id,sha256=excluded.sha256,title=excluded.title,authors_json=excluded.authors_json,
            format=excluded.format,size_bytes=excluded.size_bytes,last_seen_at=excluded.last_seen_at",
            params![id, relative, book.book.id, book.file.file.sha256, book.book.title, serde_json::to_string(&book.book.authors)?, book.file.file.format.as_str(), signed_size(book.file.file.size_bytes)?, Utc::now().to_rfc3339()])?;
        Ok(())
    }
}

struct OperationGuard {
    id: String,
    flag: Arc<AtomicBool>,
    state: Arc<Mutex<WirelessState>>,
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.cancellations.remove(&self.id);
        }
    }
}

#[derive(Clone)]
struct PreparedBook {
    book: Book,
    file: StoredFile,
    path: PathBuf,
    relative_path: String,
    warnings: Vec<String>,
}

fn prepare_book(
    storage: &Storage,
    repository: &BookRepository,
    id: &str,
    profile_id: Option<&str>,
    device_profile: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<PreparedBook> {
    check_cancel(cancel)?;
    let book = repository.get(id, &[])?;
    let all_files = repository.files(id)?;
    let files: Vec<StoredFile> = all_files
        .into_iter()
        .filter(|file| device_accepts(device_profile, file.file.format))
        .collect();
    let file = files
        .iter()
        .filter(|file| file.file.variant == FileVariant::Normalized)
        .max_by_key(|file| &file.file.created_at)
        .or_else(|| {
            files
                .iter()
                .find(|file| file.file.variant == FileVariant::Original)
        })
        .or_else(|| files.first())
        .cloned()
        .ok_or_else(|| {
            AppError::Unsupported(
                "Book has no format compatible with this reader; convert it first".into(),
            )
        })?;
    let mut selected = file;
    let mut warnings = Vec::new();
    if let Some(profile_id) = profile_id {
        if selected.file.format != BookFormat::Epub {
            return Err(AppError::Unsupported(
                "Optimization requires an EPUB".into(),
            ));
        }
        let variant_path = format!(
            "variants/{}/{profile_id}-{}.epub",
            safe_id(id)?,
            selected.file.sha256
        );
        if let Some(cached) = files.iter().find(|file| {
            file.relative_path == variant_path && file.file.profile.as_deref() == Some(profile_id)
        }) {
            selected = cached.clone();
        } else {
            let directory = WorkingDirectory::new(storage)?;
            let source = storage.resolve(&selected.relative_path)?;
            if Storage::hash_file(&source)? != selected.file.sha256 {
                return Err(AppError::Conflict("Managed source hash changed".into()));
            }
            let output = directory.path.join("optimized.epub");
            let profile = optimizer::profile(profile_id)?;
            let report = optimizer::optimize(&source, &output, &profile, id, &selected.file.id)?;
            check_cancel(cancel)?;
            let artifact = storage.publish_file(&variant_path, &output)?;
            warnings.extend(report.warnings);
            if let Some(existing) = repository.find_file_by_hash(&artifact.sha256)? {
                selected = existing;
            } else {
                selected = StoredFile {
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
                repository.add_file(selected.clone())?;
            }
        }
    }
    check_cancel(cancel)?;
    let path = storage.resolve(&selected.relative_path)?;
    if Storage::hash_file(&path)? != selected.file.sha256 {
        return Err(AppError::Conflict(
            "Managed transfer file hash changed".into(),
        ));
    }
    let metadata = metadata_from_book(&book);
    let relative_path = storage
        .managed_book_path(id, &metadata, selected.file.format)
        .replacen("books/", "Books/", 1);
    validate_relative(&relative_path)?;
    Ok(PreparedBook {
        book,
        file: selected,
        path,
        relative_path,
        warnings,
    })
}

struct WorkingDirectory {
    path: PathBuf,
}
impl WorkingDirectory {
    fn new(storage: &Storage) -> Result<Self> {
        let relative = format!("cache/transfers/{}/reservation", Uuid::new_v4());
        storage.write_new(&relative, &[])?;
        let path = storage
            .resolve(&relative)?
            .parent()
            .ok_or_else(|| AppError::InvalidInput("Missing working directory".into()))?
            .to_path_buf();
        Ok(Self { path })
    }
}
impl Drop for WorkingDirectory {
    fn drop(&mut self) {
        for name in ["reservation", "download.epub", "optimized.epub"] {
            let _ = fs::remove_file(self.path.join(name));
        }
        let _ = fs::remove_dir(&self.path);
    }
}

fn copy_usb(
    devices: &DeviceService,
    id: &str,
    book: &PreparedBook,
    cancel: &Arc<AtomicBool>,
) -> Result<(TransferItemStatus, String)> {
    let root_path = devices.resolve_connected(id)?;
    for record in devices.inventory(id)? {
        if record.sha256.as_deref() == Some(book.file.file.sha256.as_str()) {
            validate_relative(&record.relative_path)?;
            let existing = root_path.join(&record.relative_path);
            if let Ok(file) = open_absolute_regular(&existing)
                && copy_and_hash(File::from(file), None, cancel)? == book.file.file.sha256
            {
                return Ok((TransferItemStatus::AlreadyPresent, record.relative_path));
            }
        }
    }
    copy_usb_at(&root_path, book, cancel, || {
        let current = devices.resolve_connected(id)?;
        if current != root_path {
            return Err(AppError::Conflict("Device mount path changed".into()));
        }
        Ok(())
    })
}

fn copy_usb_at(
    root_path: &Path,
    book: &PreparedBook,
    cancel: &Arc<AtomicBool>,
    revalidate: impl Fn() -> Result<()>,
) -> Result<(TransferItemStatus, String)> {
    check_cancel(cancel)?;
    let root = File::from(open_absolute_directory(root_path)?);
    let identity = root.metadata()?;
    validate_relative(&book.relative_path)?;
    let components: Vec<&str> = book.relative_path.split('/').collect();
    let filename = components
        .last()
        .ok_or_else(|| AppError::InvalidInput("Missing device filename".into()))?;
    let mut parent = root.try_clone()?;
    for component in &components[..components.len() - 1] {
        revalidate()?;
        match openat(
            &parent,
            *component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(directory) => parent = File::from(directory),
            Err(Errno::NOENT) => {
                match mkdirat(&parent, *component, Mode::from_raw_mode(0o755)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(error) => return Err(std::io::Error::from(error).into()),
                }
                parent = File::from(
                    openat(
                        &parent,
                        *component,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(std::io::Error::from)?,
                );
            }
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
    }
    if let Some(hash) = existing_hash(&parent, filename)? {
        if hash == book.file.file.sha256 {
            return Ok((
                TransferItemStatus::AlreadyPresent,
                book.relative_path.clone(),
            ));
        }
        return Err(AppError::Conflict(
            "Device destination already contains different content".into(),
        ));
    }
    if let Ok(stats) = fstatvfs(&parent)
        && stats
            .f_bavail
            .checked_mul(stats.f_frsize)
            .is_some_and(|available| {
                available < book.file.file.size_bytes.saturating_add(1024 * 1024)
            })
    {
        return Err(AppError::Unsupported(
            "Insufficient free space on device".into(),
        ));
    }
    let temporary_name = format!(".library-manager-transfer-{}", Uuid::new_v4());
    let mut stage = UsbStage {
        parent,
        temporary_name,
        published: None,
        committed: false,
    };
    let mut destination = File::from(
        openat(
            &stage.parent,
            stage.temporary_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o644),
        )
        .map_err(std::io::Error::from)?,
    );
    let source = File::from(open_absolute_regular(&book.path)?);
    let before = source.metadata()?;
    let hash = copy_and_hash(source, Some(&mut destination), cancel)?;
    destination.sync_all()?;
    if hash != book.file.file.sha256 || destination.metadata()?.len() != book.file.file.size_bytes {
        return Err(AppError::Conflict(
            "Transfer source changed during copying".into(),
        ));
    }
    let after = fs::symlink_metadata(&book.path)?;
    if !same_file(&before, &after) {
        return Err(AppError::Conflict("Transfer source was replaced".into()));
    }
    revalidate()?;
    let current_root = File::from(open_absolute_directory(root_path)?).metadata()?;
    if identity.dev() != current_root.dev() || identity.ino() != current_root.ino() {
        return Err(AppError::Conflict(
            "Device root changed before publication".into(),
        ));
    }
    check_cancel(cancel)?;
    let parent_path = root_path.join(components[..components.len() - 1].join("/"));
    let expected_parent = stage.parent.metadata()?;
    let reopened_parent = File::from(open_absolute_directory(&parent_path)?).metadata()?;
    if expected_parent.dev() != reopened_parent.dev()
        || expected_parent.ino() != reopened_parent.ino()
    {
        return Err(AppError::Conflict(
            "Device destination folder changed before publication".into(),
        ));
    }
    match renameat_with(
        &stage.parent,
        stage.temporary_name.as_str(),
        &stage.parent,
        *filename,
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {}
        Err(Errno::EXIST) => {
            if existing_hash(&stage.parent, filename)?.as_deref()
                == Some(book.file.file.sha256.as_str())
            {
                return Ok((
                    TransferItemStatus::AlreadyPresent,
                    book.relative_path.clone(),
                ));
            }
            return Err(AppError::Conflict(
                "Device destination appeared during transfer".into(),
            ));
        }
        Err(Errno::INVAL | Errno::NOSYS | Errno::OPNOTSUPP) => {
            return Err(AppError::Unsupported(
                "Device filesystem cannot publish atomically without overwriting".into(),
            ));
        }
        Err(error) => return Err(std::io::Error::from(error).into()),
    }
    let published = destination.metadata()?;
    stage.published = Some((filename.to_string(), published.dev(), published.ino()));
    stage.parent.sync_all()?;
    revalidate()?;
    let reopened_parent = File::from(open_absolute_directory(&parent_path)?).metadata()?;
    if expected_parent.dev() != reopened_parent.dev()
        || expected_parent.ino() != reopened_parent.ino()
    {
        return Err(AppError::Conflict(
            "Device destination folder changed during publication".into(),
        ));
    }
    stage.committed = true;
    Ok((TransferItemStatus::Copied, book.relative_path.clone()))
}

struct UsbStage {
    parent: File,
    temporary_name: String,
    published: Option<(String, u64, u64)>,
    committed: bool,
}
impl Drop for UsbStage {
    fn drop(&mut self) {
        if !self.committed
            && let Some((name, device, inode)) = &self.published
            && let Ok(file) = openat(
                &self.parent,
                name.as_str(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            && File::from(file)
                .metadata()
                .is_ok_and(|metadata| metadata.dev() == *device && metadata.ino() == *inode)
        {
            let _ = unlinkat(&self.parent, name.as_str(), AtFlags::empty());
        }
        let _ = unlinkat(&self.parent, self.temporary_name.as_str(), AtFlags::empty());
    }
}

fn existing_hash(parent: &File, name: &str) -> Result<Option<String>> {
    let fd = match openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(None),
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    Ok(Some(copy_and_hash(
        File::from(fd),
        None,
        &Arc::new(AtomicBool::new(false)),
    )?))
}

fn copy_and_hash(
    mut source: File,
    mut destination: Option<&mut File>,
    cancel: &Arc<AtomicBool>,
) -> Result<String> {
    let before = source.metadata()?;
    if !before.is_file() || before.len() > MAX_FILE_BYTES {
        return Err(AppError::Unsupported(
            "Transfer requires a regular file up to 512 MiB".into(),
        ));
    }
    let mut digest = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    loop {
        check_cancel(cancel)?;
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        copied += count as u64;
        if copied > MAX_FILE_BYTES {
            return Err(AppError::Unsupported(
                "Transfer source exceeds 512 MiB".into(),
            ));
        }
        digest.update(&buffer[..count]);
        if let Some(writer) = destination.as_deref_mut() {
            writer.write_all(&buffer[..count])?;
        }
    }
    if copied != before.len() || !same_file(&before, &source.metadata()?) {
        return Err(AppError::Conflict(
            "Transfer source changed during reading".into(),
        ));
    }
    Ok(hex_digest(digest.finalize().as_slice()))
}

fn open_absolute_directory(path: &Path) -> Result<OwnedFd> {
    if !path.is_absolute() {
        return Err(AppError::InvalidInput(
            "Transfer root must be absolute".into(),
        ));
    }
    let mut parent = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                parent = openat(
                    &parent,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(std::io::Error::from)?;
            }
            _ => return Err(AppError::InvalidInput("Unsafe transfer root path".into())),
        }
    }
    Ok(parent)
}

fn open_absolute_regular(path: &Path) -> Result<OwnedFd> {
    let directory = path
        .parent()
        .ok_or_else(|| AppError::InvalidInput("Missing source parent".into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| AppError::InvalidInput("Missing source filename".into()))?;
    let parent = open_absolute_directory(directory)?;
    let file = openat(
        &parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let metadata = File::from(file.try_clone()?).metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return Err(AppError::Unsupported(
            "Transfer source must be a regular file up to 512 MiB".into(),
        ));
    }
    Ok(file)
}

async fn read_status(
    base: &Url,
    client: &Client,
    cancel: Option<&Arc<AtomicBool>>,
) -> Result<RemoteStatus> {
    let response = send(
        client
            .get(endpoint(
                base,
                "/api/status",
                &[("plugin", "library-manager")],
            )?)
            .timeout(STATUS_TIMEOUT),
        cancel,
    )
    .await?;
    let status: RemoteStatus =
        serde_json::from_slice(&bounded_reply(response, MAX_REPLY_BYTES, cancel).await?)?;
    if status.version.is_empty()
        || !matches!(status.mode.as_str(), "STA" | "AP")
        || !matches!(status.device.as_str(), "X3" | "X4" | "X4Pro")
    {
        return Err(AppError::Unsupported(
            "Endpoint is not a compatible CrossPoint reader".into(),
        ));
    }
    if status
        .device_id
        .as_ref()
        .is_some_and(|id| id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(AppError::InvalidInput(
            "Invalid CrossPoint device identity".into(),
        ));
    }
    Ok(status)
}

async fn verify_wireless(device: &WirelessDevice, cancel: Option<&Arc<AtomicBool>>) -> Result<()> {
    let status = read_status(&device.base, &device.client, cancel).await?;
    if device.pairing.expected_device_id.is_some()
        && device.pairing.expected_device_id != status.device_id
    {
        return Err(AppError::Conflict(
            "Wireless address now belongs to a different device".into(),
        ));
    }
    Ok(())
}

async fn remote_files(
    device: &WirelessDevice,
    directory: &str,
    cancel: Option<&Arc<AtomicBool>>,
) -> Result<Vec<RemoteEntry>> {
    validate_remote_path(directory)?;
    let response = send(
        device
            .client
            .get(endpoint(
                &device.base,
                "/api/files",
                &[("path", directory)],
            )?)
            .timeout(STATUS_TIMEOUT),
        cancel,
    )
    .await?;
    let entries: Vec<RemoteEntry> =
        serde_json::from_slice(&bounded_reply(response, MAX_JSON_BYTES, cancel).await?)?;
    if entries.len() > MAX_REMOTE_BOOKS {
        return Err(AppError::Unsupported(
            "Remote folder entry limit exceeded".into(),
        ));
    }
    let mut names = BTreeSet::new();
    for entry in &entries {
        if entry.name.is_empty()
            || matches!(entry.name.as_str(), "." | "..")
            || entry
                .name
                .chars()
                .any(|character| character.is_control() || matches!(character, '/' | '\\'))
        {
            return Err(AppError::InvalidInput(
                "Device listing contains a non-component filename".into(),
            ));
        }
        if ignored_remote_name(&entry.name) {
            continue;
        }
        validate_component(&entry.name)?;
        if !names.insert(entry.name.to_lowercase()) {
            return Err(AppError::Conflict("Ambiguous device filenames".into()));
        }
    }
    Ok(entries)
}

async fn ensure_remote_directories(
    device: &WirelessDevice,
    path: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    validate_remote_path(path)?;
    let mut parent = String::from("/");
    for name in path
        .trim_start_matches('/')
        .split('/')
        .filter(|name| !name.is_empty())
    {
        let entries = remote_files(device, &parent, Some(cancel)).await?;
        if let Some(existing) = entries
            .iter()
            .find(|entry| entry.name.to_lowercase() == name.to_lowercase())
        {
            if !existing.is_directory {
                return Err(AppError::Conflict(
                    "Device folder path contains a file".into(),
                ));
            }
            if existing.name != name {
                return Err(AppError::Conflict(
                    "Device folder has different letter case".into(),
                ));
            }
        } else {
            verify_wireless(device, Some(cancel)).await?;
            let response = send(
                device
                    .client
                    .post(endpoint(&device.base, "/mkdir", &[])?)
                    .timeout(STATUS_TIMEOUT)
                    .form(&[("name", name), ("path", parent.as_str())]),
                Some(cancel),
            )
            .await?;
            bounded_reply(response, MAX_REPLY_BYTES, Some(cancel)).await?;
        }
        parent = join_remote(&parent, name);
    }
    Ok(())
}

async fn download_to(
    device: &WirelessDevice,
    path: &str,
    destination: &Path,
    expected_size: u64,
    cancel: Option<&Arc<AtomicBool>>,
) -> Result<String> {
    if expected_size > MAX_FILE_BYTES {
        return Err(AppError::Unsupported("Remote book exceeds 512 MiB".into()));
    }
    validate_remote_path(path)?;
    let mut response = send(
        device
            .client
            .get(endpoint(&device.base, "/download", &[("path", path)])?),
        cancel,
    )
    .await?;
    check_response(&response)?;
    if response
        .content_length()
        .is_some_and(|size| size != expected_size)
    {
        return Err(AppError::Conflict("Remote file size changed".into()));
    }
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .await?;
    let mut bytes = 0_u64;
    let mut digest = Sha256::new();
    while let Some(chunk) = receive_chunk(&mut response, cancel).await? {
        bytes += chunk.len() as u64;
        if bytes > expected_size || bytes > MAX_FILE_BYTES {
            return Err(AppError::Unsupported(
                "Remote download exceeded its declared size".into(),
            ));
        }
        digest.update(&chunk);
        file.write_all(&chunk).await?;
    }
    if bytes != expected_size {
        return Err(AppError::Conflict("Remote download was incomplete".into()));
    }
    file.sync_all().await?;
    Ok(hex_digest(digest.finalize().as_slice()))
}

async fn send(
    request: reqwest::RequestBuilder,
    cancel: Option<&Arc<AtomicBool>>,
) -> Result<Response> {
    let future = request.send();
    tokio::pin!(future);
    loop {
        if let Some(cancel) = cancel {
            check_cancel(cancel)?;
        }
        tokio::select! {
            result = &mut future => return result.map_err(|_| AppError::Network("Device HTTP request failed".into())),
            _ = tokio::time::sleep(Duration::from_millis(100)), if cancel.is_some() => {},
        }
    }
}

async fn receive_chunk(
    response: &mut Response,
    cancel: Option<&Arc<AtomicBool>>,
) -> Result<Option<Vec<u8>>> {
    let future = response.chunk();
    tokio::pin!(future);
    loop {
        if let Some(cancel) = cancel {
            check_cancel(cancel)?;
        }
        tokio::select! {
            result = &mut future => return result.map(|chunk| chunk.map(|bytes| bytes.to_vec())).map_err(|_| AppError::Network("Device response was interrupted".into())),
            _ = tokio::time::sleep(Duration::from_millis(100)), if cancel.is_some() => {},
        }
    }
}

async fn bounded_reply(
    mut response: Response,
    limit: usize,
    cancel: Option<&Arc<AtomicBool>>,
) -> Result<Vec<u8>> {
    check_response(&response)?;
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(AppError::Unsupported(
            "Device response size limit exceeded".into(),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = receive_chunk(&mut response, cancel).await? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(AppError::Unsupported(
                "Device response size limit exceeded".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn check_response(response: &Response) -> Result<()> {
    if response.status().as_u16() == 409 {
        return Err(AppError::Conflict(
            "Device destination already exists".into(),
        ));
    }
    if !response.status().is_success() {
        return Err(AppError::Network(format!(
            "Device HTTP status {}",
            response.status().as_u16()
        )));
    }
    Ok(())
}

fn endpoint(base: &Url, path: &str, query: &[(&str, &str)]) -> Result<Url> {
    let mut url = base
        .join(path)
        .map_err(|_| AppError::InvalidInput("Invalid device endpoint".into()))?;
    url.query_pairs_mut().extend_pairs(query.iter().copied());
    Ok(url)
}

fn validate_address(address: &str, allow_loopback: bool) -> Result<Url> {
    if address.len() > 512 || address.chars().any(char::is_control) {
        return Err(AppError::InvalidInput("Invalid wireless address".into()));
    }
    let mut url = Url::parse(&if address.contains("://") {
        address.into()
    } else {
        format!("http://{address}")
    })
    .map_err(|_| AppError::InvalidInput("Invalid wireless address".into()))?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || (!allow_loopback && url.port_or_known_default() != Some(80))
    {
        return Err(AppError::InvalidInput(
            "Use the reader HTTP address on port 80 without credentials or path".into(),
        ));
    }
    if let Some(host) = url
        .host_str()
        .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
        && !allowed_device_ip(host, allow_loopback)
    {
        return Err(AppError::InvalidInput(
            "Reader address must be on a private LAN".into(),
        ));
    }
    url.set_path("/");
    Ok(url)
}

fn allowed_device_ip(ip: IpAddr, allow_loopback: bool) -> bool {
    if allow_loopback && ip.is_loopback() {
        return true;
    }
    match ip {
        IpAddr::V4(ip) => ip.is_private() && !ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_unique_local(),
    }
}

fn validate_component(name: &str) -> Result<()> {
    if name.is_empty()
        || name.encode_utf16().count() > 255
        || name.starts_with('.')
        || name.ends_with(['.', ' '])
        || name.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
        })
        || matches!(
            name.to_ascii_uppercase()
                .split('.')
                .next()
                .unwrap_or_default(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
    {
        return Err(AppError::InvalidInput(
            "Unsafe or incompatible reader filename".into(),
        ));
    }
    Ok(())
}

fn validate_relative(path: &str) -> Result<()> {
    if path.len() > 2048 || path.starts_with('/') {
        return Err(AppError::InvalidInput(
            "Reader path must be relative".into(),
        ));
    }
    for component in path.split('/') {
        validate_component(component)?;
    }
    Ok(())
}
fn validate_remote_path(path: &str) -> Result<()> {
    if path == "/" {
        return Ok(());
    }
    let relative = path
        .strip_prefix('/')
        .ok_or_else(|| AppError::InvalidInput("Remote path must be absolute".into()))?;
    validate_relative(relative)
}
fn join_remote(parent: &str, name: &str) -> String {
    format!("{}/{name}", parent.trim_end_matches('/'))
}
fn ignored_remote_name(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "XTCache" | "System Volume Information" | "$RECYCLE.BIN" | "lost+found"
        )
}
fn format_for_name(name: &str) -> Option<BookFormat> {
    match Path::new(name)
        .extension()?
        .to_str()?
        .to_ascii_lowercase()
        .as_str()
    {
        "epub" => Some(BookFormat::Epub),
        "mobi" | "prc" | "azw" => Some(BookFormat::Mobi),
        "azw3" => Some(BookFormat::Azw3),
        "fb2" => Some(BookFormat::Fb2),
        "txt" => Some(BookFormat::Txt),
        "html" | "htm" => Some(BookFormat::Html),
        "pdf" => Some(BookFormat::Pdf),
        "cbz" => Some(BookFormat::Cbz),
        _ => None,
    }
}
fn device_accepts(profile: &str, format: BookFormat) -> bool {
    match profile {
        "xteink" => matches!(format, BookFormat::Epub | BookFormat::Txt),
        "kindle" => matches!(
            format,
            BookFormat::Mobi | BookFormat::Azw3 | BookFormat::Pdf | BookFormat::Txt
        ),
        "kobo" => format != BookFormat::Azw3,
        _ => true,
    }
}
fn clean_label(label: &str, fallback: &str) -> String {
    let clean: String = label
        .chars()
        .filter(|character| !character.is_control())
        .take(128)
        .collect();
    if clean.trim().is_empty() {
        fallback.into()
    } else {
        clean.trim().into()
    }
}
fn safe_id(id: &str) -> Result<&str> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AppError::InvalidInput("Unsafe book identifier".into()));
    }
    Ok(id)
}
fn signed_size(size: u64) -> Result<i64> {
    i64::try_from(size)
        .map_err(|_| AppError::InvalidInput("File size is outside SQLite range".into()))
}
fn nonnegative(value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| AppError::InvalidInput("Invalid device inventory count".into()))
}
fn check_cancel(cancel: &Arc<AtomicBool>) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(AppError::Cancelled)
    } else {
        Ok(())
    }
}
fn same_file(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    after.is_file()
        && before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}
fn digest_bytes(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes).as_slice())
}
fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn metadata_from_book(book: &Book) -> BookMetadata {
    BookMetadata {
        title: book.title.clone(),
        authors: book.authors.clone(),
        author_sort: book.author_sort.clone(),
        series: book.series.clone(),
        series_index: book.series_index,
        genres: book.genres.clone(),
        tags: book.tags.clone(),
        language: book.language.clone(),
        description: book.description.clone(),
        isbn: book.isbn.clone(),
        publisher: book.publisher.clone(),
        published: book.published.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct Fixture {
        _temporary: TempDir,
        storage: Storage,
        repository: BookRepository,
        service: TransferService,
    }

    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let database = Database::new(&temporary.path().join("profile")).unwrap();
            let storage = Storage::new(&temporary.path().join("library")).unwrap();
            let repository = BookRepository::new(database.clone());
            let devices = DeviceService::new(database.clone());
            let mut service =
                TransferService::new(database, devices, storage.clone(), repository.clone());
            service.allow_test_endpoints = true;
            Self {
                _temporary: temporary,
                storage,
                repository,
                service,
            }
        }

        fn book(&self, id: &str, bytes: &[u8], format: BookFormat) -> PreparedBook {
            let source = self
                ._temporary
                .path()
                .join(format!("source-{id}.{}", format.as_str()));
            fs::write(&source, bytes).unwrap();
            let stored = self.storage.import_original(&source, format).unwrap();
            let file = StoredFile {
                relative_path: stored.relative_path,
                file: BookFile {
                    id: format!("file-{id}"),
                    book_id: id.into(),
                    format,
                    variant: FileVariant::Original,
                    profile: None,
                    sha256: stored.sha256,
                    size_bytes: stored.size_bytes,
                    created_at: Utc::now().to_rfc3339(),
                },
            };
            self.repository
                .insert(
                    id,
                    BookMetadata {
                        title: "Livre & progrès".into(),
                        authors: vec!["Émile Test".into()],
                        author_sort: "Test, Émile".into(),
                        series: Some("Une série".into()),
                        series_index: Some(3.5),
                        ..Default::default()
                    },
                    &[file],
                    None,
                )
                .unwrap();
            prepare_book(
                &self.storage,
                &self.repository,
                id,
                None,
                "generic",
                &Arc::new(AtomicBool::new(false)),
            )
            .unwrap()
        }

        fn card(&self) -> PathBuf {
            let path = self._temporary.path().join("card");
            fs::create_dir(&path).unwrap();
            path
        }
    }

    #[test]
    fn usb_copy_is_atomic_idempotent_and_refuses_overwrites() {
        let fixture = Fixture::new();
        let book = fixture.book("book-usb", b"Book text", BookFormat::Txt);
        let root = fixture.card();
        let cancel = Arc::new(AtomicBool::new(false));
        let copied = copy_usb_at(&root, &book, &cancel, || Ok(())).unwrap();
        assert_eq!(copied.0, TransferItemStatus::Copied);
        let target = root.join(&book.relative_path);
        assert_eq!(fs::read(&target).unwrap(), b"Book text");
        assert_eq!(
            Storage::hash_file(&book.path).unwrap(),
            book.file.file.sha256
        );
        assert_eq!(
            copy_usb_at(&root, &book, &cancel, || Ok(())).unwrap().0,
            TransferItemStatus::AlreadyPresent
        );
        fs::write(&target, b"Existing progress-sensitive edition").unwrap();
        assert!(matches!(
            copy_usb_at(&root, &book, &cancel, || Ok(())),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(
            fs::read(&target).unwrap(),
            b"Existing progress-sensitive edition"
        );
        assert!(
            !fs::read_dir(target.parent().unwrap())
                .unwrap()
                .any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".library-manager-transfer-"))
        );
    }

    #[test]
    fn usb_disconnect_cancel_and_symlinks_leave_no_partial_book() {
        let fixture = Fixture::new();
        let book = fixture.book("book-disconnect", b"Book text", BookFormat::Txt);
        let root = fixture.card();
        let calls = Cell::new(0);
        let result = copy_usb_at(&root, &book, &Arc::new(AtomicBool::new(false)), || {
            calls.set(calls.get() + 1);
            if calls.get() == 4 {
                Err(AppError::NotFound("Disconnected fixture".into()))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert!(!root.join(&book.relative_path).exists());
        let parent = root
            .join(&book.relative_path)
            .parent()
            .unwrap()
            .to_path_buf();
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
        assert!(matches!(
            copy_usb_at(&root, &book, &Arc::new(AtomicBool::new(true)), || Ok(())),
            Err(AppError::Cancelled)
        ));
        let link = fixture._temporary.path().join("linked-card");
        symlink(&root, &link).unwrap();
        assert!(copy_usb_at(&link, &book, &Arc::new(AtomicBool::new(false)), || Ok(())).is_err());
        let outside = fixture._temporary.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::remove_dir_all(root.join("Books")).unwrap();
        symlink(&outside, root.join("Books")).unwrap();
        assert!(copy_usb_at(&root, &book, &Arc::new(AtomicBool::new(false)), || Ok(())).is_err());
        assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    }

    #[test]
    fn address_and_remote_names_are_validated_before_network_access() {
        for address in [
            "http://127.0.0.1",
            "http://8.8.8.8",
            "http://169.254.169.254",
            "http://192.168.1.4:8080",
            "https://192.168.1.4",
            "http://user:secret@192.168.1.4",
            "http://192.168.1.4/path",
            "http://192.168.1.4?key=value",
        ] {
            assert!(validate_address(address, false).is_err(), "{address}");
        }
        assert!(validate_address("192.168.1.4", false).is_ok());
        assert!(validate_address("crosspoint.local", false).is_ok());
        for name in [
            "../private",
            "folder/file",
            "back\\slash",
            "CON.epub",
            "trailing.",
            ".crosspoint",
            "bad\nname",
        ] {
            assert!(validate_component(name).is_err(), "{name}");
        }
        let base = Url::parse("http://192.168.1.4/").unwrap();
        let url = endpoint(&base, "/api/files", &[("path", "/Books/Émile & livres")]).unwrap();
        assert_eq!(url.query_pairs().next().unwrap().1, "/Books/Émile & livres");
        assert!(device_accepts("xteink", BookFormat::Epub));
        assert!(!device_accepts("xteink", BookFormat::Mobi));
        assert!(!device_accepts("kindle", BookFormat::Epub));
        assert!(device_accepts("kindle", BookFormat::Azw3));
    }

    #[derive(Default)]
    struct ServerState {
        files: BTreeMap<String, Vec<u8>>,
        folders: BTreeSet<String>,
        requests: Vec<(String, String)>,
        fail_rename: bool,
        corrupt_upload: bool,
        redirect_status: bool,
        unsafe_listing: bool,
    }

    struct Server {
        address: String,
        state: Arc<Mutex<ServerState>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Server {
        async fn new() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = format!("http://{}/", listener.local_addr().unwrap());
            let state = Arc::new(Mutex::new(ServerState {
                folders: BTreeSet::from(["/".into()]),
                ..Default::default()
            }));
            let owned = state.clone();
            let task = tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 8192];
                    let header_end = loop {
                        let count = socket.read(&mut buffer).await.unwrap();
                        if count == 0 {
                            break None;
                        }
                        request.extend_from_slice(&buffer[..count]);
                        if let Some(index) = find_bytes(&request, b"\r\n\r\n") {
                            break Some(index + 4);
                        }
                        assert!(request.len() < MAX_JSON_BYTES);
                    };
                    let Some(header_end) = header_end else {
                        continue;
                    };
                    let header = String::from_utf8(request[..header_end].to_vec()).unwrap();
                    let content_length = header
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                        .unwrap_or(0);
                    while request.len() < header_end + content_length {
                        let count = socket.read(&mut buffer).await.unwrap();
                        assert!(count > 0);
                        request.extend_from_slice(&buffer[..count]);
                    }
                    let (status, body, extra) = {
                        let mut state = owned.lock().unwrap();
                        route(
                            &header,
                            &request[header_end..header_end + content_length],
                            &mut state,
                        )
                    };
                    let reply = format!(
                        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
                        body.len()
                    );
                    socket.write_all(reply.as_bytes()).await.unwrap();
                    socket.write_all(&body).await.unwrap();
                }
            });
            Self {
                address,
                state,
                task,
            }
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
    fn route(header: &str, body: &[u8], state: &mut ServerState) -> (u16, Vec<u8>, String) {
        let mut first = header.lines().next().unwrap().split_whitespace();
        let method = first.next().unwrap();
        let request_path = first.next().unwrap();
        let url = Url::parse(&format!("http://fixture{request_path}")).unwrap();
        let query: BTreeMap<String, String> = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        state.requests.push((method.into(), url.path().into()));
        let form: BTreeMap<String, String> = url::form_urlencoded::parse(body)
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        let reply = match (method, url.path()) {
            ("GET", "/api/status") => {
                if state.redirect_status {
                    return (302, Vec::new(), "Location: http://8.8.8.8/\r\n".into());
                }
                serde_json::to_vec(&serde_json::json!({"version":"fixture","mode":"STA","device":"X4","deviceId":"a".repeat(64)})).unwrap()
            }
            ("GET", "/api/files") => {
                let parent = query.get("path").map(String::as_str).unwrap_or("/");
                if !state.folders.contains(parent) {
                    return (404, Vec::new(), String::new());
                }
                if state.unsafe_listing {
                    return (
                        200,
                        br#"[{"name":"../escape","size":0,"isDirectory":false}]"#.to_vec(),
                        String::new(),
                    );
                }
                let mut files = Vec::new();
                for (path, bytes) in &state.files {
                    let (directory, name) = path.rsplit_once('/').unwrap();
                    let directory = if directory.is_empty() { "/" } else { directory };
                    if directory == parent {
                        files.push(
                            serde_json::json!({"name":name,"size":bytes.len(),"isDirectory":false}),
                        );
                    }
                }
                for path in &state.folders {
                    if path == "/" {
                        continue;
                    }
                    let (directory, name) = path.rsplit_once('/').unwrap();
                    let directory = if directory.is_empty() { "/" } else { directory };
                    if directory == parent {
                        files.push(serde_json::json!({"name":name,"size":0,"isDirectory":true}));
                    }
                }
                serde_json::to_vec(&files).unwrap()
            }
            ("GET", "/download") => match state.files.get(&query["path"]) {
                Some(bytes) => bytes.clone(),
                None => return (404, Vec::new(), String::new()),
            },
            ("POST", "/mkdir") => {
                state
                    .folders
                    .insert(join_remote(&form["path"], &form["name"]));
                b"Created".to_vec()
            }
            ("POST", "/upload") => {
                let part_header_end = find_bytes(body, b"\r\n\r\n").unwrap();
                let part_header = String::from_utf8(body[..part_header_end].to_vec()).unwrap();
                let filename = part_header
                    .split("filename=\"")
                    .nth(1)
                    .unwrap()
                    .split('"')
                    .next()
                    .unwrap();
                let boundary = header
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                    .unwrap()
                    .1
                    .split("boundary=")
                    .nth(1)
                    .unwrap()
                    .trim();
                let data = &body[part_header_end + 4..];
                let end = find_bytes(data, format!("\r\n--{boundary}").as_bytes()).unwrap();
                let path = join_remote(&query["path"], filename);
                if state.files.contains_key(&path) {
                    return (409, Vec::new(), String::new());
                }
                let mut bytes = data[..end].to_vec();
                if state.corrupt_upload {
                    bytes.push(b'!');
                }
                state.files.insert(path, bytes);
                b"Uploaded".to_vec()
            }
            ("POST", "/rename") => {
                if state.fail_rename {
                    return (500, b"Rename failed".to_vec(), String::new());
                }
                let source = &form["path"];
                let parent = source.rsplit_once('/').unwrap().0;
                let destination = join_remote(parent, &form["name"]);
                if state.files.contains_key(&destination) {
                    return (409, Vec::new(), String::new());
                }
                let bytes = state.files.remove(source).unwrap();
                state.files.insert(destination, bytes);
                b"Renamed".to_vec()
            }
            ("POST", "/delete") => {
                state.files.remove(&form["path"]);
                b"Deleted".to_vec()
            }
            _ => return (404, Vec::new(), String::new()),
        };
        (200, reply, String::new())
    }

    #[tokio::test]
    async fn crosspoint_stages_verifies_renames_and_preserves_existing_content() {
        let fixture = Fixture::new();
        let book = fixture.book("book-wireless", b"Wireless book", BookFormat::Txt);
        let server = Server::new().await;
        let device = fixture
            .service
            .connect_wireless(
                &server.address,
                DeviceTransport::Crosspoint,
                "My reader\n",
                None,
            )
            .await
            .unwrap();
        assert_eq!(device.label, "My reader");
        assert!(device.connected);
        let report = fixture
            .service
            .transfer(&device.id, std::slice::from_ref(&book.book.id), None)
            .await
            .unwrap();
        assert_eq!((report.copied, report.failed), (1, 0));
        let target = format!("/{}", book.relative_path);
        {
            let state = server.state.lock().unwrap();
            assert_eq!(state.files[&target], b"Wireless book");
            assert_eq!(state.files.len(), 1);
            assert!(state.requests.iter().any(|(_, path)| path == "/upload"));
            assert!(state.requests.iter().any(|(_, path)| path == "/rename"));
        }
        assert_eq!(
            fixture
                .service
                .transfer(&device.id, std::slice::from_ref(&book.book.id), None)
                .await
                .unwrap()
                .skipped,
            1
        );
        server
            .state
            .lock()
            .unwrap()
            .files
            .insert(target.clone(), b"Existing edition".to_vec());
        let report = fixture
            .service
            .transfer(&device.id, std::slice::from_ref(&book.book.id), None)
            .await
            .unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(
            server.state.lock().unwrap().files[&target],
            b"Existing edition"
        );
        assert_eq!(
            fixture
                .service
                .index_wireless(&device.id)
                .await
                .unwrap()
                .len(),
            1
        );
        fixture.service.disconnect(&device.id).unwrap();
        assert!(!fixture.service.scan_wireless().await.unwrap()[0].connected);
        assert!(
            fixture
                .service
                .connected_wireless_ids()
                .await
                .unwrap()
                .is_empty()
        );
        let new_service = TransferService {
            state: Arc::new(Mutex::new(WirelessState::default())),
            ..fixture.service.clone()
        };
        assert!(new_service.scan_wireless().await.unwrap()[0].connected);
        let json = fixture
            .service
            .database
            .connect()
            .unwrap()
            .query_row(
                "SELECT value_json FROM settings WHERE key LIKE 'wirelessDevice:%'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert!(!json.contains("password"));
        assert!(!json.contains("secret"));
    }

    #[tokio::test]
    async fn wireless_failure_rolls_back_staging_and_existing_path_is_kept() {
        let fixture = Fixture::new();
        let book = fixture.book("book-rollback", b"Wireless book", BookFormat::Txt);
        let server = Server::new().await;
        let device = fixture
            .service
            .connect_wireless(&server.address, DeviceTransport::Crosspoint, "Reader", None)
            .await
            .unwrap();
        server.state.lock().unwrap().fail_rename = true;
        assert_eq!(
            fixture
                .service
                .transfer(&device.id, std::slice::from_ref(&book.book.id), None)
                .await
                .unwrap()
                .failed,
            1
        );
        assert!(server.state.lock().unwrap().files.is_empty());
        server.state.lock().unwrap().fail_rename = false;
        server.state.lock().unwrap().corrupt_upload = true;
        assert_eq!(
            fixture
                .service
                .transfer(&device.id, std::slice::from_ref(&book.book.id), None)
                .await
                .unwrap()
                .failed,
            1
        );
        assert!(server.state.lock().unwrap().files.is_empty());
        server.state.lock().unwrap().corrupt_upload = false;
        server
            .state
            .lock()
            .unwrap()
            .files
            .insert("/Legacy.txt".into(), b"Wireless book".to_vec());
        let index = fixture.service.index_wireless(&device.id).await.unwrap();
        assert_eq!(index[0].book_id.as_deref(), Some("book-rollback"));
        let report = fixture
            .service
            .transfer(&device.id, std::slice::from_ref(&book.book.id), None)
            .await
            .unwrap();
        assert_eq!(report.skipped, 1);
        assert_eq!(report.items[0].relative_path, "Legacy.txt");
        assert_eq!(server.state.lock().unwrap().files.len(), 1);
    }

    #[tokio::test]
    async fn remote_redirects_unsafe_paths_and_false_protocols_are_refused() {
        let fixture = Fixture::new();
        let server = Server::new().await;
        assert!(matches!(
            fixture
                .service
                .connect_wireless(
                    &server.address,
                    DeviceTransport::CalibreWireless,
                    "Reader",
                    None
                )
                .await,
            Err(AppError::Unsupported(_))
        ));
        assert!(matches!(
            fixture
                .service
                .connect_wireless(
                    &server.address,
                    DeviceTransport::Crosspoint,
                    "Reader",
                    Some("secret")
                )
                .await,
            Err(AppError::Unsupported(_))
        ));
        server.state.lock().unwrap().redirect_status = true;
        assert!(
            fixture
                .service
                .connect_wireless(&server.address, DeviceTransport::Crosspoint, "Reader", None)
                .await
                .is_err()
        );
        server.state.lock().unwrap().redirect_status = false;
        let device = fixture
            .service
            .connect_wireless(&server.address, DeviceTransport::Crosspoint, "Reader", None)
            .await
            .unwrap();
        server.state.lock().unwrap().unsafe_listing = true;
        assert!(fixture.service.index_wireless(&device.id).await.is_err());
        assert!(
            fixture
                .service
                .devices
                .inventory(&device.id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn operation_guard_serializes_jobs_and_releases_after_cancellation() {
        let fixture = Fixture::new();
        let operation = fixture.service.begin_operation("fixture").unwrap();
        assert!(matches!(
            fixture.service.begin_operation("fixture"),
            Err(AppError::Conflict(_))
        ));
        fixture.service.cancel("fixture").unwrap();
        assert!(check_cancel(&operation.flag).is_err());
        drop(operation);
        assert!(fixture.service.begin_operation("fixture").is_ok());
        let book = fixture.book("mobi-input", b"mobi fixture", BookFormat::Mobi);
        assert!(
            prepare_book(
                &fixture.storage,
                &fixture.repository,
                &book.book.id,
                None,
                "xteink",
                &Arc::new(AtomicBool::new(false))
            )
            .is_err()
        );
        assert!(
            prepare_book(
                &fixture.storage,
                &fixture.repository,
                &book.book.id,
                None,
                "kindle",
                &Arc::new(AtomicBool::new(false))
            )
            .is_ok()
        );
    }
    #[test]
    fn optimization_keeps_original_and_reuses_the_verified_variant() {
        let fixture = Fixture::new();
        let entries=BTreeMap::from([
            ("mimetype".into(),b"application/epub+zip".to_vec()),
            ("META-INF/container.xml".into(),br#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.to_vec()),
            ("content.opf".into(),br#"<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">urn:test:transfer</dc:identifier><dc:title>Transfer</dc:title><dc:creator>Writer</dc:creator><dc:language>en</dc:language></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#.to_vec()),
            ("chapter.xhtml".into(),br#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Chapter</title></head><body><p>Text must remain intact during optimization.</p></body></html>"#.to_vec())
        ]);
        let input = fixture._temporary.path().join("input.epub");
        EpubDocument::from_entries(entries.clone(), "Transfer").unwrap();
        let mut archive = zip::ZipWriter::new(File::create(&input).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        archive.start_file("mimetype", options).unwrap();
        archive.write_all(&entries["mimetype"]).unwrap();
        for (path, bytes) in &entries {
            if path == "mimetype" {
                continue;
            }
            archive.start_file(path, options).unwrap();
            archive.write_all(bytes).unwrap();
        }
        archive.finish().unwrap();
        let original = fixture.book(
            "book-optimized",
            &fs::read(input).unwrap(),
            BookFormat::Epub,
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let optimized = prepare_book(
            &fixture.storage,
            &fixture.repository,
            &original.book.id,
            Some("lossless"),
            "xteink",
            &cancel,
        )
        .unwrap();
        assert_eq!(optimized.file.file.variant, FileVariant::Optimized);
        assert_eq!(optimized.file.file.profile.as_deref(), Some("lossless"));
        assert_eq!(
            Storage::hash_file(&original.path).unwrap(),
            original.file.file.sha256
        );
        assert_eq!(
            EpubDocument::open(&original.path)
                .unwrap()
                .text_fingerprint()
                .unwrap(),
            EpubDocument::open(&optimized.path)
                .unwrap()
                .text_fingerprint()
                .unwrap()
        );
        let second = prepare_book(
            &fixture.storage,
            &fixture.repository,
            &original.book.id,
            Some("lossless"),
            "xteink",
            &cancel,
        )
        .unwrap();
        assert_eq!(optimized.file.file.id, second.file.file.id);
        assert_eq!(
            fixture.repository.files(&original.book.id).unwrap().len(),
            2
        );
    }
    #[tokio::test]
    async fn public_async_operations_can_run_as_background_jobs() {
        fn send<T: Send>(future: T) -> T {
            future
        }
        let fixture = Fixture::new();
        assert!(
            send(fixture.service.scan_wireless())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            send(fixture.service.index_wireless("missing"))
                .await
                .is_err()
        );
        assert!(
            send(
                fixture
                    .service
                    .transfer("missing", &["missing".into()], None)
            )
            .await
            .is_err()
        );
    }
}
