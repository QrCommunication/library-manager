use std::fs::{self, File, Metadata};
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rustix::fd::OwnedFd;
use rustix::fs::{
    AtFlags, Mode, OFlags, RenameFlags, mkdirat, open, openat, renameat_with, unlinkat,
};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::error::{AppError, Result};
use crate::models::{BookFormat, BookMetadata};

pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_READ_BYTES: u64 = 64 * 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const OWNERSHIP_MARKER: &str = ".library-manager-storage";
const MARKER_CONTENT: &[u8] = b"Library Manager managed storage v1\n";
const STAGING_PREFIX: &str = ".library-manager-stage-";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredArtifact {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

pub type StoredOriginal = StoredArtifact;

#[derive(Debug, Clone)]
pub struct Storage {
    root: PathBuf,
    root_fd: Arc<OwnedFd>,
    identity: FileIdentity,
}

impl Storage {
    pub fn new(root: &Path) -> Result<Self> {
        if root.as_os_str().is_empty() {
            return Err(AppError::InvalidInput(
                "The storage directory is empty".into(),
            ));
        }
        let absolute = absolute_path(root)?;
        if absolute
            .components()
            .filter(|part| matches!(part, Component::Normal(_)))
            .count()
            == 0
        {
            return Err(AppError::InvalidInput(
                "The filesystem root cannot be managed storage".into(),
            ));
        }
        let root_fd = open_absolute_directory(&absolute, true)?;
        let directory = File::from(root_fd.try_clone()?);
        let identity = FileIdentity::from_metadata(&directory.metadata()?);
        let canonical = fs::canonicalize(&absolute)?;
        if FileIdentity::from_metadata(&fs::symlink_metadata(&canonical)?) != identity {
            return Err(AppError::Conflict(
                "The storage directory changed during opening".into(),
            ));
        }
        ensure_owned_directory(&root_fd, &canonical)?;
        make_private(&directory, 0o700)?;
        directory.sync_all()?;
        Ok(Self {
            root: canonical,
            root_fd: Arc::new(root_fd),
            identity,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve(&self, relative: &str) -> Result<PathBuf> {
        let components = relative_components(relative)?;
        self.verify_root()?;
        let mut candidate = self.root.clone();
        let mut missing = false;
        for (index, component) in components.iter().enumerate() {
            candidate.push(component);
            if missing {
                continue;
            }
            match fs::symlink_metadata(&candidate) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        return Err(AppError::InvalidInput(
                            "Symbolic links are not allowed in managed paths".into(),
                        ));
                    }
                    if index + 1 < components.len() && !metadata.is_dir() {
                        return Err(AppError::InvalidInput(
                            "A managed path parent is not a directory".into(),
                        ));
                    }
                    if !fs::canonicalize(&candidate)?.starts_with(&self.root) {
                        return Err(AppError::InvalidInput(
                            "The path escapes managed storage".into(),
                        ));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => missing = true,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(candidate)
    }

    pub fn import_original(&self, source: &Path, format: BookFormat) -> Result<StoredOriginal> {
        let parent = self.parent_for("originals/pending", true)?;
        let mut stage = Stage::new(parent)?;
        let copied = copy_source(source, &mut stage.file)?;
        let relative = format!("originals/{}.{}", copied.sha256, format.as_str());
        let target = format!("{}.{}", copied.sha256, format.as_str());
        self.finish(stage, &target, &relative, copied, true)
    }

    pub fn write_new(&self, relative: &str, bytes: &[u8]) -> Result<StoredArtifact> {
        validate_publish_path(relative)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(AppError::Unsupported(
                "The generated file exceeds the storage size limit".into(),
            ));
        }
        let parent = self.parent_for(relative, true)?;
        let mut stage = Stage::new(parent)?;
        stage.file.write_all(bytes)?;
        let copied = CopiedContent {
            sha256: hex_digest(Sha256::digest(bytes).as_slice()),
            size_bytes: bytes.len() as u64,
        };
        let target = leaf_name(relative)?;
        self.finish(stage, target, relative, copied, false)
    }

    pub fn publish_file(&self, relative: &str, source_temp: &Path) -> Result<StoredArtifact> {
        validate_publish_path(relative)?;
        let parent = self.parent_for(relative, true)?;
        let mut stage = Stage::new(parent)?;
        let copied = copy_source(source_temp, &mut stage.file)?;
        let target = leaf_name(relative)?;
        self.finish(stage, target, relative, copied, false)
    }

    pub fn read(&self, relative: &str) -> Result<Vec<u8>> {
        let parent = self.parent_for(relative, false)?;
        let mut file = open_regular_at(&parent, leaf_name(relative)?)?;
        let before = FileSnapshot::new(&file.metadata()?)?;
        if before.size > MAX_READ_BYTES {
            return Err(AppError::Unsupported(
                "The resource exceeds the in-memory read limit".into(),
            ));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(before.size as usize)
            .map_err(|_| AppError::Unsupported("Insufficient memory for the resource".into()))?;
        Read::by_ref(&mut file)
            .take(MAX_READ_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_READ_BYTES {
            return Err(AppError::Unsupported(
                "The resource grew beyond the read limit".into(),
            ));
        }
        before.verify(&file.metadata()?)?;
        verify_target_identity(&parent, leaf_name(relative)?, &before)?;
        Ok(bytes)
    }

    pub fn hash_file(path: &Path) -> Result<String> {
        hash_file(path)
    }

    pub fn managed_book_path(
        &self,
        book_id: &str,
        metadata: &BookMetadata,
        format: BookFormat,
    ) -> String {
        managed_book_path(book_id, metadata, format)
    }

    fn verify_root(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.root)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || FileIdentity::from_metadata(&metadata) != self.identity
        {
            return Err(AppError::Conflict(
                "The managed storage directory was replaced".into(),
            ));
        }
        let reopened = open_absolute_directory(&self.root, false)?;
        if FileIdentity::from_metadata(&File::from(reopened).metadata()?) != self.identity {
            return Err(AppError::Conflict(
                "The managed storage path no longer identifies the profile".into(),
            ));
        }
        Ok(())
    }

    fn parent_for(&self, relative: &str, create: bool) -> Result<OwnedFd> {
        let components = relative_components(relative)?;
        self.verify_root()?;
        let mut parent = self.root_fd.try_clone()?;
        for component in components.iter().take(components.len() - 1) {
            parent = descend_directory(&parent, Path::new(component), create)?;
        }
        Ok(parent)
    }

    fn finish(
        &self,
        stage: Stage,
        target: &str,
        relative: &str,
        copied: CopiedContent,
        original: bool,
    ) -> Result<StoredArtifact> {
        stage.file.sync_all()?;
        if original {
            make_private(&stage.file, 0o400)?;
            stage.file.sync_all()?;
        }
        self.verify_root()?;
        match renameat_with(
            &stage.parent,
            &stage.name,
            &stage.parent,
            target,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => rustix::fs::fsync(&stage.parent).map_err(std::io::Error::from)?,
            Err(Errno::EXIST) => {
                let mut existing = open_regular_at(&stage.parent, target)?;
                let before = FileSnapshot::new(&existing.metadata()?)?;
                let content = digest_reader(&mut existing, None)?;
                before.verify(&existing.metadata()?)?;
                verify_target_identity(&stage.parent, target, &before)?;
                if content.sha256 != copied.sha256 || content.size_bytes != copied.size_bytes {
                    return Err(AppError::Conflict(
                        "A different file already exists at this library path".into(),
                    ));
                }
                if original {
                    use std::os::unix::fs::PermissionsExt;
                    if existing.metadata()?.permissions().mode() & 0o777 != 0o400 {
                        return Err(AppError::Conflict(
                            "The immutable original permissions were changed".into(),
                        ));
                    }
                }
            }
            Err(Errno::NOSYS | Errno::INVAL | Errno::NOTSUP) => {
                return Err(AppError::Unsupported(
                    "The filesystem cannot publish files without replacement".into(),
                ));
            }
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
        Ok(StoredArtifact {
            relative_path: relative.into(),
            sha256: copied.sha256,
            size_bytes: copied.size_bytes,
        })
    }
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut source = Source::open(path)?;
    let copied = digest_reader(&mut source.file, None)?;
    source.verify()?;
    Ok(copied.sha256)
}

pub fn managed_book_path(book_id: &str, metadata: &BookMetadata, format: BookFormat) -> String {
    let author = if metadata.author_sort.trim().is_empty() {
        metadata
            .authors
            .first()
            .map(String::as_str)
            .unwrap_or("Unknown Author")
    } else {
        metadata.author_sort.as_str()
    };
    let author = sanitize_component(author, 96, "Unknown Author");
    let series = metadata
        .series
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let series_directory = sanitize_component(series.unwrap_or("_Standalone"), 96, "_Standalone");
    let prefix = if series.is_some() {
        metadata
            .series_index
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(series_prefix)
            .unwrap_or_default()
    } else {
        String::new()
    };
    let title = sanitize_component(&metadata.title, 120, "Untitled");
    let suffix = id_suffix(book_id);
    format!(
        "books/{author}/{series_directory}/{prefix}{title} - {suffix}.{}",
        format.as_str()
    )
}

fn series_prefix(index: f64) -> String {
    let index = if index == 0.0 { 0.0 } else { index };
    let text = index.to_string();
    let (whole, fraction) = text
        .split_once('.')
        .map_or((text.as_str(), None), |(whole, fraction)| {
            (whole, Some(fraction))
        });
    let mut prefix = format!("T{whole:0>3}");
    if let Some(fraction) = fraction {
        prefix.push('.');
        prefix.push_str(fraction);
    }
    truncate_utf8(&mut prefix, 32);
    prefix.push_str(" - ");
    prefix
}

fn id_suffix(id: &str) -> String {
    match Uuid::parse_str(id) {
        Ok(id) => id.simple().to_string()[..8].into(),
        Err(_) => hex_digest(Sha256::digest(id.as_bytes()).as_slice())[..8].into(),
    }
}

fn sanitize_component(value: &str, max_bytes: usize, fallback: &str) -> String {
    let replaced: String = value.nfc().filter(|character| !character.is_control()
        && !matches!(*character, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}'))
        .map(|character| if matches!(character, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { character })
        .collect();
    let normalized = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut component = normalized.trim_matches([' ', '.']).to_string();
    if component.is_empty() {
        component = fallback.into();
    }
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        component.insert(0, '_');
    }
    truncate_utf8(&mut component, max_bytes);
    component.trim_end_matches([' ', '.']).into()
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if absolute
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(AppError::InvalidInput(
            "Filesystem paths cannot contain parent traversal".into(),
        ));
    }
    Ok(absolute)
}

fn open_absolute_directory(path: &Path, create: bool) -> Result<OwnedFd> {
    let mut directory =
        open(Path::new("/"), directory_flags(), Mode::empty()).map_err(std::io::Error::from)?;
    for component in path.components() {
        match component {
            Component::Normal(component) => {
                directory = descend_directory(&directory, Path::new(component), create)?
            }
            Component::RootDir | Component::CurDir => {}
            _ => return Err(AppError::InvalidInput("Unsafe directory path".into())),
        }
    }
    Ok(directory)
}

fn descend_directory(parent: &OwnedFd, name: &Path, create: bool) -> Result<OwnedFd> {
    match openat(parent, name, directory_flags(), Mode::empty()) {
        Ok(directory) => Ok(directory),
        Err(Errno::NOENT) if create => {
            match mkdirat(parent, name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                Ok(()) => rustix::fs::fsync(parent).map_err(std::io::Error::from)?,
                Err(Errno::EXIST) => {}
                Err(error) => return Err(std::io::Error::from(error).into()),
            }
            Ok(openat(parent, name, directory_flags(), Mode::empty())
                .map_err(std::io::Error::from)?)
        }
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn relative_components(relative: &str) -> Result<Vec<&str>> {
    if relative.is_empty()
        || relative.len() > 4096
        || relative.contains('\\')
        || relative.chars().any(char::is_control)
        || relative
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || Path::new(relative)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(AppError::InvalidInput(
            "The path must remain relative to managed storage".into(),
        ));
    }
    Ok(relative.split('/').collect())
}

fn leaf_name(relative: &str) -> Result<&str> {
    relative_components(relative)?
        .last()
        .copied()
        .ok_or_else(|| AppError::InvalidInput("Missing filename".into()))
}

fn validate_publish_path(relative: &str) -> Result<()> {
    let parts = relative_components(relative)?;
    if parts[0] == "originals" || parts[0] == OWNERSHIP_MARKER {
        return Err(AppError::InvalidInput(
            "Immutable originals and the storage marker are reserved".into(),
        ));
    }
    Ok(())
}

fn ensure_owned_directory(directory: &OwnedFd, path: &Path) -> Result<()> {
    match open_regular_at(directory, OWNERSHIP_MARKER) {
        Ok(mut marker) => {
            if marker.metadata()?.len() != MARKER_CONTENT.len() as u64 {
                return Err(AppError::Conflict(
                    "Invalid Library Manager storage marker".into(),
                ));
            }
            let mut content = Vec::new();
            Read::by_ref(&mut marker)
                .take(MARKER_CONTENT.len() as u64 + 1)
                .read_to_end(&mut content)?;
            if content != MARKER_CONTENT {
                return Err(AppError::Conflict(
                    "The directory is not recognized as Library Manager storage".into(),
                ));
            }
            Ok(())
        }
        Err(AppError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            if fs::read_dir(path)?.next().is_some() {
                return Err(AppError::Conflict(
                    "Choose an empty dedicated library directory; existing files will be preserved"
                        .into(),
                ));
            }
            let fd = openat(
                directory,
                OWNERSHIP_MARKER,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(std::io::Error::from)?;
            let mut marker = File::from(fd);
            marker.write_all(MARKER_CONTENT)?;
            marker.sync_all()?;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn open_regular_at(parent: &OwnedFd, name: &str) -> Result<File> {
    let fd = openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let file = File::from(fd);
    if !file.metadata()?.is_file() {
        return Err(AppError::InvalidInput("A regular file is required".into()));
    }
    Ok(file)
}

fn make_private(file: &File, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (file, mode);
    Ok(())
}

struct Stage {
    parent: OwnedFd,
    name: String,
    file: File,
}

impl Stage {
    fn new(parent: OwnedFd) -> Result<Self> {
        let name = format!("{STAGING_PREFIX}{}", Uuid::new_v4());
        let fd = openat(
            &parent,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(std::io::Error::from)?;
        Ok(Self {
            parent,
            name,
            file: File::from(fd),
        })
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = unlinkat(&self.parent, &self.name, AtFlags::empty());
    }
}

struct Source {
    path: PathBuf,
    file: File,
    snapshot: FileSnapshot,
}

impl Source {
    fn open(path: &Path) -> Result<Self> {
        let absolute = absolute_path(path)?;
        let parent_path = absolute
            .parent()
            .ok_or_else(|| AppError::InvalidInput("Missing source parent".into()))?;
        let leaf = absolute
            .file_name()
            .ok_or_else(|| AppError::InvalidInput("Missing source filename".into()))?;
        let parent = open_absolute_directory(parent_path, false)?;
        let fd = openat(
            &parent,
            Path::new(leaf),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        let file = File::from(fd);
        let snapshot = FileSnapshot::new(&file.metadata()?)?;
        Ok(Self {
            path: absolute,
            file,
            snapshot,
        })
    }

    fn verify(&self) -> Result<()> {
        self.snapshot.verify(&self.file.metadata()?)?;
        let reopened = Source::open(&self.path)
            .map_err(|_| AppError::Conflict("The source path changed while reading".into()))?;
        self.snapshot.verify(&reopened.file.metadata()?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileSnapshot {
    identity: FileIdentity,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileSnapshot {
    fn new(metadata: &Metadata) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        if !metadata.is_file() {
            return Err(AppError::InvalidInput(
                "Only regular files can be stored".into(),
            ));
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(AppError::Unsupported(
                "The source exceeds the 512 MiB import limit".into(),
            ));
        }
        Ok(Self {
            identity: FileIdentity::from_metadata(metadata),
            size: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }

    fn verify(&self, metadata: &Metadata) -> Result<()> {
        if Self::new(metadata)? != *self {
            return Err(AppError::Conflict(
                "The file changed while its content was being read".into(),
            ));
        }
        Ok(())
    }
}

struct CopiedContent {
    sha256: String,
    size_bytes: u64,
}

fn copy_source(path: &Path, destination: &mut File) -> Result<CopiedContent> {
    let mut source = Source::open(path)?;
    let copied = digest_reader(&mut source.file, Some(destination))?;
    source.verify()?;
    source.file.rewind()?;
    let verified = digest_reader(&mut source.file, None)?;
    source.verify()?;
    if copied.sha256 != verified.sha256 || copied.size_bytes != verified.size_bytes {
        return Err(AppError::Conflict(
            "The source content changed during copying".into(),
        ));
    }
    Ok(copied)
}

fn digest_reader(reader: &mut File, mut destination: Option<&mut File>) -> Result<CopiedContent> {
    let mut digest = Sha256::new();
    let mut size_bytes = 0;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size_bytes += read as u64;
        if size_bytes > MAX_FILE_BYTES {
            return Err(AppError::Unsupported(
                "The file grew beyond the storage limit".into(),
            ));
        }
        digest.update(&buffer[..read]);
        if let Some(writer) = destination.as_deref_mut() {
            writer.write_all(&buffer[..read])?;
        }
    }
    Ok(CopiedContent {
        sha256: hex_digest(digest.finalize().as_slice()),
        size_bytes,
    })
}

fn verify_target_identity(parent: &OwnedFd, name: &str, expected: &FileSnapshot) -> Result<()> {
    let reopened = open_regular_at(parent, name)?;
    expected.verify(&reopened.metadata()?)
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn fixture() -> Result<(tempfile::TempDir, Storage)> {
        let temporary = tempfile::tempdir()?;
        let storage = Storage::new(&temporary.path().join("library"))?;
        Ok((temporary, storage))
    }

    fn source(temporary: &tempfile::TempDir, bytes: &[u8]) -> Result<PathBuf> {
        let path = temporary.path().join("source.epub");
        fs::write(&path, bytes)?;
        Ok(path)
    }

    #[test]
    fn original_import_is_immutable_idempotent_and_preserves_the_source() -> Result<()> {
        let (temporary, storage) = fixture()?;
        let source = source(&temporary, b"Original book bytes")?;
        let before = hash_file(&source)?;
        let imported = storage.import_original(&source, BookFormat::Epub)?;
        let repeated = storage.import_original(&source, BookFormat::Epub)?;
        assert_eq!(imported, repeated);
        assert_eq!(imported.sha256, before);
        assert_eq!(hash_file(&source)?, before);
        assert_eq!(fs::read(&source)?, b"Original book bytes");
        assert_eq!(
            storage.read(&imported.relative_path)?,
            b"Original book bytes"
        );
        assert_eq!(
            fs::metadata(storage.resolve(&imported.relative_path)?)?
                .permissions()
                .mode()
                & 0o777,
            0o400
        );
        assert!(
            storage
                .write_new(&imported.relative_path, b"Replacement")
                .is_err()
        );
        let immutable = storage.resolve(&imported.relative_path)?;
        let snapshot = FileSnapshot::new(&fs::metadata(&immutable)?)?;
        assert_eq!(
            storage.import_original(&immutable, BookFormat::Epub)?,
            imported
        );
        snapshot.verify(&fs::metadata(immutable)?)?;
        assert_eq!(Storage::new(storage.root())?.root(), storage.root());
        Ok(())
    }

    #[test]
    fn publish_is_atomic_without_overwrite_and_cleans_staging_on_conflict() -> Result<()> {
        let (temporary, storage) = fixture()?;
        let first = storage.write_new("books/Author/Saga/title.epub", b"First version")?;
        assert_eq!(
            storage.write_new(&first.relative_path, b"First version")?,
            first
        );
        assert!(matches!(
            storage.write_new(&first.relative_path, b"Second version"),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(storage.read(&first.relative_path)?, b"First version");
        let prepared = source(&temporary, b"Prepared version")?;
        let artifact = storage.publish_file("variants/book/optimized.epub", &prepared)?;
        assert_eq!(artifact.sha256, hash_file(&prepared)?);
        assert_eq!(fs::read(&prepared)?, b"Prepared version");
        let parent = storage
            .resolve("books/Author/Saga/title.epub")?
            .parent()
            .unwrap()
            .to_path_buf();
        assert!(fs::read_dir(parent)?.all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(STAGING_PREFIX)
        }));
        Ok(())
    }

    #[test]
    fn unsafe_relative_paths_and_symbolic_links_cannot_escape_the_library() -> Result<()> {
        let (temporary, storage) = fixture()?;
        for path in [
            "../outside",
            "/outside",
            "books/../outside",
            "books/./outside",
            "books//outside",
            "books\\outside",
            "books/\0outside",
        ] {
            assert!(storage.resolve(path).is_err());
            assert!(storage.write_new(path, b"Bad").is_err());
        }
        let outside = temporary.path().join("outside");
        fs::create_dir(&outside)?;
        symlink(&outside, storage.root().join("linked"))?;
        assert!(storage.resolve("linked/book.epub").is_err());
        assert!(storage.write_new("linked/book.epub", b"Bad").is_err());
        assert!(!outside.join("book.epub").exists());
        let original = source(&temporary, b"Keep")?;
        symlink(&original, storage.root().join("linked-source"))?;
        assert!(
            storage
                .import_original(&storage.root().join("linked-source"), BookFormat::Epub)
                .is_err()
        );
        assert!(hash_file(&storage.root().join("linked-source")).is_err());
        let linked_parent = temporary.path().join("linked-parent");
        symlink(temporary.path(), &linked_parent)?;
        assert!(
            storage
                .import_original(&linked_parent.join("source.epub"), BookFormat::Epub)
                .is_err()
        );
        assert_eq!(fs::read(original)?, b"Keep");
        Ok(())
    }

    #[test]
    fn existing_nonempty_directories_are_preserved_without_permission_changes() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let user_directory = temporary.path().join("user-books");
        fs::create_dir(&user_directory)?;
        fs::set_permissions(&user_directory, fs::Permissions::from_mode(0o755))?;
        fs::write(user_directory.join("existing.epub"), b"Existing user book")?;
        assert!(matches!(
            Storage::new(&user_directory),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(
            fs::metadata(&user_directory)?.permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::read(user_directory.join("existing.epub"))?,
            b"Existing user book"
        );
        Ok(())
    }

    #[test]
    fn managed_names_preserve_unicode_and_decimal_series_without_path_injection() {
        let metadata = BookMetadata {
            title: "E\u{301}popée / une: histoire?".into(),
            author_sort: "CON".into(),
            series: Some(".. / Saga\\NUL".into()),
            series_index: Some(3.5),
            ..BookMetadata::default()
        };
        let id = "12345678-1234-4234-8234-123456789abc";
        let path = managed_book_path(id, &metadata, BookFormat::Epub);
        assert!(path.starts_with("books/_CON/"));
        assert!(path.contains("T003.5 - Épopée _ une_ histoire_ - 12345678.epub"));
        assert!(relative_components(&path).is_ok());
        assert_eq!(path, managed_book_path(id, &metadata, BookFormat::Epub));
        let zero = BookMetadata {
            series_index: Some(0.0),
            ..metadata.clone()
        };
        assert!(managed_book_path(id, &zero, BookFormat::Epub).contains("T000 - "));
        let long = BookMetadata {
            title: "🦉".repeat(200),
            author_sort: "é".repeat(200),
            ..metadata
        };
        let long_path = managed_book_path("../../untrusted", &long, BookFormat::Mobi);
        assert!(long_path.split('/').all(|component| component.len() <= 200));
        assert!(relative_components(&long_path).is_ok());
    }

    #[test]
    fn a_changed_source_and_replaced_storage_root_are_refused() -> Result<()> {
        let (temporary, storage) = fixture()?;
        let path = source(&temporary, b"Before")?;
        let opened = Source::open(&path)?;
        // Equal-size writes may share a filesystem clock tick; this exercises the metadata guard.
        fs::write(&path, b"After a deliberate size change")?;
        assert!(matches!(opened.verify(), Err(AppError::Conflict(_))));
        let old_root = temporary.path().join("old-library");
        fs::rename(storage.root(), &old_root)?;
        fs::create_dir(storage.root())?;
        assert!(matches!(
            storage.write_new("book.epub", b"Bad"),
            Err(AppError::Conflict(_))
        ));
        assert!(!storage.root().join("book.epub").exists());
        Ok(())
    }

    #[test]
    fn repeated_reads_detect_source_content_changes_of_the_same_size() -> Result<()> {
        let (temporary, _storage) = fixture()?;
        let path = source(&temporary, b"Before")?;
        let mut opened = Source::open(&path)?;
        let first = digest_reader(&mut opened.file, None)?;
        fs::write(&path, b"After!")?;
        opened.file.rewind()?;
        let second = digest_reader(&mut opened.file, None)?;
        assert_eq!(first.size_bytes, second.size_bytes);
        assert_ne!(first.sha256, second.sha256);
        assert_eq!(
            first.sha256,
            hex_digest(Sha256::digest(b"Before").as_slice())
        );
        assert_eq!(
            second.sha256,
            hex_digest(Sha256::digest(b"After!").as_slice())
        );
        Ok(())
    }

    #[test]
    fn oversize_files_are_rejected_before_copying() -> Result<()> {
        let (temporary, storage) = fixture()?;
        let path = source(&temporary, b"")?;
        File::options()
            .write(true)
            .open(&path)?
            .set_len(MAX_FILE_BYTES + 1)?;
        assert!(matches!(
            storage.import_original(&path, BookFormat::Epub),
            Err(AppError::Unsupported(_))
        ));
        assert!(hash_file(&path).is_err());
        assert!(!storage.root().join("originals").read_dir()?.any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(STAGING_PREFIX)
        }));
        Ok(())
    }

    #[test]
    fn an_unpublishable_target_leaves_no_partial_file() -> Result<()> {
        let (_temporary, storage) = fixture()?;
        fs::create_dir(storage.root().join("destination.epub"))?;
        assert!(
            storage
                .write_new("destination.epub", b"Prepared bytes")
                .is_err()
        );
        assert!(storage.root().join("destination.epub").is_dir());
        assert!(fs::read_dir(storage.root())?.all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(STAGING_PREFIX)
        }));
        Ok(())
    }

    #[test]
    fn streamed_hash_matches_the_standard_sha256_vector() -> Result<()> {
        let (temporary, _storage) = fixture()?;
        let path = source(&temporary, b"abc")?;
        assert_eq!(
            hash_file(&path)?,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        Ok(())
    }
}
