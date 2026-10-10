//! Handle-relative filesystem operations shared by storage and device transfers.
//!
//! Adapters must reject links (including Windows reparse points), preserve the
//! directory handle as the authority, and never emulate exclusive publication
//! with a check followed by an overwriting rename.

use std::{
    ffi::OsStr,
    fs::File,
    path::{Component, Path, PathBuf},
};

use crate::error::{AppError, Result};

#[cfg(unix)]
#[path = "secure_fs/unix.rs"]
mod backend;
#[cfg(windows)]
#[path = "secure_fs/windows.rs"]
// Windows handle-relative opens, ACLs and identity operations require audited
// native FFI. Keep this exception on the adapter, never on its safe callers.
#[allow(unsafe_code)]
mod backend;

#[cfg(not(any(unix, windows)))]
compile_error!("secure filesystem operations require a Unix or Windows adapter");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessPolicy {
    /// Owner-only access: Unix permissions or a protected Windows owner ACL.
    Private,
    /// Files on a reader remain accessible to the reader's operating system.
    Shared,
}

/// Identity read from an open handle, never inferred from a pathname.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    pub volume: u64,
    pub id: [u8; 16],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Timestamp {
    pub seconds: i64,
    pub nanos: u32,
}

/// `changed` is Unix ctime or Windows ChangeTime, not creation time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileSnapshot {
    pub identity: FileIdentity,
    pub size: u64,
    pub modified: Timestamp,
    pub changed: Timestamp,
}

impl FileSnapshot {
    pub fn verify(&self, file: &File) -> Result<()> {
        if snapshot(file)? != *self {
            return Err(AppError::Conflict("File changed during operation".into()));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishResult {
    Published,
    AlreadyExists,
}

/// A directory opened without following any link in its path.
#[derive(Debug)]
pub struct SecureDir {
    file: File,
}

impl SecureDir {
    pub fn open(path: &Path, create: bool, policy: AccessPolicy) -> Result<Self> {
        let path = absolute_path(path)?;
        Ok(Self {
            file: backend::open_dir(&path, create, policy)?,
        })
    }

    pub fn as_file(&self) -> &File {
        &self.file
    }

    pub fn try_clone(&self) -> Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
        })
    }

    pub fn child(&self, name: &OsStr, create: bool, policy: AccessPolicy) -> Result<Self> {
        validate_component(name)?;
        Ok(Self {
            file: backend::child(&self.file, name, create, policy)?,
        })
    }

    pub fn identity(&self) -> Result<FileIdentity> {
        identity(&self.file)
    }

    pub fn open_regular(&self, name: &OsStr) -> Result<File> {
        validate_component(name)?;
        backend::open_regular(&self.file, name)
    }

    /// Opens a principal file while preventing its replacement on Windows.
    /// SQLite sidecars must use `open_regular` so their cleanup remains possible.
    pub fn open_pinned_regular(&self, name: &OsStr) -> Result<File> {
        validate_component(name)?;
        backend::open_pinned_regular(&self.file, name)
    }

    /// Creates a new regular file exclusively, with its access policy in force
    /// before any bytes are written. Existing files are never truncated.
    pub fn create_new(&self, name: &OsStr, policy: AccessPolicy) -> Result<File> {
        validate_component(name)?;
        backend::create_new(&self.file, name, policy)
    }

    /// Atomically publishes the staged file without replacing a destination.
    /// The adapter also checks that `staging_name` still names `staged`.
    pub fn publish_noreplace(
        &self,
        staged: &File,
        staging_name: &OsStr,
        target: &OsStr,
    ) -> Result<PublishResult> {
        validate_component(staging_name)?;
        validate_component(target)?;
        backend::publish_noreplace(&self.file, staged, staging_name, target)
    }

    /// Removes only the regular file with the expected identity. A missing or
    /// replaced entry returns false; links are rejected without being removed.
    pub fn remove_if_identity(&self, name: &OsStr, expected: FileIdentity) -> Result<bool> {
        validate_component(name)?;
        backend::remove_if_identity(&self.file, name, expected)
    }

    pub fn free_space(&self) -> Result<u64> {
        backend::free_space(&self.file)
    }

    /// Requests the adapter's directory durability barrier. Unsupported
    /// filesystem guarantees must return an error rather than silent success.
    pub fn sync(&self) -> Result<()> {
        backend::sync_directory(&self.file)
    }
}

