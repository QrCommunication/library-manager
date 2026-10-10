//! Discovery and read-only indexing of already mounted ebook devices.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Take};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::Utc;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
#[cfg(unix)]
use rustix::fs::{Access, access, statvfs};
use serde::Serialize;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::database::Database;
#[cfg(test)]
use crate::epub::EpubDocument;
use crate::error::{AppError, Result};
use crate::models::{BookFormat, Device};
use crate::secure_fs::{self, AccessPolicy, FileIdentity, FileSnapshot, SecureDir};
use crate::storage::MAX_FILE_BYTES;
#[cfg(test)]
use crate::storage::Storage;

const MAX_MOUNTINFO_BYTES: u64 = 4 * 1024 * 1024;
const MAX_MOUNTS: usize = 4_096;
const MAX_INDEX_ENTRIES: usize = 100_000;
const MAX_INDEX_BOOKS: usize = 20_000;
const MAX_INDEX_DEPTH: usize = 32;
const MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_METADATA_CHARS: usize = 4_096;
const MAX_AUTHORS: usize = 128;

/// Internal inventory record. Absolute paths are kept in backend capabilities.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedDeviceBook {
    pub device_id: String,
    pub relative_path: String,
    pub book_id: Option<String>,
    pub sha256: Option<String>,
    pub title: String,
    pub authors: Vec<String>,
    pub format: BookFormat,
    pub size_bytes: u64,
    pub last_seen_at: String,
    pub warnings: Vec<String>,
}

/// Counters are measured from enumeration and actual file reads, never elapsed time.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceIndexProgress {
    pub phase: String,
    pub visited_entries: u64,
    pub processed_books: u64,
    pub total_books: u64,
    pub bytes_read: u64,
    pub total_bytes: u64,
    pub current_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInventoryPage {
    pub items: Vec<IndexedDeviceBook>,
    pub total: u64,
    pub offset: u64,
    pub limit: u64,
}

