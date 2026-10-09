//! Discovery and read-only indexing of already mounted Linux ebook devices.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Take};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::Utc;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use rustix::fs::{Access, access, statvfs};
use serde::Serialize;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::database::Database;
#[cfg(test)]
use crate::epub::EpubDocument;
use crate::error::{AppError, Result};
use crate::models::{BookFormat, Device};
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
    versions: BTreeMap<String, fs::Metadata>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct RootIdentity {
    device: u64,
    inode: u64,
}

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
    mountinfo: PathBuf,
    media_roots: Vec<PathBuf>,
    gvfs_roots: Vec<PathBuf>,
}

/// Linux mount discovery has no external program or automount dependency.
#[derive(Clone, Debug)]
pub struct DeviceService {
    database: Database,
    source: Arc<MountSource>,
    active: Arc<Mutex<BTreeMap<String, MountCapability>>>,
    inventories: Arc<Mutex<BTreeMap<String, InventorySnapshot>>>,
}

impl DeviceService {
    pub fn new(database: Database) -> Self {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| {
                PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw()))
            });
        Self {
            database,
            source: Arc::new(MountSource {
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
                .and_then(|mounted| statvfs(&mounted.path).ok())
                .map(|stats| {
                    (
                        stats.f_blocks.checked_mul(stats.f_frsize),
                        stats.f_bavail.checked_mul(stats.f_frsize),
                    )
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
            let metadata = fs::symlink_metadata(entry.path())?;
            discovered_bytes = discovered_bytes
                .checked_add(metadata.len())
                .filter(|bytes| *bytes <= MAX_INDEX_BYTES)
                .ok_or_else(|| {
                    AppError::Unsupported("Device inventory byte limit exceeded".into())
                })?;
            if metadata.len() <= MAX_FILE_BYTES {
                progress.total_bytes += metadata.len();
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
            if !same_file_version(&metadata, &fs::symlink_metadata(&path)?) {
                return Err(AppError::Conflict(
                    "A device file changed during indexing".into(),
                ));
            }
            let relative_path = relative_utf8_path(&path, &capability.path)?;
            progress.current_path = Some(relative_path.clone());
            let size_bytes = metadata.len();
            let mut warnings = Vec::new();
            let sha256 = if size_bytes <= MAX_FILE_BYTES {
                let mut file = fs::File::open(&path)?;
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
                match crate::epub::read_inventory_metadata(&path) {
                    Ok(metadata) => {
                        title = metadata.title;
                        authors = metadata.authors;
                    }
                    Err(_) => warnings.push("metadataUnavailable".into()),
                }
            }
            if !same_file_version(&metadata, &fs::symlink_metadata(&path)?) {
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
        let current = fs::symlink_metadata(&path)?;
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
                    Ok(same_file_version(version, &current))
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
            if current.len() != positive_integer(size)?
                || crate::storage::Storage::hash_file(&path)? != expected
                || !same_file_version(&current, &fs::symlink_metadata(&path)?)
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
                for child in children.take(MAX_MOUNTS) {
                    let child = child?;
                    if !child.file_name().to_string_lossy().starts_with("mtp:host=") {
                        continue;
                    }
                    let stable = format!("mtp:{}", child.file_name().to_string_lossy());
                    self.insert_capability(&mut result, &mount, child.path(), stable)?;
                }
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
        let writable = mount.options.iter().any(|option| option == "rw")
            && access(&path, Access::WRITE_OK).is_ok();
        let capability = MountCapability {
            id: id.clone(),
            label,
            profile,
            mount: mount.clone(),
            path,
            identity,
            stable_identity,
            writable,
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
                mountinfo: mountinfo.into(),
                media_roots: vec![media.into()],
                gvfs_roots: vec![gvfs.into()],
            }),
            active: Arc::new(Mutex::new(BTreeMap::new())),
            inventories: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
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
    validate_no_symlinks(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || fs::canonicalize(path)? != path {
        return Err(AppError::InvalidInput(
            "Device root must be a canonical directory".into(),
        ));
    }
    Ok(RootIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn require_root(path: &Path, expected: &RootIdentity) -> Result<()> {
    if directory_identity(path)? != *expected {
        return Err(AppError::Conflict("The mounted device root changed".into()));
    }
    Ok(())
}

fn validate_no_symlinks(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(AppError::InvalidInput(
            "Device path must be absolute".into(),
        ));
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Normal(_) => current.push(component),
            _ => {
                return Err(AppError::InvalidInput(
                    "Unsafe device path component".into(),
                ));
            }
        }
        if fs::symlink_metadata(&current)?.file_type().is_symlink() {
            return Err(AppError::InvalidInput(
                "Device symlinks are not followed".into(),
            ));
        }
    }
    Ok(())
}

fn validate_contained_file(path: &Path, root: &Path) -> Result<()> {
    validate_no_symlinks(path)?;
    if !path.starts_with(root)
        || !fs::symlink_metadata(path)?.is_file()
        || !fs::canonicalize(path)?.starts_with(root)
    {
        return Err(AppError::InvalidInput(
            "Device file escapes its mounted root".into(),
        ));
    }
    Ok(())
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
        fs::symlink_metadata(path.join(name))
            .is_ok_and(|metadata| !metadata.file_type().is_symlink())
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

fn same_file_version(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    after.is_file()
        && before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book_repository::{BookRepository, StoredFile};
    use crate::models::{BookFile, BookMetadata, DeviceTransport, FileVariant};
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

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
        assert!(devices[0].total_bytes.is_some());
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
        let mtp = fixture.gvfs.join("mtp:host=Reader_SERIAL");
        let remote = fixture.gvfs.join("sftp:host=remote");
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
        let devices = fixture.service.scan().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].transport, DeviceTransport::Usb);
        assert_eq!(
            fixture.service.index(&devices[0].id).unwrap()[0].relative_path,
            "Mobile.txt"
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