pub fn identity(file: &File) -> Result<FileIdentity> {
    backend::identity(file)
}

pub fn snapshot(file: &File) -> Result<FileSnapshot> {
    backend::snapshot(file)
}

pub fn make_private(file: &File, read_only: bool) -> Result<()> {
    backend::make_private(file, read_only)
}

pub fn is_private_read_only(file: &File) -> Result<bool> {
    backend::is_private_read_only(file)
}

/// Checks an existing mutable private file without repairing its permissions.
/// Includes ownership and rejects readonly files or access by other users.
pub fn is_private_read_write(file: &File) -> Result<bool> {
    backend::is_private_read_write(file)
}

/// Makes a dedicated profile private, including owner-only inheritance for
/// files that SQLite creates itself. It does not grant access to other users.
pub fn make_private_inheritable(directory: &File) -> Result<()> {
    backend::make_private_inheritable(directory)
}

/// Checks the private profile policy without changing permissions.
pub fn is_private_inheritable(directory: &File) -> Result<bool> {
    backend::is_private_inheritable(directory)
}

/// Accepts only owner-private mutable SQLite files, including files whose
/// exact owner-only ACL was inherited from the validated profile directory.
/// The caller must retain and validate that directory's private policy.
pub fn is_private_sqlite_file(file: &File) -> Result<bool> {
    backend::is_private_sqlite_file(file)
}

/// Resolve a relative input against cwd without normalizing away traversal.
/// Windows drive and UNC roots are supported, while device namespaces and
/// drive-relative paths are rejected.
pub fn absolute_path(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        #[cfg(windows)]
        if path
            .components()
            .any(|part| matches!(part, Component::Prefix(_)))
        {
            return Err(invalid_path());
        }
        std::env::current_dir()?.join(path)
    };
    for component in path.components() {
        match component {
            Component::ParentDir => return Err(invalid_path()),
            Component::Normal(name) => validate_component(name)?,
            #[cfg(windows)]
            Component::Prefix(prefix) => {
                use std::path::Prefix;
                if !matches!(
                    prefix.kind(),
                    Prefix::Disk(_)
                        | Prefix::VerbatimDisk(_)
                        | Prefix::UNC(_, _)
                        | Prefix::VerbatimUNC(_, _)
                ) {
                    return Err(invalid_path());
                }
            }
            #[cfg(not(windows))]
            Component::Prefix(_) => return Err(invalid_path()),
            Component::RootDir | Component::CurDir => {}
        }
    }
    Ok(path)
}

fn invalid_path() -> AppError {
    AppError::InvalidInput("Unsafe filesystem path".into())
}

fn validate_component(name: &OsStr) -> Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(value)) if value == name)
        || components.next().is_some()
    {
        return Err(invalid_path());
    }
    if name.as_encoded_bytes().contains(&0) {
        return Err(invalid_path());
    }
    #[cfg(windows)]
    {
        let name = name.to_str().ok_or_else(invalid_path)?;
        if name.ends_with(['.', ' '])
            || name
                .chars()
                .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        {
            return Err(invalid_path());
        }
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(invalid_path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_relative_operations_accept_only_one_normal_component() {
        for invalid in ["", ".", "..", "../book", "folder/book", "book\0.epub"] {
            assert!(
                validate_component(OsStr::new(invalid)).is_err(),
                "{invalid:?}"
            );
        }
        assert!(validate_component(OsStr::new("Édition ISBN.epub")).is_ok());
    }

    #[test]
    fn absolute_paths_do_not_erase_parent_traversal() {
        assert!(absolute_path(Path::new("folder/../book.epub")).is_err());
        assert!(absolute_path(Path::new("book.epub")).unwrap().is_absolute());
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_reject_device_namespaces_and_alternate_streams() {
        for invalid in [
            r"\\.\PhysicalDrive0",
            r"C:book.epub",
            r"C:\books\file:stream",
            r"C:\books\NUL.epub",
            r"C:\books\file.",
        ] {
            assert!(absolute_path(Path::new(invalid)).is_err(), "{invalid:?}");
        }
        assert!(absolute_path(Path::new(r"C:\books\Édition.epub")).is_ok());
        assert!(absolute_path(Path::new(r"\\server\share\books\book.epub")).is_ok());
    }
}