#[derive(Debug)]
struct InventorySnapshot {
    capability: MountCapability,
    books: Vec<IndexedDeviceBook>,
    versions: BTreeMap<String, FileSnapshot>,
    indexing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MountEntry {
    mount_id: String,
    root: String,
    mount_point: PathBuf,
    options: Vec<String>,
    filesystem: String,
    source: String,
}

type RootIdentity = FileIdentity;

#[derive(Clone, Debug)]
struct MountCapability {
    id: String,
    label: String,
    profile: String,
    mount: MountEntry,
    path: PathBuf,
    identity: RootIdentity,
    stable_identity: String,
    writable: bool,
    total_bytes: Option<u64>,
}

impl MountCapability {
    fn same_connection(&self, other: &Self) -> bool {
        self.mount == other.mount
            && self.path == other.path
            && self.identity == other.identity
            && self.stable_identity == other.stable_identity
    }
}

#[derive(Debug)]
struct MountSource {
    native: bool,
    mountinfo: PathBuf,
    media_roots: Vec<PathBuf>,
    gvfs_roots: Vec<PathBuf>,
}

/// Probes only mounted volumes; discovery never mounts or modifies a device.
#[derive(Clone, Debug)]
pub struct DeviceService {
    database: Database,
    source: Arc<MountSource>,
    active: Arc<Mutex<BTreeMap<String, MountCapability>>>,
    inventories: Arc<Mutex<BTreeMap<String, InventorySnapshot>>>,
}

impl DeviceService {
    pub fn new(database: Database) -> Self {
        #[cfg(unix)]
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| {
                PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw()))
            });
        #[cfg(not(unix))]
        let runtime = PathBuf::new();
        Self {
            database,
            source: Arc::new(MountSource {
                native: cfg!(any(windows, target_os = "macos")),
                mountinfo: PathBuf::from("/proc/self/mountinfo"),
                media_roots: vec![PathBuf::from("/media"), PathBuf::from("/run/media")],
                gvfs_roots: vec![runtime.join("gvfs")],
            }),
            active: Arc::new(Mutex::new(BTreeMap::new())),
            inventories: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Returns live devices and remembered devices with `connected = false`.
    /// Cached presence is invalidated on a new mount session until indexing ends.
    pub fn scan(&self) -> Result<Vec<Device>> {
        let current = self.discover()?;
        let timestamp = Utc::now().to_rfc3339();
        let mut active = self.lock_active()?;
        reject_replaced_roots(&active, &current)?;
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for capability in current.values() {
            if !active
                .get(&capability.id)
                .is_some_and(|previous| previous.same_connection(capability))
            {
                transaction.execute(
                    "DELETE FROM device_books WHERE device_id = ?1",
                    [&capability.id],
                )?;
            }
            transaction.execute(
                "INSERT INTO devices(id, label, transport, profile, mount_identity, last_seen_at)
                 VALUES (?1, ?2, 'usb', ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET label = excluded.label,
                    transport = excluded.transport, profile = excluded.profile,
                    mount_identity = excluded.mount_identity, last_seen_at = excluded.last_seen_at",
                params![
                    capability.id,
                    capability.label,
                    capability.profile,
                    capability.stable_identity,
                    timestamp
                ],
            )?;
        }
        let mut statement = transaction.prepare(
            "SELECT d.id, d.label, d.transport, d.profile, d.last_seen_at,
                (SELECT count(*) FROM device_books b WHERE b.device_id = d.id),
                (SELECT count(*) FROM device_books b WHERE b.device_id = d.id AND b.book_id IS NOT NULL)
             FROM devices d ORDER BY d.label COLLATE NOCASE, d.id",
        )?;
        let mut devices = Vec::new();
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let capability = current.get(&id);
            let (total_bytes, free_bytes) = capability
                .map(|mounted| {
                    let (total, free) = volume_capacity(&mounted.path);
                    (mounted.total_bytes.or(total), free)
                })
                .unwrap_or((None, None));
            let transport: String = row.get(2)?;
            devices.push(Device {
                id,
                label: row.get(1)?,
                transport: transport.parse().map_err(AppError::InvalidInput)?,
                connected: capability.is_some(),
                writable: capability.is_some_and(|mounted| mounted.writable),
                profile: row.get(3)?,
                mount_path: capability.map(|mounted| mounted.path.to_string_lossy().into_owned()),
                address: None,
                total_bytes,
                free_bytes,
                book_count: positive_integer(row.get(5)?)?,
                matched_book_count: positive_integer(row.get(6)?)?,
                last_seen_at: row.get(4)?,
            });
        }
        drop(rows);
        drop(statement);
        transaction.commit()?;
        self.lock_inventories()?.retain(|id, snapshot| {
            current
                .get(id)
                .is_none_or(|mounted| snapshot.capability.same_connection(mounted))
        });
        *active = current;
        Ok(devices)
    }

    pub fn connected_ids(&self) -> Result<Vec<String>> {
        Ok(self
            .scan()?
            .into_iter()
            .filter(|device| device.connected)
            .map(|device| device.id)
            .collect())
    }

    /// Resolves a known device ID only after rereading the mount table and root.
    /// Transfer services must revalidate immediately before each publication.
    pub fn resolve_connected(&self, id: &str) -> Result<PathBuf> {
        let capability = self.connected_capability(id)?;
        require_root(&capability.path, &capability.identity)?;
        Ok(capability.path)
    }

    /// Compatibility wrapper for callers that do not need live progress.
    pub fn index(&self, id: &str) -> Result<Vec<IndexedDeviceBook>> {
        self.index_with_progress(id, Arc::new(AtomicBool::new(false)), |_| Ok(()))
    }

    /// Enumerates first, fixing the read denominator before hashing any book.
    /// Completed records become visible without exposing a partially written DB.
    pub fn index_with_progress<F>(
        &self,
        id: &str,
        cancelled: Arc<AtomicBool>,
        mut callback: F,
    ) -> Result<Vec<IndexedDeviceBook>>
    where
        F: FnMut(DeviceIndexProgress) -> Result<()>,
    {
        let capability = self.connected_capability(id)?;
        {
            let mut inventories = self.lock_inventories()?;
            if inventories
                .get(id)
                .is_some_and(|snapshot| snapshot.indexing)
            {
                return Err(AppError::Conflict(
                    "Device inventory is already running".into(),
                ));
            }
            inventories.insert(
                id.into(),
                InventorySnapshot {
                    capability: capability.clone(),
                    books: Vec::new(),
                    versions: BTreeMap::new(),
                    indexing: true,
                },
            );
        }
        let outcome = self.perform_index(&capability, &cancelled, &mut callback);
        let mut inventories = self.lock_inventories()?;
        if inventories
            .get(id)
            .is_some_and(|snapshot| snapshot.capability.same_connection(&capability))
        {
            if outcome.is_err() {
                inventories.remove(id);
            } else if let Some(snapshot) = inventories.get_mut(id) {
                snapshot.indexing = false;
            }
        }
        outcome
    }

    fn perform_index<F>(
        &self,
        capability: &MountCapability,
        cancelled: &AtomicBool,
        callback: &mut F,
    ) -> Result<Vec<IndexedDeviceBook>>
    where
        F: FnMut(DeviceIndexProgress) -> Result<()>,
    {
        let id = capability.id.as_str();
        let timestamp = Utc::now().to_rfc3339();
        let mut progress = DeviceIndexProgress {
            phase: "discovering".into(),
            visited_entries: 0,
            processed_books: 0,
            total_books: 0,
            bytes_read: 0,
            total_bytes: 0,
            current_path: None,
        };
        check_cancelled(cancelled)?;
        callback(progress.clone())?;
        let mut discovered = Vec::new();
        let mut discovered_bytes = 0_u64;
        let walker = WalkDir::new(&capability.path)
            .follow_links(false)
            .same_file_system(true)
            .max_depth(MAX_INDEX_DEPTH)
            .into_iter()
            .filter_entry(|entry| !ignored_directory(entry.path(), &capability.path));
        for entry in walker {
            check_cancelled(cancelled)?;
            let entry = entry.map_err(|error| {
                AppError::Io(error.into_io_error().unwrap_or_else(|| {
                    std::io::Error::other("Cannot enumerate the mounted device")
                }))
            })?;
            progress.visited_entries += 1;
            if progress.visited_entries > MAX_INDEX_ENTRIES as u64 {
                return Err(AppError::Unsupported(
                    "Device inventory entry limit exceeded".into(),
                ));
            }
            if entry.file_type().is_dir() && entry.depth() == MAX_INDEX_DEPTH {
                return Err(AppError::Unsupported(
                    "Device folder depth limit exceeded".into(),
                ));
            }
            progress.current_path = Some(
                entry
                    .path()
                    .strip_prefix(&capability.path)
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .into_owned(),
            );
            callback(progress.clone())?;
            if !entry.file_type().is_file() {
                continue;
            }
            let Some(format) = format_for_path(entry.path()) else {
                continue;
            };
            if discovered.len() == MAX_INDEX_BOOKS {
                return Err(AppError::Unsupported(
                    "Device inventory book limit exceeded".into(),
                ));
            }
            require_root(&capability.path, &capability.identity)?;
            validate_contained_file(entry.path(), &capability.path)?;
            let metadata = device_snapshot(entry.path(), &capability.path)?;
            discovered_bytes = discovered_bytes
                .checked_add(metadata.size)
                .filter(|bytes| *bytes <= MAX_INDEX_BYTES)
                .ok_or_else(|| {
                    AppError::Unsupported("Device inventory byte limit exceeded".into())
                })?;
            if metadata.size <= MAX_FILE_BYTES {
                progress.total_bytes += metadata.size;
            }
            discovered.push((entry.path().to_owned(), format, metadata));
            progress.total_books = discovered.len() as u64;
        }
        progress.phase = "reading".into();
        progress.current_path = None;
        callback(progress.clone())?;
        let connection = self.database.connect()?;
        let mut indexed = Vec::with_capacity(discovered.len());
        for (path, format, metadata) in discovered {
            check_cancelled(cancelled)?;
            require_root(&capability.path, &capability.identity)?;
            validate_contained_file(&path, &capability.path)?;
            if metadata != device_snapshot(&path, &capability.path)? {
                return Err(AppError::Conflict(
                    "A device file changed during indexing".into(),
                ));
            }
            let relative_path = relative_utf8_path(&path, &capability.path)?;
            progress.current_path = Some(relative_path.clone());
            let size_bytes = metadata.size;
            let mut warnings = Vec::new();
            let sha256 = if size_bytes <= MAX_FILE_BYTES {
                let mut file = open_device_file(&path, &capability.path)?;
                metadata.verify(&file)?;
                let mut hash = Sha256::new();
                let mut buffer = vec![0_u8; 256 * 1024];
                let mut file_bytes = 0_u64;
                loop {
                    check_cancelled(cancelled)?;
                    let read = file.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    file_bytes += read as u64;
                    if file_bytes > size_bytes {
                        return Err(AppError::Conflict(
                            "A device file changed during indexing".into(),
                        ));
                    }
                    hash.update(&buffer[..read]);
                    progress.bytes_read += read as u64;
                    callback(progress.clone())?;
                }
                if file_bytes != size_bytes {
                    return Err(AppError::Conflict(
                        "A device file changed during indexing".into(),
                    ));
                }
                metadata.verify(&file)?;
                Some(
                    hash.finalize()
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>(),
                )
            } else {
                warnings.push("fileTooLarge".into());
                None
            };
            let mut title = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut authors = Vec::new();
            if format == BookFormat::Epub && size_bytes <= MAX_FILE_BYTES {
                check_cancelled(cancelled)?;
                let file = open_device_file(&path, &capability.path)?;
                metadata.verify(&file)?;
                let verify_file = file.try_clone()?;
                match crate::epub::read_inventory_metadata_file(file, &title) {
                    Ok(metadata) => {
                        title = metadata.title;
                        authors = metadata.authors;
                    }
                    Err(_) => warnings.push("metadataUnavailable".into()),
                }
                metadata.verify(&verify_file)?;
            }
            if metadata != device_snapshot(&path, &capability.path)? {
                return Err(AppError::Conflict(
                    "A device file changed during indexing".into(),
                ));
            }
            authors.truncate(MAX_AUTHORS);
            let book_id = sha256
                .as_ref()
                .map(|hash| {
                    connection
                        .query_row(
                            "SELECT book_id FROM book_files WHERE sha256 = ?1",
                            [hash],
                            |row| row.get(0),
                        )
                        .optional()
                })
                .transpose()?
                .flatten();
            let book = IndexedDeviceBook {
                device_id: id.into(),
                relative_path: relative_path.clone(),
                book_id,
                sha256,
                title: bounded_text(&title),
                authors: authors
                    .into_iter()
                    .map(|author| bounded_text(&author))
                    .collect(),
                format,
                size_bytes,
                last_seen_at: timestamp.clone(),
                warnings,
            };
            {
                let mut inventories = self.lock_inventories()?;
                let snapshot = inventories
                    .get_mut(id)
                    .ok_or_else(|| AppError::Conflict("Device inventory was invalidated".into()))?;
                if !snapshot.capability.same_connection(capability) {
                    return Err(AppError::Conflict(
                        "Device inventory connection changed".into(),
                    ));
                }
                snapshot.versions.insert(relative_path, metadata);
                snapshot.books.push(book.clone());
            }
            indexed.push(book);
            progress.processed_books += 1;
            callback(progress.clone())?;
        }
        check_cancelled(cancelled)?;
        self.revalidate_connection(capability)?;
        progress.phase = "finalizing".into();
        progress.current_path = None;
        callback(progress.clone())?;
        check_cancelled(cancelled)?;
        drop(connection);
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("DELETE FROM device_books WHERE device_id = ?1", [id])?;
        for book in &mut indexed {
            check_cancelled(cancelled)?;
            if let Some(hash) = &book.sha256 {
                book.book_id = transaction
                    .query_row(
                        "SELECT book_id FROM book_files WHERE sha256 = ?1",
                        [hash],
                        |row| row.get(0),
                    )
                    .optional()?;
            }
            transaction.execute(
                "INSERT INTO device_books(device_id, relative_path, book_id, sha256, title,
                    authors_json, format, size_bytes, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id,
                    book.relative_path,
                    book.book_id,
                    book.sha256,
                    book.title,
                    serde_json::to_string(&book.authors)?,
                    book.format.as_str(),
                    i64::try_from(book.size_bytes).map_err(|_| AppError::InvalidInput(
                        "Device file size is outside SQLite range".into()
                    ))?,
                    timestamp
                ],
            )?;
        }
        self.revalidate_connection(capability)?;
        check_cancelled(cancelled)?;
        transaction.commit()?;
        Ok(indexed)
    }

    /// Updates presence using backend-owned local hashes after a successful import.
    pub fn reconcile_import(&self, device_id: &str, book_id: &str) -> Result<()> {
        let connection = self.database.connect()?;
        connection.execute(
            "UPDATE device_books SET book_id = ?2 WHERE device_id = ?1
                AND sha256 IN (SELECT sha256 FROM book_files WHERE book_id = ?2)",
            params![device_id, book_id],
        )?;
        Ok(())
    }

    pub fn inventory_page(
        &self,
        id: &str,
        offset: u64,
        limit: u64,
        unknown_only: bool,
    ) -> Result<DeviceInventoryPage> {
        let mut books = self.inventory(id)?;
        if unknown_only {
            books.retain(|book| book.book_id.is_none());
        }
        let total = books.len() as u64;
        let offset_index = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(books.len());
        let count = usize::try_from(limit.min(MAX_INDEX_BOOKS as u64)).unwrap_or(MAX_INDEX_BOOKS);
        let items = books.into_iter().skip(offset_index).take(count).collect();
        Ok(DeviceInventoryPage {
            items,
            total,
            offset,
            limit: limit.min(MAX_INDEX_BOOKS as u64),
        })
    }

    /// Resolves only an inventoried, unchanged file on the same live connection.
    pub fn resolve_import_path(&self, id: &str, relative_path: &str) -> Result<PathBuf> {
        let relative = Path::new(relative_path);
        if relative_path.is_empty()
            || !relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            return Err(AppError::InvalidInput("Unsafe device import path".into()));
        }
        let capability = self.connected_capability(id)?;
        let path = capability.path.join(relative);
        validate_contained_file(&path, &capability.path)?;
        let current = device_snapshot(&path, &capability.path)?;
        let remembered = {
            let inventories = self.lock_inventories()?;
            inventories
                .get(id)
                .map(|snapshot| {
                    if !snapshot.capability.same_connection(&capability) {
                        return Err(AppError::Conflict(
                            "Device connection changed since inventory".into(),
                        ));
                    }
                    let version = snapshot.versions.get(relative_path).ok_or_else(|| {
                        AppError::NotFound("File is absent from the device inventory".into())
                    })?;
                    Ok(*version == current)
                })
                .transpose()?
        };
        if remembered == Some(false) {
            return Err(AppError::Conflict(
                "Device file changed since inventory".into(),
            ));
        }
        // A restarted process has persisted hashes but no in-memory inode version.
        if remembered.is_none() {
            let connection = self.database.connect()?;
            let record: Option<(Option<String>, i64)> = connection.query_row(
                "SELECT sha256, size_bytes FROM device_books WHERE device_id = ?1 AND relative_path = ?2",
                params![id, relative_path], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            let (hash, size) = record.ok_or_else(|| {
                AppError::NotFound("File is absent from the device inventory".into())
            })?;
            let expected = hash.ok_or_else(|| {
                AppError::Unsupported("This file cannot be verified for import".into())
            })?;
            if current.size != positive_integer(size)?
                || crate::storage::Storage::hash_file(&path)? != expected
                || current != device_snapshot(&path, &capability.path)?
            {
                return Err(AppError::Conflict(
                    "Device file changed since inventory".into(),
                ));
            }
        }
        self.revalidate_connection(&capability)?;
        Ok(path)
    }

    /// Cached inventory remains available for an offline device; it is not presence.
    pub fn inventory(&self, id: &str) -> Result<Vec<IndexedDeviceBook>> {
        let connection = self.database.connect()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM devices WHERE id = ?1)",
            [id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(AppError::NotFound("Unknown device".into()));
        }
        let staged = {
            let inventories = self.lock_inventories()?;
            inventories
                .get(id)
                .filter(|snapshot| snapshot.indexing)
                .map(|snapshot| snapshot.books.clone())
        };
        if let Some(mut books) = staged {
            for book in &mut books {
                book.book_id = book
                    .sha256
                    .as_ref()
                    .map(|hash| {
                        connection
                            .query_row(
                                "SELECT book_id FROM book_files WHERE sha256 = ?1",
                                [hash],
                                |row| row.get(0),
                            )
                            .optional()
                    })
                    .transpose()?
                    .flatten();
            }
            books.sort_by(|a, b| {
                a.relative_path
                    .to_lowercase()
                    .cmp(&b.relative_path.to_lowercase())
            });
            return Ok(books);
        }
        let mut statement = connection.prepare(
            "SELECT d.relative_path, (SELECT book_id FROM book_files WHERE sha256 = d.sha256 LIMIT 1),
                d.sha256, d.title, d.authors_json, d.format, d.size_bytes,
                d.last_seen_at FROM device_books d WHERE d.device_id = ?1
             ORDER BY d.relative_path COLLATE NOCASE",
        )?;
        let mut rows = statement.query([id])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            let authors: String = row.get(4)?;
            let format: String = row.get(5)?;
            result.push(IndexedDeviceBook {
                device_id: id.into(),
                relative_path: row.get(0)?,
                book_id: row.get(1)?,
                sha256: row.get(2)?,
                title: row.get(3)?,
                authors: serde_json::from_str(&authors)?,
                format: format.parse().map_err(AppError::InvalidInput)?,
                size_bytes: positive_integer(row.get(6)?)?,
                last_seen_at: row.get(7)?,
                warnings: Vec::new(),
            });
        }
        Ok(result)
    }

    fn lock_inventories(&self) -> Result<MutexGuard<'_, BTreeMap<String, InventorySnapshot>>> {
        self.inventories
            .lock()
            .map_err(|_| AppError::Conflict("Device inventory state is unavailable".into()))
    }

    fn lock_active(&self) -> Result<MutexGuard<'_, BTreeMap<String, MountCapability>>> {
        self.active
            .lock()
            .map_err(|_| AppError::Conflict("Device discovery state is unavailable".into()))
    }

    fn connected_capability(&self, id: &str) -> Result<MountCapability> {
        self.scan()?;
        self.lock_active()?
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::NotFound("Device is disconnected".into()))
    }

    fn revalidate_connection(&self, expected: &MountCapability) -> Result<()> {
        let current = self.discover()?;
        let found = current
            .get(&expected.id)
            .filter(|found| expected.same_connection(found))
            .ok_or_else(|| AppError::Conflict("Device disconnected or its mount changed".into()))?;
        require_root(&found.path, &expected.identity)
    }

    fn discover(&self) -> Result<BTreeMap<String, MountCapability>> {
        if self.source.native {
            let mut result = BTreeMap::new();
            for volume in native_volumes()? {
                let mount = volume.mount;
                self.insert_capability(
                    &mut result,
                    &mount,
                    mount.mount_point.clone(),
                    volume.stable_identity,
                )?;
                if let Some(capability) = result.values_mut().find(|entry| entry.mount == mount) {
                    capability.total_bytes = volume.total_bytes;
                    if !volume.label.trim().is_empty() {
                        capability.label = bounded_text(&volume.label);
                        capability.profile = device_profile(&capability.path, &capability.label);
                    }
                }
            }
            return Ok(result);
        }
        let mut input = String::new();
        let mut reader: Take<fs::File> =
            fs::File::open(&self.source.mountinfo)?.take(MAX_MOUNTINFO_BYTES + 1);
        reader.read_to_string(&mut input)?;
        if input.len() as u64 > MAX_MOUNTINFO_BYTES {
            return Err(AppError::Unsupported(
                "Linux mount table size limit exceeded".into(),
            ));
        }
        let mounts = parse_mountinfo(&input)?;
        let uuids = filesystem_uuids();
        let mut result = BTreeMap::new();
        for mount in mounts {
            if mount.filesystem == "fuse.gvfsd-fuse" {
                if !self.source.gvfs_roots.contains(&mount.mount_point) {
                    continue;
                }
                let Ok(children) = fs::read_dir(&mount.mount_point) else {
                    continue;
                };
                self.insert_gvfs_children(
                    &mut result,
                    &mount,
                    children.map(|child| {
                        let child = child?;
                        Ok((child.file_name(), child.path()))
                    }),
                )?;
            } else if mount.root == "/"
                && !ignored_filesystem(&mount.filesystem)
                && self
                    .source
                    .media_roots
                    .iter()
                    .any(|root| mount.mount_point.starts_with(root) && mount.mount_point != *root)
            {
                let source_path = fs::canonicalize(&mount.source).ok();
                let stable = if mount.source.starts_with("UUID=") {
                    mount.source.clone()
                } else if let Some(uuid) = source_path.as_ref().and_then(|path| uuids.get(path)) {
                    format!("UUID={uuid}")
                } else {
                    format!(
                        "{}:{}",
                        mount.filesystem,
                        source_path
                            .map(|path| path.to_string_lossy().into_owned())
                            .unwrap_or_else(|| mount.source.clone())
                    )
                };
                self.insert_capability(&mut result, &mount, mount.mount_point.clone(), stable)?;
            }
        }
        Ok(result)
    }

    /// Logical GVFS names identify the transport; paths remain filesystem capabilities.
    /// Production supplies both from the same directory entry. Keeping this seam
    /// explicit also permits portable fixtures without illegal Windows ':' names.
    fn insert_gvfs_children(
        &self,
        result: &mut BTreeMap<String, MountCapability>,
        mount: &MountEntry,
        children: impl IntoIterator<Item = std::io::Result<(std::ffi::OsString, PathBuf)>>,
    ) -> Result<()> {
        if mount.filesystem != "fuse.gvfsd-fuse"
            || !self.source.gvfs_roots.contains(&mount.mount_point)
        {
            return Ok(());
        }
        for child in children.into_iter().take(MAX_MOUNTS) {
            let (name, path) = child?;
            if !name.to_string_lossy().starts_with("mtp:host=") {
                continue;
            }
            if path.parent() != Some(mount.mount_point.as_path()) {
                return Err(AppError::InvalidInput(
                    "GVFS child is outside its mounted root".into(),
                ));
            }
            self.insert_capability(
                result,
                mount,
                path,
                format!("mtp:{}", name.to_string_lossy()),
            )?;
        }
        Ok(())
    }

    fn insert_capability(
        &self,
        result: &mut BTreeMap<String, MountCapability>,
        mount: &MountEntry,
        path: PathBuf,
        stable_identity: String,
    ) -> Result<()> {
        if self.database.path().starts_with(&path) || path.starts_with(self.database.path()) {
            return Ok(());
        }
        let Ok(identity) = directory_identity(&path) else {
            return Ok(());
        };
        let label = path
            .file_name()
            .map(|name| bounded_text(&name.to_string_lossy()))
            .unwrap_or_else(|| "E-reader".into());
        let profile = device_profile(&path, &label);
        let id = format!(
            "usb-{}",
            hex_digest(stable_identity.as_bytes())[..32].to_owned()
        );
        let writable = mount.options.iter().any(|option| option == "rw") && may_write(&path);
        let capability = MountCapability {
            id: id.clone(),
            label,
            profile,
            mount: mount.clone(),
            path,
            identity,
            stable_identity,
            writable,
            total_bytes: None,
        };
        if let Some(previous) = result.get(&id) {
            if previous.path != capability.path || previous.identity != capability.identity {
                return Err(AppError::Conflict(
                    "Two mounted devices have the same identity".into(),
                ));
            }
        } else {
            result.insert(id, capability);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn scan_at(database: Database, mountinfo: &Path, media: &Path, gvfs: &Path) -> Self {
        Self {
            database,
            source: Arc::new(MountSource {
                native: false,
                mountinfo: mountinfo.into(),
                media_roots: vec![media.into()],
                gvfs_roots: vec![gvfs.into()],
            }),
            active: Arc::new(Mutex::new(BTreeMap::new())),
            inventories: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

#[derive(Debug)]
struct NativeVolume {
    mount: MountEntry,
    stable_identity: String,
    label: String,
    total_bytes: Option<u64>,
}

#[cfg(any(windows, target_os = "macos", test))]
const MAX_NATIVE_VOLUMES: usize = 128;
#[cfg(any(windows, target_os = "macos", test))]
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn native_volumes() -> Result<Vec<NativeVolume>> {
    #[cfg(windows)]
    {
        let system = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| {
                AppError::Unsupported("Windows system directory is unavailable".into())
            })?;
        let executable = system.join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let output = run_probe(
            &executable,
            &[
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                WINDOWS_VOLUME_PROBE,
            ],
            std::time::Instant::now() + PROBE_TIMEOUT,
        )?;
        parse_windows_volumes(&output)
    }
    #[cfg(target_os = "macos")]
    {
        let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
        let executable = Path::new("/usr/sbin/diskutil");
        let output = run_probe(
            executable,
            &["list", "-plist", "external", "physical"],
            deadline,
        )?;
        let identifiers = mac_volume_identifiers(&output)?;
        let mut volumes = Vec::new();
        for identifier in identifiers {
            // Identifiers are parsed from the OS reply, never accepted from chat
            // or IPC, and are validated before they become a command argument.
            let info = run_probe(executable, &["info", "-plist", &identifier], deadline)?;
            if let Some(volume) = parse_mac_volume(&info, &identifier)? {
                volumes.push(volume);
            }
        }
        Ok(volumes)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Err(AppError::Unsupported(
            "Native volume probe is unavailable".into(),
        ))
    }
}

// No interpolated command fragments, mounting, formatting or write probes.
// PowerShell 5.1 ships with Windows 11; InputObject keeps 0/1/N results arrays.
#[cfg(any(windows, test))]
const WINDOWS_VOLUME_PROBE: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$clock = [System.Diagnostics.Stopwatch]::StartNew()
function Write-ProbeStage([string]$stage) {
  [Console]::Error.WriteLine(('library-manager-volume-probe:{0}:{1}' -f $stage, $clock.ElapsedMilliseconds))
}
Write-ProbeStage 'start'
$namespace = 'root/Microsoft/Windows/Storage'
# Three local read-only snapshots avoid Storage-module initialization and N+1
# association queries for every disk/partition, including unrelated fixed disks.
$disks = @(Get-CimInstance -Namespace $namespace -ClassName MSFT_Disk -Property Number,UniqueId,BusType,IsBoot,IsSystem,IsReadOnly -OperationTimeoutSec 8)
Write-ProbeStage 'disks'
$partitions = @(Get-CimInstance -Namespace $namespace -ClassName MSFT_Partition -Property DiskNumber,PartitionNumber,DriveLetter,IsReadOnly,IsBoot,IsSystem -OperationTimeoutSec 8)
Write-ProbeStage 'partitions'
$volumes = @(Get-CimInstance -Namespace $namespace -ClassName MSFT_Volume -Property DriveLetter,UniqueId,FileSystem,FileSystemLabel,DriveType,Size -OperationTimeoutSec 8)
Write-ProbeStage 'volumes'
if ($disks.Count -gt 4096 -or $partitions.Count -gt 4096 -or $volumes.Count -gt 4096) { throw 'Volume snapshot limit exceeded' }
$diskByNumber = @{}
foreach ($disk in $disks) {
  if ($null -eq $disk.Number -or $diskByNumber.ContainsKey([string]$disk.Number)) { throw 'Ambiguous disk snapshot' }
  $diskByNumber[[string]$disk.Number] = $disk
}
$partitionByLetter = @{}
foreach ($partition in $partitions) {
  $letter = [string]$partition.DriveLetter
  if ([string]::IsNullOrEmpty($letter) -or $letter -eq [string][char]0) { continue }
  if ($letter -notmatch '^[a-zA-Z]$' -or $partitionByLetter.ContainsKey($letter)) { throw 'Ambiguous partition snapshot' }
  $partitionByLetter[$letter] = $partition
}
$result = @()
foreach ($volume in $volumes) {
  $letter = [string]$volume.DriveLetter
  if ([string]::IsNullOrEmpty($letter) -or $letter -eq [string][char]0) { continue }
  if ($letter -notmatch '^[a-zA-Z]$' -or $null -eq $volume.DriveType) { throw 'Invalid mounted volume' }
  $removable = [uint32]$volume.DriveType -eq 2
  $partition = $partitionByLetter[$letter]
  if ($null -eq $partition) {
    if ($removable) { throw 'Mounted removable volume has no partition' }
    continue
  }
  $disk = $diskByNumber[[string]$partition.DiskNumber]
  if ($null -eq $disk -or $null -eq $disk.BusType) { throw 'Mounted partition has no disk' }
  $usb = [uint16]$disk.BusType -eq 7
  if (-not $usb -and -not $removable) { continue }
  foreach ($entry in @($disk, $partition)) {
    if ($entry.IsBoot -isnot [bool] -or $entry.IsSystem -isnot [bool] -or $entry.IsReadOnly -isnot [bool]) { throw 'Incomplete disk safety properties' }
  }
  if ($disk.IsBoot -or $disk.IsSystem -or $partition.IsBoot -or $partition.IsSystem) { continue }
  if ([string]::IsNullOrEmpty([string]$volume.FileSystem) -or $volume.FileSystem -eq 'Unknown') { continue }
  if ([string]::IsNullOrEmpty([string]$disk.UniqueId) -or [string]::IsNullOrEmpty([string]$volume.UniqueId) -or $null -eq $partition.PartitionNumber -or $null -eq $volume.Size) { throw 'Incomplete mounted volume identity' }
  $result += [pscustomobject]@{
    driveLetter = $letter
    volumeId = [string]$volume.UniqueId
    diskId = [string]$disk.UniqueId
    diskNumber = [uint32]$disk.Number
    partitionNumber = [uint32]$partition.PartitionNumber
    label = [string]$volume.FileSystemLabel
    filesystem = [string]$volume.FileSystem
    readOnly = [bool]($disk.IsReadOnly -or $partition.IsReadOnly)
    isBoot = [bool]($disk.IsBoot -or $partition.IsBoot)
    isSystem = [bool]($disk.IsSystem -or $partition.IsSystem)
    usb = [bool]$usb
    removable = [bool]$removable
    sizeBytes = [uint64]$volume.Size
  }
  if ($result.Count -gt 128) { throw 'Mounted volume limit exceeded' }
}
Write-ProbeStage 'complete'
ConvertTo-Json -InputObject @($result) -Depth 4 -Compress
"#;

#[cfg(any(windows, test))]
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WindowsVolume {
    drive_letter: String,
    volume_id: String,
    disk_id: String,
    disk_number: u32,
    partition_number: u32,
    label: String,
    filesystem: String,
    read_only: bool,
    is_boot: bool,
    is_system: bool,
    usb: bool,
    removable: bool,
    size_bytes: u64,
}

#[cfg(any(windows, test))]
fn parse_windows_volumes(input: &str) -> Result<Vec<NativeVolume>> {
    ensure_probe_size(input)?;
    let rows: Vec<WindowsVolume> = serde_json::from_str(input)
        .map_err(|_| AppError::Unsupported("Invalid Windows volume information".into()))?;
    if rows.len() > MAX_NATIVE_VOLUMES {
        return Err(probe_limit());
    }
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for row in rows {
        if row.is_boot || row.is_system || !(row.usb || row.removable) {
            continue;
        }
        if row.drive_letter.len() != 1
            || !row.drive_letter.as_bytes()[0].is_ascii_alphabetic()
            || !safe_native_identifier(&row.volume_id)
            || !safe_native_identifier(&row.disk_id)
            || row.filesystem.is_empty()
            || row.filesystem.len() > 64
            || !row
                .filesystem
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric())
            || row.size_bytes == 0
        {
            return Err(AppError::Unsupported(
                "Invalid Windows mounted volume".into(),
            ));
        }
        let stable = format!("windows:{}:{}", row.disk_id, row.volume_id);
        if !seen.insert(stable.clone()) {
            return Err(AppError::Conflict(
                "Duplicate mounted volume identity".into(),
            ));
        }
        let path = PathBuf::from(format!("{}:\\", row.drive_letter.to_ascii_uppercase()));
        result.push(NativeVolume {
            mount: MountEntry {
                mount_id: format!(
                    "{}:{}:{}",
                    row.disk_number, row.partition_number, row.volume_id
                ),
                root: "/".into(),
                mount_point: path,
                options: vec![if row.read_only { "ro" } else { "rw" }.into()],
                filesystem: row.filesystem,
                source: row.disk_id,
            },
            stable_identity: stable,
            label: row.label,
            total_bytes: Some(row.size_bytes),
        });
    }
    Ok(result)
}

#[cfg(any(windows, target_os = "macos", test))]
fn safe_native_identifier(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
}

#[cfg(any(windows, target_os = "macos", test))]
fn ensure_probe_size(input: &str) -> Result<()> {
    if input.len() as u64 > MAX_MOUNTINFO_BYTES {
        return Err(probe_limit());
    }
    Ok(())
}

#[cfg(any(windows, target_os = "macos", test))]
fn probe_limit() -> AppError {
    AppError::Unsupported("Mounted volume probe limit exceeded".into())
}

#[cfg(any(windows, target_os = "macos", test))]
fn safe_probe_diagnostic(line: &str) -> Option<String> {
    let line = line.trim_end_matches(['\r', '\n']);
    let tail = line.strip_prefix("library-manager-volume-probe:")?;
    let (stage, elapsed) = tail.split_once(':')?;
    if !matches!(
        stage,
        "start" | "disks" | "partitions" | "volumes" | "complete"
    ) || elapsed.is_empty()
        || elapsed.len() > 5
        || !elapsed.bytes().all(|byte| byte.is_ascii_digit())
        || elapsed.parse::<u64>().ok()? > PROBE_TIMEOUT.as_millis() as u64
    {
        return None;
    }
    Some(format!("Mounted volume probe stage {stage}: {elapsed} ms"))
}

#[cfg(any(windows, target_os = "macos", test))]
fn run_probe(program: &Path, arguments: &[&str], deadline: std::time::Instant) -> Result<String> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    if Instant::now() >= deadline {
        return Err(AppError::Unsupported(
            "Mounted volume probe timed out".into(),
        ));
    }
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(stderr) = child.stderr.take() {
        // Bounded diagnostics contain no provider output, paths or identifiers.
        // This reader ends when the subprocess exits or is killed at the deadline.
        std::thread::spawn(move || {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(stderr.take(4096));
            for line in reader.lines().map_while(std::result::Result::ok) {
                if let Some(message) = safe_probe_diagnostic(&line) {
                    eprintln!("{message}");
                }
            }
        });
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Unsupported("Mounted volume probe output unavailable".into()))?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let result = stdout
            .take(MAX_MOUNTINFO_BYTES + 1)
            .read_to_end(&mut output)
            .map(|_| output);
        let _ = sender.send(result);
    });
    let mut output = None;
    let mut status = None;
    loop {
        if output.is_none() {
            match receiver.try_recv() {
                Ok(Ok(bytes)) if bytes.len() as u64 <= MAX_MOUNTINFO_BYTES => output = Some(bytes),
                Ok(result) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return match result {
                        Err(error) => Err(error.into()),
                        _ => Err(probe_limit()),
                    };
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(AppError::Unsupported(
                        "Mounted volume probe output failed".into(),
                    ));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(value) => status = value,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.into());
                }
            }
        }
        if let (Some(bytes), Some(exit)) = (output.as_ref(), status) {
            let _ = reader.join();
            if !exit.success() {
                return Err(AppError::Unsupported("Mounted volume probe failed".into()));
            }
            return String::from_utf8(bytes.clone()).map_err(|_| {
                AppError::Unsupported("Mounted volume probe must return UTF-8".into())
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::Unsupported(
                "Mounted volume probe timed out".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_plist(input: &str) -> Result<serde_json::Value> {
    ensure_probe_size(input)?;
    // Apple's standard declaration has no internal entities. All other DTDs
    // remain rejected by roxmltree; no external resource is ever resolved.
    let input = input.replace("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">", "");
    let document = roxmltree::Document::parse_with_options(
        &input,
        roxmltree::ParsingOptions {
            nodes_limit: 100_000,
            ..Default::default()
        },
    )
    .map_err(|_| AppError::Unsupported("Invalid mounted volume property list".into()))?;
    let root = document.root_element();
    if !root.has_tag_name("plist") {
        return Err(probe_limit());
    }
    let children: Vec<_> = root.children().filter(|node| node.is_element()).collect();
    if children.len() != 1 {
        return Err(probe_limit());
    }
    plist_value(children[0], 0)
}

#[cfg(any(target_os = "macos", test))]
fn plist_value(node: roxmltree::Node<'_, '_>, depth: usize) -> Result<serde_json::Value> {
    use serde_json::Value;
    if depth > 32 {
        return Err(probe_limit());
    }
    Ok(match node.tag_name().name() {
        "dict" => {
            let mut children = node.children().filter(|child| child.is_element());
            let mut fields = serde_json::Map::new();
            while let Some(key) = children.next() {
                if !key.has_tag_name("key") {
                    return Err(probe_limit());
                }
                let key = key.text().ok_or_else(probe_limit)?;
                let value = children.next().ok_or_else(probe_limit)?;
                if fields
                    .insert(key.to_owned(), plist_value(value, depth + 1)?)
                    .is_some()
                {
                    return Err(probe_limit());
                }
            }
            Value::Object(fields)
        }
        "array" => Value::Array(
            node.children()
                .filter(|child| child.is_element())
                .map(|child| plist_value(child, depth + 1))
                .collect::<Result<_>>()?,
        ),
        "string" => Value::String(node.text().unwrap_or_default().to_owned()),
        "integer" => Value::from(
            node.text()
                .unwrap_or_default()
                .parse::<u64>()
                .map_err(|_| probe_limit())?,
        ),
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => return Err(probe_limit()),
    })
}

#[cfg(any(target_os = "macos", test))]
fn mac_disk_identifier(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("disk") else {
        return false;
    };
    !rest.is_empty()
        && rest
            .split('s')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && value.len() <= 64
}

#[cfg(any(target_os = "macos", test))]
fn mac_volume_identifiers(input: &str) -> Result<Vec<String>> {
    let list = parse_plist(input)?;
    let disks = list
        .get("AllDisksAndPartitions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(probe_limit)?;
    let mut identifiers = std::collections::BTreeSet::new();
    fn collect(
        value: &serde_json::Value,
        result: &mut std::collections::BTreeSet<String>,
        depth: usize,
    ) -> Result<()> {
        if depth > 32 {
            return Err(probe_limit());
        }
        let identifier = value
            .get("DeviceIdentifier")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(probe_limit)?;
        if !mac_disk_identifier(identifier) {
            return Err(probe_limit());
        }
        // Whole disks are queried too: a superfloppy can have no partition.
        result.insert(identifier.to_owned());
        if result.len() > MAX_NATIVE_VOLUMES {
            return Err(probe_limit());
        }
        for key in ["Partitions", "APFSVolumes"] {
            if let Some(children) = value.get(key) {
                let children = children.as_array().ok_or_else(probe_limit)?;
                for child in children {
                    collect(child, result, depth + 1)?;
                }
            }
        }
        Ok(())
    }
    for disk in disks {
        collect(disk, &mut identifiers, 0)?;
    }
    Ok(identifiers.into_iter().collect())
}

#[cfg(any(target_os = "macos", test))]
fn parse_mac_volume(input: &str, expected_identifier: &str) -> Result<Option<NativeVolume>> {
    let info = parse_plist(input)?;
    let text = |key: &str| info.get(key).and_then(serde_json::Value::as_str);
    if text("DeviceIdentifier") != Some(expected_identifier)
        || !mac_disk_identifier(expected_identifier)
    {
        return Err(probe_limit());
    }
    let Some(mount) = text("MountPoint").filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if info.get("Internal").and_then(serde_json::Value::as_bool) != Some(false)
        || !mount.starts_with("/Volumes/")
        || Path::new(mount)
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(AppError::Unsupported("Unsafe external macOS volume".into()));
    }
    let media_read_only = info
        .get("MediaReadOnly")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(probe_limit)?;
    let volume_read_only = info
        .get("VolumeReadOnly")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(probe_limit)?;
    let read_only = media_read_only || volume_read_only;
    let stable = text("VolumeUUID")
        .or_else(|| text("MediaUUID"))
        .filter(|value| safe_native_identifier(value))
        .ok_or_else(probe_limit)?;
    let filesystem = text("FilesystemType")
        .or_else(|| text("FileSystemPersonality"))
        .ok_or_else(probe_limit)?;
    Ok(Some(NativeVolume {
        mount: MountEntry {
            mount_id: format!("{expected_identifier}:{stable}"),
            root: "/".into(),
            mount_point: PathBuf::from(mount),
            options: vec![if read_only { "ro" } else { "rw" }.into()],
            filesystem: bounded_text(filesystem),
            source: expected_identifier.into(),
        },
        stable_identity: format!("macos:{stable}"),
        label: text("VolumeName").unwrap_or("E-reader").into(),
        total_bytes: info.get("TotalSize").and_then(serde_json::Value::as_u64),
    }))
}

fn parse_mountinfo(input: &str) -> Result<Vec<MountEntry>> {
    let mut entries = Vec::new();
    for line in input.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let fields: Vec<&str> = left.split_ascii_whitespace().collect();
        let suffix: Vec<&str> = right.split_ascii_whitespace().collect();
        if fields.len() < 6 || suffix.len() < 3 {
            continue;
        }
        if entries.len() == MAX_MOUNTS {
            return Err(AppError::Unsupported(
                "Linux mount count limit exceeded".into(),
            ));
        }
        let mut options: Vec<String> = fields[5].split(',').map(str::to_owned).collect();
        if suffix[2].split(',').any(|option| option == "ro") {
            options.retain(|option| option != "rw");
            options.push("ro".into());
        }
        entries.push(MountEntry {
            mount_id: fields[0].into(),
            root: decode_mount_field(fields[3])?,
            mount_point: PathBuf::from(decode_mount_field(fields[4])?),
            options,
            filesystem: suffix[0].into(),
            source: decode_mount_field(suffix[1])?,
        });
    }
    Ok(entries)
}

fn decode_mount_field(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            let digits = bytes.get(index + 1..index + 4).ok_or_else(|| {
                AppError::InvalidInput("Malformed escaped Linux mount field".into())
            })?;
            if !digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) {
                return Err(AppError::InvalidInput(
                    "Malformed escaped Linux mount field".into(),
                ));
            }
            let number = u16::from(digits[0] - b'0') * 64
                + u16::from(digits[1] - b'0') * 8
                + u16::from(digits[2] - b'0');
            decoded.push(
                u8::try_from(number).map_err(|_| {
                    AppError::InvalidInput("Invalid escaped Linux mount field".into())
                })?,
            );
            index += 4;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .map_err(|_| AppError::InvalidInput("Linux mount name is not valid UTF-8".into()))
}

fn filesystem_uuids() -> BTreeMap<PathBuf, String> {
    let mut result = BTreeMap::new();
    if let Ok(entries) = fs::read_dir("/dev/disk/by-uuid") {
        for entry in entries.take(MAX_MOUNTS).flatten() {
            if let Ok(path) = fs::canonicalize(entry.path()) {
                result.insert(path, entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    result
}

fn ignored_filesystem(name: &str) -> bool {
    matches!(
        name,
        "proc"
            | "sysfs"
            | "tmpfs"
            | "devtmpfs"
            | "cgroup"
            | "cgroup2"
            | "overlay"
            | "squashfs"
            | "autofs"
            | "securityfs"
            | "debugfs"
            | "tracefs"
            | "ramfs"
            | "nfs"
            | "nfs4"
            | "cifs"
            | "fuse.sshfs"
    )
}

fn directory_identity(path: &Path) -> Result<RootIdentity> {
    SecureDir::open(path, false, AccessPolicy::Shared)?.identity()
}

fn require_root(path: &Path, expected: &RootIdentity) -> Result<()> {
    if directory_identity(path)? != *expected {
        return Err(AppError::Conflict("The mounted device root changed".into()));
    }
    Ok(())
}

fn validate_contained_file(path: &Path, root: &Path) -> Result<()> {
    open_device_file(path, root).map(|_| ())
}

fn open_device_file(path: &Path, root: &Path) -> Result<fs::File> {
    let relative = relative_utf8_path(path, root)?;
    let mut components = Path::new(&relative).components().peekable();
    let mut parent = SecureDir::open(root, false, AccessPolicy::Shared)?;
    while let Some(Component::Normal(name)) = components.next() {
        if components.peek().is_none() {
            return parent.open_regular(name);
        }
        parent = parent.child(name, false, AccessPolicy::Shared)?;
    }
    Err(AppError::InvalidInput(
        "Unsafe device inventory path".into(),
    ))
}

fn device_snapshot(path: &Path, root: &Path) -> Result<FileSnapshot> {
    secure_fs::snapshot(&open_device_file(path, root)?)
}

fn may_write(path: &Path) -> bool {
    #[cfg(unix)]
    {
        access(path, Access::WRITE_OK).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

fn volume_capacity(path: &Path) -> (Option<u64>, Option<u64>) {
    let free = SecureDir::open(path, false, AccessPolicy::Shared)
        .and_then(|dir| dir.free_space())
        .ok();
    #[cfg(unix)]
    let total = statvfs(path)
        .ok()
        .and_then(|stats| stats.f_blocks.checked_mul(stats.f_frsize));
    #[cfg(not(unix))]
    let total = None;
    (total, free)
}

fn relative_utf8_path(path: &Path, root: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| AppError::InvalidInput("Device file escapes its mounted root".into()))?;
    if !relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(AppError::InvalidInput(
            "Unsafe device inventory path".into(),
        ));
    }
    relative
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| AppError::InvalidInput("Device filename is not valid UTF-8".into()))
}

fn reject_replaced_roots(
    previous: &BTreeMap<String, MountCapability>,
    current: &BTreeMap<String, MountCapability>,
) -> Result<()> {
    for (id, mounted) in current {
        if previous.get(id).is_some_and(|old| {
            old.mount.mount_id == mounted.mount.mount_id
                && old.path == mounted.path
                && old.identity != mounted.identity
        }) {
            return Err(AppError::Conflict(
                "A device root was replaced without remounting".into(),
            ));
        }
    }
    Ok(())
}

fn device_profile(path: &Path, label: &str) -> String {
    let label = label.to_lowercase();
    let marker = |name: &str| {
        let target = path.join(name);
        SecureDir::open(&target, false, AccessPolicy::Shared).is_ok()
            || open_device_file(&target, path).is_ok()
    };
    if label.contains("xteink") || marker(".crosspoint") || marker(".crossink") {
        "xteink"
    } else if label.contains("kindle") || marker("system/com.amazon.ebook.booklet.reader") {
        "kindle"
    } else if label.contains("kobo") || marker(".kobo") {
        "kobo"
    } else if label.contains("pocketbook") {
        "pocketbook"
    } else if label.contains("tolino") {
        "tolino"
    } else {
        "generic"
    }
    .into()
}

fn ignored_directory(path: &Path, root: &Path) -> bool {
    if path == root || !path.is_dir() {
        return false;
    }
    path.file_name().is_some_and(|name| {
        matches!(
            name.to_str(),
            Some(
                ".crosspoint"
                    | ".crossink"
                    | ".Trash"
                    | ".Trashes"
                    | "$RECYCLE.BIN"
                    | "System Volume Information"
                    | "lost+found"
            )
        )
    })
}

fn format_for_path(path: &Path) -> Option<BookFormat> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
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

fn bounded_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .take(MAX_METADATA_CHARS)
        .collect()
}

fn positive_integer(value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| AppError::InvalidInput("Invalid stored device count or size".into()))
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        Err(AppError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book_repository::{BookRepository, StoredFile};
    use crate::models::{BookFile, BookMetadata, DeviceTransport, FileVariant};
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    #[cfg(windows)]
    fn symlink(source: impl AsRef<Path>, destination: impl AsRef<Path>) -> std::io::Result<()> {
        if source.as_ref().is_dir() {
            std::os::windows::fs::symlink_dir(source, destination)
        } else {
            std::os::windows::fs::symlink_file(source, destination)
        }
    }

    fn windows_volume() -> serde_json::Value {
        serde_json::json!({"driveLetter":"E","volumeId":"volume-unique","diskId":"usb-serial",
            "diskNumber":2,"partitionNumber":1,"label":"Reader","filesystem":"FAT32",
            "readOnly":false,"isBoot":false,"isSystem":false,"usb":true,"removable":true,
            "sizeBytes":32_000_000})
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn native_readonly_volume_probe_runs_on_its_actual_operating_system() {
        // CI may have no removable media. This still executes the platform's
        // actual builtin command and parses its real empty or populated reply.
        // It does not claim physical reader compatibility.
        let volumes = native_volumes().unwrap();
        assert!(volumes.len() <= MAX_NATIVE_VOLUMES);
        for volume in volumes {
            assert!(volume.mount.mount_point.is_absolute());
            assert!(safe_native_identifier(&volume.stable_identity));
        }
    }

    #[test]
    fn windows_probe_uses_bulk_snapshots_without_per_partition_queries() {
        assert_eq!(WINDOWS_VOLUME_PROBE.matches("Get-CimInstance").count(), 3);
        for command in ["Get-Disk", "Get-Partition", "Get-Volume"] {
            assert!(!WINDOWS_VOLUME_PROBE.contains(command));
        }
        for class in ["MSFT_Disk", "MSFT_Partition", "MSFT_Volume"] {
            assert!(WINDOWS_VOLUME_PROBE.contains(class));
        }
        assert_eq!(PROBE_TIMEOUT.as_secs(), 10);
    }

    #[cfg(windows)]
    #[test]
    fn windows_bulk_probe_joins_complete_snapshots_and_rejects_partial_results() {
        let base = serde_json::json!({
            "disks": [{"Number":2,"UniqueId":"usb-serial","BusType":7,
                "IsBoot":false,"IsSystem":false,"IsReadOnly":false}],
            "partitions": [{"DiskNumber":2,"PartitionNumber":1,"DriveLetter":"E",
                "IsBoot":false,"IsSystem":false,"IsReadOnly":false}],
            "volumes": [{"DriveLetter":"E","UniqueId":"volume-unique","FileSystem":"FAT32",
                "FileSystemLabel":"Reader","DriveType":3,"Size":32_000_000}]
        });
        let execute = |fixture: &serde_json::Value| -> Result<Vec<NativeVolume>> {
            // Execute the production PowerShell join/parser, replacing only the
            // three OS queries with deterministic snapshots. No device is touched.
            let script = format!(
                r#"
$fixture = ConvertFrom-Json -InputObject '{}'
$script:calls = 0
function Get-CimInstance {{
  param([string]$Namespace,[string]$ClassName,[string[]]$Property,[uint32]$OperationTimeoutSec)
  if ($Namespace -ne 'root/Microsoft/Windows/Storage' -or $OperationTimeoutSec -ne 8 -or $Property.Count -eq 0) {{ throw 'Invalid query contract' }}
  $script:calls++
  switch ($ClassName) {{
    'MSFT_Disk' {{ return $fixture.disks }}
    'MSFT_Partition' {{ return $fixture.partitions }}
    'MSFT_Volume' {{ return $fixture.volumes }}
    default {{ throw 'Unexpected query' }}
  }}
}}
{}
if ($script:calls -ne 3) {{ throw 'Incorrect query count' }}
"#,
                fixture.to_string().replace('\'', "''"),
                WINDOWS_VOLUME_PROBE
            );
            let program = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
            let output = run_probe(
                &program,
                &[
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    &script,
                ],
                std::time::Instant::now() + PROBE_TIMEOUT,
            )?;
            parse_windows_volumes(&output)
        };
        let original = execute(&base).unwrap();
        assert_eq!(original.len(), 1);
        assert_eq!(original[0].mount.options, ["rw"]);
        for section in ["disks", "partitions"] {
            let mut read_only = base.clone();
            read_only[section][0]["IsReadOnly"] = true.into();
            assert_eq!(execute(&read_only).unwrap()[0].mount.options, ["ro"]);
        }
        let mut internal = base.clone();
        internal["disks"][0]["BusType"] = 17.into();
        assert!(execute(&internal).unwrap().is_empty());
        internal["volumes"][0]["DriveType"] = 2.into();
        assert_eq!(execute(&internal).unwrap().len(), 1);
        let mut boot = base.clone();
        boot["partitions"][0]["IsBoot"] = true.into();
        assert!(execute(&boot).unwrap().is_empty());
        let mut missing_disk = base.clone();
        missing_disk["disks"] = serde_json::json!([]);
        assert!(execute(&missing_disk).is_err());
        let mut missing_partition = internal.clone();
        missing_partition["partitions"] = serde_json::json!([]);
        assert!(execute(&missing_partition).is_err());
        let mut unknown_readonly = base.clone();
        unknown_readonly["partitions"][0]["IsReadOnly"] = serde_json::Value::Null;
        assert!(execute(&unknown_readonly).is_err());
        let mut unknown_identity = base.clone();
        unknown_identity["volumes"][0]["UniqueId"] = "".into();
        assert!(execute(&unknown_identity).is_err());
        let empty = serde_json::json!({"disks":[],"partitions":[],"volumes":[]});
        assert!(execute(&empty).unwrap().is_empty());
    }

    #[test]
    fn probe_stage_diagnostics_do_not_copy_arbitrary_stderr() {
        assert_eq!(
            safe_probe_diagnostic("library-manager-volume-probe:disks:42\r\n"),
            Some("Mounted volume probe stage disks: 42 ms".into())
        );
        for unsafe_line in [
            "provider key or private path",
            "library-manager-volume-probe:secret:42",
            "library-manager-volume-probe:disks:42:private",
            "library-manager-volume-probe:disks:-1",
            "library-manager-volume-probe:disks:10001",
            "library-manager-volume-probe:disks:",
        ] {
            assert!(safe_probe_diagnostic(unsafe_line).is_none());
        }
        assert!(safe_probe_diagnostic("library-manager-volume-probe:complete:10000").is_some());
    }

    #[test]
    fn windows_probe_filters_system_disks_and_preserves_readonly_volume_identity() {
        let mut row = windows_volume();
        let volumes = parse_windows_volumes(&serde_json::json!([row]).to_string()).unwrap();
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].mount.options, ["rw"]);
        assert_eq!(volumes[0].mount.mount_point.to_string_lossy(), "E:\\");
        let stable = volumes[0].stable_identity.clone();
        row["driveLetter"] = "F".into();
        row["readOnly"] = true.into();
        let volumes = parse_windows_volumes(&serde_json::json!([row]).to_string()).unwrap();
        assert_eq!(volumes[0].stable_identity, stable);
        assert_eq!(volumes[0].mount.options, ["ro"]);
        row["isBoot"] = true.into();
        assert!(
            parse_windows_volumes(&serde_json::json!([row]).to_string())
                .unwrap()
                .is_empty()
        );
        assert!(parse_windows_volumes("[]").unwrap().is_empty());
        assert!(!WINDOWS_VOLUME_PROBE.contains("Set-"));
        assert_eq!(PROBE_TIMEOUT.as_secs(), 10);
    }

    #[test]
    fn windows_probe_rejects_unsafe_paths_types_and_ambiguous_identity() {
        for letter in ["E:\\", "../E", "\\\\server", "1", ""] {
            let mut row = windows_volume();
            row["driveLetter"] = letter.into();
            assert!(parse_windows_volumes(&serde_json::json!([row]).to_string()).is_err());
        }
        let mut row = windows_volume();
        row["readOnly"] = "false".into();
        assert!(parse_windows_volumes(&serde_json::json!([row]).to_string()).is_err());
        let row = windows_volume();
        assert!(matches!(
            parse_windows_volumes(&serde_json::json!([row, row]).to_string()),
            Err(AppError::Conflict(_))
        ));
        assert!(parse_windows_volumes(&" ".repeat(MAX_MOUNTINFO_BYTES as usize + 1)).is_err());
    }

    fn plist(body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0">{body}</plist>"#
        )
    }

    fn mac_info() -> String {
        plist(
            "<dict><key>DeviceIdentifier</key><string>disk2s1</string><key>Internal</key><false/><key>MountPoint</key><string>/Volumes/Reader</string><key>MediaReadOnly</key><false/><key>VolumeReadOnly</key><true/><key>VolumeUUID</key><string>volume-uuid</string><key>VolumeName</key><string>Reader</string><key>FilesystemType</key><string>msdos</string><key>TotalSize</key><integer>32000000</integer></dict>",
        )
    }

    #[test]
    fn mac_plist_probe_only_accepts_external_mounted_volumes_and_fixed_disk_identifiers() {
        let list = plist(
            "<dict><key>AllDisksAndPartitions</key><array><dict><key>DeviceIdentifier</key><string>disk2</string><key>Partitions</key><array><dict><key>DeviceIdentifier</key><string>disk2s1</string></dict></array></dict></array></dict>",
        );
        assert_eq!(mac_volume_identifiers(&list).unwrap(), ["disk2", "disk2s1"]);
        let volume = parse_mac_volume(&mac_info(), "disk2s1").unwrap().unwrap();
        assert_eq!(volume.mount.options, ["ro"]);
        assert_eq!(volume.total_bytes, Some(32_000_000));
        assert!(parse_mac_volume(&mac_info(), "disk3s1").is_err());
        for unsafe_id in ["disk2;echo", "--help", "disk", "disk2s", "/dev/disk2"] {
            assert!(!mac_disk_identifier(unsafe_id));
        }
        assert!(parse_mac_volume(&mac_info().replace("/Volumes/Reader", "/"), "disk2s1").is_err());
        assert!(
            parse_mac_volume(
                &mac_info().replace("/Volumes/Reader", "/Volumes/../Users"),
                "disk2s1"
            )
            .is_err()
        );
        assert!(
            parse_mac_volume(
                &mac_info().replace("<key>Internal</key><false/>", "<key>Internal</key><true/>"),
                "disk2s1"
            )
            .is_err()
        );
        assert!(
            parse_mac_volume(
                &mac_info().replace("<key>VolumeReadOnly</key><true/>", ""),
                "disk2s1"
            )
            .is_err()
        );
        assert!(
            parse_mac_volume(&mac_info().replace("/Volumes/Reader", ""), "disk2s1")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn mac_plist_parser_rejects_duplicate_keys_entities_and_probe_limits() {
        assert!(
            parse_plist(&plist(
                "<dict><key>x</key><true/><key>x</key><false/></dict>"
            ))
            .is_err()
        );
        assert!(parse_plist("<!DOCTYPE plist [<!ENTITY external SYSTEM 'file:///etc/passwd'>]><plist><string>&external;</string></plist>").is_err());
        assert!(parse_plist(&plist("<dict><key>x</key></dict>")).is_err());
        assert!(parse_plist(&" ".repeat(MAX_MOUNTINFO_BYTES as usize + 1)).is_err());
        let rows = serde_json::json!(vec![windows_volume(); MAX_NATIVE_VOLUMES + 1]);
        assert!(parse_windows_volumes(&rows.to_string()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn readonly_probe_has_a_bounded_deadline_and_output() {
        let start = std::time::Instant::now();
        assert!(matches!(
            run_probe(
                Path::new("/bin/sleep"),
                &["5"],
                start + std::time::Duration::from_millis(30)
            ),
            Err(AppError::Unsupported(message)) if message == "Mounted volume probe timed out"
        ));
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        assert!(matches!(
            run_probe(
                Path::new("/bin/sh"),
                &["-c", "printf '%4194305s' ''"],
                std::time::Instant::now() + PROBE_TIMEOUT
            ),
            Err(AppError::Unsupported(message)) if message == "Mounted volume probe limit exceeded"
        ));
        assert_eq!(
            run_probe(
                Path::new("/bin/sh"),
                &["-c", "printf '[]'"],
                std::time::Instant::now() + PROBE_TIMEOUT
            )
            .unwrap(),
            "[]"
        );
    }

    struct Fixture {
        _temporary: TempDir,
        media: PathBuf,
        card: PathBuf,
        gvfs: PathBuf,
        mountinfo: PathBuf,
        service: DeviceService,
        database: Database,
    }

    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let media = temporary.path().join("media");
            let card = media.join("Xteink Livres é");
            let gvfs = temporary.path().join("gvfs");
            let mountinfo = temporary.path().join("mountinfo");
            fs::create_dir_all(&card).unwrap();
            fs::create_dir_all(&gvfs).unwrap();
            let database = Database::new(&temporary.path().join("profile")).unwrap();
            let service = DeviceService::scan_at(database.clone(), &mountinfo, &media, &gvfs);
            let fixture = Self {
                _temporary: temporary,
                media,
                card,
                gvfs,
                mountinfo,
                service,
                database,
            };
            fixture.mount(42, "rw");
            fixture
        }

        fn mount(&self, mount_id: u64, options: &str) {
            fs::write(
                &self.mountinfo,
                format!(
                    "{mount_id} 1 8:1 / {} {options},nosuid - vfat UUID=TEST-CARD {options}\n",
                    escape_path(&self.card)
                ),
            )
            .unwrap();
        }

        fn id(&self) -> String {
            self.service
                .scan()
                .unwrap()
                .into_iter()
                .find(|device| device.connected)
                .unwrap()
                .id
        }
    }

    fn escape_path(path: &Path) -> String {
        path.to_str()
            .unwrap()
            .replace('\\', "\\134")
            .replace(' ', "\\040")
    }

    fn epub(path: &Path) {
        let entries = BTreeMap::from([
            ("mimetype".into(), b"application/epub+zip".to_vec()),
            ("META-INF/container.xml".into(), br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.to_vec()),
            ("content.opf".into(), br#"<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">urn:test:device</dc:identifier><dc:title>Device Book</dc:title><dc:creator>Jane Writer</dc:creator><dc:language>en</dc:language></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#.to_vec()),
            ("chapter.xhtml".into(), br#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Chapter</title></head><body><p>Device text.</p></body></html>"#.to_vec()),
        ]);
        EpubDocument::from_entries(entries, "Device Book")
            .unwrap()
            .write(path, 6)
            .unwrap();
    }

    #[test]
    fn mounted_card_disconnect_history_and_no_source_changes() {
        let fixture = Fixture::new();
        let source = fixture.card.join("Book.epub");
        epub(&source);
        let hash = Storage::hash_file(&source).unwrap();
        let id = fixture.id();
        let devices = fixture.service.scan().unwrap();
        assert_eq!(devices.len(), 1);
        assert!(devices[0].connected && devices[0].writable);
        assert_eq!(devices[0].profile, "xteink");
        // The mountinfo fixture has no native Windows capacity descriptor.
        // Real Windows probes supply TotalSize; Unix can query statvfs here.
        assert_eq!(devices[0].total_bytes.is_some(), cfg!(unix));
        assert_eq!(
            fixture.service.resolve_connected(&id).unwrap(),
            fixture.card
        );
        let indexed = fixture.service.index(&id).unwrap();
        assert_eq!(indexed[0].title, "Device Book");
        assert_eq!(indexed[0].authors, ["Jane Writer"]);
        assert_eq!(indexed[0].sha256.as_deref(), Some(hash.as_str()));
        assert!(indexed[0].book_id.is_none());
        assert!(indexed[0].warnings.is_empty());
        assert_eq!(Storage::hash_file(&source).unwrap(), hash);
        fs::write(&fixture.mountinfo, "").unwrap();
        let disconnected = fixture.service.scan().unwrap();
        assert!(!disconnected[0].connected && !disconnected[0].writable);
        assert!(disconnected[0].mount_path.is_none());
        assert!(fixture.service.connected_ids().unwrap().is_empty());
        assert!(fixture.service.resolve_connected(&id).is_err());
        assert_eq!(fixture.service.inventory(&id).unwrap().len(), 1);
    }

    #[test]
    fn hash_matching_and_reconnect_invalidates_cached_presence() {
        let fixture = Fixture::new();
        let source = fixture.card.join("Matching.epub");
        epub(&source);
        let hash = Storage::hash_file(&source).unwrap();
        let repository = BookRepository::new(fixture.database.clone());
        repository
            .insert(
                "book-device",
                BookMetadata {
                    title: "Local Book".into(),
                    ..Default::default()
                },
                &[StoredFile {
                    file: BookFile {
                        id: "file-device".into(),
                        book_id: "book-device".into(),
                        format: BookFormat::Epub,
                        variant: FileVariant::Original,
                        profile: None,
                        size_bytes: fs::metadata(&source).unwrap().len(),
                        sha256: hash,
                        created_at: Utc::now().to_rfc3339(),
                    },
                    relative_path: "originals/fixture.epub".into(),
                }],
                None,
            )
            .unwrap();
        let id = fixture.id();
        assert_eq!(
            fixture.service.index(&id).unwrap()[0].book_id.as_deref(),
            Some("book-device")
        );
        assert_eq!(fixture.service.scan().unwrap()[0].matched_book_count, 1);
        fixture
            .database
            .connect()
            .unwrap()
            .execute(
                "UPDATE device_books SET book_id = NULL WHERE device_id = ?1",
                [&id],
            )
            .unwrap();
        assert_eq!(
            fixture
                .service
                .inventory_page(&id, 0, 20_000, true)
                .unwrap()
                .total,
            0
        );
        assert_eq!(
            fixture.service.inventory(&id).unwrap()[0]
                .book_id
                .as_deref(),
            Some("book-device")
        );
        fixture
            .service
            .reconcile_import(&id, "book-device")
            .unwrap();
        fixture
            .service
            .reconcile_import(&id, "book-device")
            .unwrap();
        assert_eq!(fixture.service.scan().unwrap()[0].matched_book_count, 1);
        assert_eq!(
            repository
                .get("book-device", std::slice::from_ref(&id))
                .unwrap()
                .on_device_ids,
            std::slice::from_ref(&id)
        );
        fixture.mount(43, "rw");
        assert_eq!(fixture.service.scan().unwrap()[0].id, id);
        assert!(fixture.service.inventory(&id).unwrap().is_empty());
        assert!(
            repository
                .get("book-device", std::slice::from_ref(&id))
                .unwrap()
                .on_device_ids
                .is_empty()
        );
        fixture.service.index(&id).unwrap();
        assert_eq!(fixture.service.inventory(&id).unwrap().len(), 1);
    }

    #[test]
    fn symlink_files_and_directories_are_not_indexed() {
        let fixture = Fixture::new();
        let outside = fixture.media.parent().unwrap().join("outside");
        fs::create_dir(&outside).unwrap();
        epub(&outside.join("Private.epub"));
        symlink(outside.join("Private.epub"), fixture.card.join("Link.epub")).unwrap();
        symlink(&outside, fixture.card.join("Linked folder")).unwrap();
        fs::write(fixture.card.join("Unknown.pdf"), b"test pdf").unwrap();
        let inventory = fixture.service.index(&fixture.id()).unwrap();
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].relative_path, "Unknown.pdf");
    }

    #[test]
    fn root_replacement_cannot_reuse_an_existing_mount_capability() {
        let fixture = Fixture::new();
        let id = fixture.id();
        fs::rename(&fixture.card, fixture.media.join("old-card")).unwrap();
        fs::create_dir(&fixture.card).unwrap();
        assert!(matches!(
            fixture.service.resolve_connected(&id),
            Err(AppError::Conflict(_))
        ));
        fixture.mount(44, "rw");
        assert_eq!(
            fixture.service.resolve_connected(&id).unwrap(),
            fixture.card
        );
    }

    #[test]
    fn read_only_media_system_mounts_and_bind_mounts_are_filtered() {
        let fixture = Fixture::new();
        fixture.mount(42, "ro");
        let input = fs::read_to_string(&fixture.mountinfo).unwrap();
        fs::write(&fixture.mountinfo, format!(
            "{input}1 0 8:0 / / rw - ext4 /dev/root rw\n99 1 8:1 /home/rony {} rw - ext4 /dev/root rw\n",
            escape_path(&fixture.media.join("bound-home"))
        )).unwrap();
        let devices = fixture.service.scan().unwrap();
        assert_eq!(devices.len(), 1);
        assert!(devices[0].connected);
        assert!(!devices[0].writable);
    }

    #[test]
    fn gvfs_only_indexes_mounted_mtp_children() {
        let fixture = Fixture::new();
        // The logical GVFS transport name is not a legal Windows directory name.
        let mtp = fixture.gvfs.join("mtp-child");
        let remote = fixture.gvfs.join("remote-child");
        fs::create_dir_all(&mtp).unwrap();
        fs::create_dir_all(&remote).unwrap();
        fs::write(mtp.join("Mobile.txt"), b"mobile reader").unwrap();
        fs::write(remote.join("Private.txt"), b"remote document").unwrap();
        fs::write(
            &fixture.mountinfo,
            format!(
                "77 1 0:22 / {} rw - fuse.gvfsd-fuse gvfsd-fuse rw\n",
                escape_path(&fixture.gvfs)
            ),
        )
        .unwrap();
        let mount = parse_mountinfo(&fs::read_to_string(&fixture.mountinfo).unwrap())
            .unwrap()
            .remove(0);
        let mut capabilities = BTreeMap::new();
        fixture
            .service
            .insert_gvfs_children(
                &mut capabilities,
                &mount,
                [
                    Ok(("mtp:host=Reader_SERIAL".into(), mtp.clone())),
                    Ok(("sftp:host=remote".into(), remote.clone())),
                ],
            )
            .unwrap();
        assert_eq!(capabilities.len(), 1);
        let capability = capabilities.values().next().unwrap();
        assert_eq!(capability.path, mtp);
        assert_eq!(capability.stable_identity, "mtp:mtp:host=Reader_SERIAL");
        let mut selected_file =
            open_device_file(&capability.path.join("Mobile.txt"), &capability.path).unwrap();
        let mut contents = String::new();
        selected_file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "mobile reader");
        assert!(!capability.path.join("Private.txt").exists());
        // Linux/macOS can additionally exercise actual GVFS names through read_dir
        // and the complete public indexing path; the common selection/read assertions
        // above run on Windows as well, without skipping this test.
        #[cfg(unix)]
        {
            fs::rename(&mtp, fixture.gvfs.join("mtp:host=Reader_SERIAL")).unwrap();
            fs::rename(&remote, fixture.gvfs.join("sftp:host=remote")).unwrap();
            let devices = fixture.service.scan().unwrap();
            assert_eq!(devices.len(), 1);
            assert_eq!(devices[0].transport, DeviceTransport::Usb);
            assert_eq!(
                fixture.service.index(&devices[0].id).unwrap()[0].relative_path,
                "Mobile.txt"
            );
        }
    }

    #[test]
    fn gvfs_selection_rejects_unmounted_roots_outside_children_and_links() {
        let fixture = Fixture::new();
        let mut mount = parse_mountinfo(&fs::read_to_string(&fixture.mountinfo).unwrap())
            .unwrap()
            .remove(0);
        let child = fixture.gvfs.join("child");
        fs::create_dir(&child).unwrap();
        let mut found = BTreeMap::new();
        fixture
            .service
            .insert_gvfs_children(
                &mut found,
                &mount,
                [Ok(("mtp:host=Reader".into(), child.clone()))],
            )
            .unwrap();
        assert!(found.is_empty());
        mount.filesystem = "fuse.gvfsd-fuse".into();
        mount.mount_point = fixture.gvfs.clone();
        assert!(
            fixture
                .service
                .insert_gvfs_children(
                    &mut found,
                    &mount,
                    [Ok(("mtp:host=Reader".into(), fixture.card.clone()))]
                )
                .is_err()
        );
        let link = fixture.gvfs.join("link");
        symlink(&fixture.card, &link).unwrap();
        fixture
            .service
            .insert_gvfs_children(&mut found, &mount, [Ok(("mtp:host=Reader".into(), link))])
            .unwrap();
        assert!(found.is_empty());
        assert!(
            fixture
                .service
                .insert_gvfs_children(
                    &mut found,
                    &mount,
                    [Err(std::io::Error::other("Enumeration failed"))]
                )
                .is_err()
        );
    }

    #[test]
    fn corrupt_epub_is_visible_with_warning_and_complete_index_is_idempotent() {
        let fixture = Fixture::new();
        fs::write(fixture.card.join("Broken.epub"), b"not a zip").unwrap();
        let id = fixture.id();
        let first = fixture.service.index(&id).unwrap();
        assert_eq!(first[0].title, "Broken");
        assert_eq!(first[0].warnings, ["metadataUnavailable"]);
        assert!(first[0].sha256.is_some());
        fixture.service.index(&id).unwrap();
        assert_eq!(fixture.service.inventory(&id).unwrap().len(), 1);
        assert_eq!(fixture.service.scan().unwrap()[0].book_count, 1);
    }

    #[test]
    fn mount_field_decoding_preserves_unicode_and_rejects_invalid_escapes() {
        assert_eq!(
            decode_mount_field(r"/media/Livres\040é\134x").unwrap(),
            "/media/Livres é\\x"
        );
        assert!(decode_mount_field(r"/media/\999").is_err());
        assert!(decode_mount_field(r"/media/\777").is_err());
        assert_eq!(parse_mountinfo("bad line").unwrap().len(), 0);
    }

    #[test]
    fn incomplete_deep_inventory_preserves_the_previous_complete_index() {
        let fixture = Fixture::new();
        fs::write(fixture.card.join("Known.txt"), b"known book").unwrap();
        let id = fixture.id();
        fixture.service.index(&id).unwrap();
        let mut deep = fixture.card.clone();
        for _ in 0..MAX_INDEX_DEPTH {
            deep.push("nested");
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("Deep.txt"), b"deep book").unwrap();
        assert!(matches!(
            fixture.service.index(&id),
            Err(AppError::Unsupported(_))
        ));
        assert_eq!(
            fixture.service.inventory(&id).unwrap()[0].relative_path,
            "Known.txt"
        );
    }

    #[test]
    fn progress_counts_actual_chunks_and_publishes_staging_before_commit() {
        let fixture = Fixture::new();
        let bytes = vec![b'x'; 3 * 256 * 1024 + 7];
        fs::write(fixture.card.join("Large.txt"), &bytes).unwrap();
        let id = fixture.id();
        let mut samples = Vec::new();
        fixture
            .service
            .index_with_progress(&id, Arc::new(AtomicBool::new(false)), |progress| {
                if progress.processed_books == 1 && progress.phase == "reading" {
                    let page = fixture.service.inventory_page(&id, 0, 100, true)?;
                    assert_eq!(page.total, 1);
                    assert!(page.items[0].sha256.is_some());
                    let connection = fixture.database.connect()?;
                    let count: i64 =
                        connection
                            .query_row("SELECT count(*) FROM device_books", [], |row| row.get(0))?;
                    assert_eq!(count, 0, "staging is visible before atomic publication");
                    assert_eq!(
                        fixture.service.resolve_import_path(&id, "Large.txt")?,
                        fixture.card.join("Large.txt")
                    );
                }
                samples.push(progress);
                Ok(())
            })
            .unwrap();
        let reading: Vec<_> = samples
            .iter()
            .filter(|sample| sample.phase == "reading")
            .collect();
        assert!(
            reading
                .iter()
                .all(|sample| sample.total_bytes == bytes.len() as u64)
        );
        let increments: Vec<_> = reading
            .windows(2)
            .filter(|pair| pair[1].bytes_read > pair[0].bytes_read)
            .map(|pair| pair[1].bytes_read - pair[0].bytes_read)
            .collect();
        assert_eq!(increments, [256 * 1024, 256 * 1024, 256 * 1024, 7]);
        assert_eq!(samples.last().unwrap().bytes_read, bytes.len() as u64);
        assert_eq!(samples.last().unwrap().phase, "finalizing");
    }

    #[test]
    fn cancellation_discards_staging_and_preserves_previous_complete_inventory() {
        let fixture = Fixture::new();
        fs::write(fixture.card.join("Known.txt"), b"known").unwrap();
        let id = fixture.id();
        fixture.service.index(&id).unwrap();
        fs::write(fixture.card.join("Large.txt"), vec![b'x'; 1024 * 1024]).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        let result = fixture
            .service
            .index_with_progress(&id, cancelled, |progress| {
                if progress.phase == "reading" && progress.bytes_read > 256 * 1024 {
                    signal.store(true, Ordering::Relaxed);
                }
                Ok(())
            });
        assert!(matches!(result, Err(AppError::Cancelled)));
        assert!(
            !fixture
                .service
                .lock_inventories()
                .unwrap()
                .contains_key(&id)
        );
        let inventory = fixture.service.inventory(&id).unwrap();
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].relative_path, "Known.txt");
    }

    #[test]
    fn import_resolution_refuses_unknown_traversal_symlinks_and_changed_versions() {
        let fixture = Fixture::new();
        let source = fixture.card.join("Known.txt");
        fs::write(&source, b"known").unwrap();
        let id = fixture.id();
        fixture.service.index(&id).unwrap();
        assert_eq!(
            fixture
                .service
                .resolve_import_path(&id, "Known.txt")
                .unwrap(),
            source
        );
        for unsafe_path in [
            "",
            "../Known.txt",
            "/Known.txt",
            "nested/../Known.txt",
            "Absent.txt",
        ] {
            assert!(
                fixture
                    .service
                    .resolve_import_path(&id, unsafe_path)
                    .is_err()
            );
        }
        fs::write(&source, b"changed version").unwrap();
        assert!(matches!(
            fixture.service.resolve_import_path(&id, "Known.txt"),
            Err(AppError::Conflict(_))
        ));
        fs::remove_file(&source).unwrap();
        let outside = fixture.media.join("Outside.txt");
        fs::write(&outside, b"known").unwrap();
        symlink(&outside, &source).unwrap();
        assert!(
            fixture
                .service
                .resolve_import_path(&id, "Known.txt")
                .is_err()
        );
    }

    #[test]
    fn failed_progress_callback_preserves_committed_inventory() {
        let fixture = Fixture::new();
        fs::write(fixture.card.join("Known.txt"), b"known").unwrap();
        let id = fixture.id();
        fixture.service.index(&id).unwrap();
        fs::write(fixture.card.join("New.txt"), b"new book").unwrap();
        let result = fixture.service.index_with_progress(
            &id,
            Arc::new(AtomicBool::new(false)),
            |progress| {
                if progress.phase == "finalizing" {
                    return Err(AppError::Conflict("callback failed".into()));
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(fixture.service.inventory(&id).unwrap().len(), 1);
    }

    #[test]
    fn duplicate_volume_identity_is_refused_instead_of_selecting_an_arbitrary_root() {
        let fixture = Fixture::new();
        let duplicate = fixture.media.join("Other card");
        fs::create_dir(&duplicate).unwrap();
        let mounts = fs::read_to_string(&fixture.mountinfo).unwrap();
        fs::write(
            &fixture.mountinfo,
            format!(
                "{mounts}43 1 8:2 / {} rw - vfat UUID=TEST-CARD rw\n",
                escape_path(&duplicate)
            ),
        )
        .unwrap();
        assert!(matches!(fixture.service.scan(), Err(AppError::Conflict(_))));
    }
}
