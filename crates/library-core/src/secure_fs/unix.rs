use std::{
    ffi::OsStr,
    fs::{File, Permissions},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path},
};

use rustix::{
    fs::{
        AtFlags, Mode, OFlags, RenameFlags, fstatvfs, mkdirat, open, openat, renameat_with,
        unlinkat,
    },
    io::Errno,
};

use crate::error::{AppError, Result};

use super::{AccessPolicy, FileIdentity, FileSnapshot, PublishResult, Timestamp};

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

pub(super) fn open_dir(path: &Path, create: bool, policy: AccessPolicy) -> Result<File> {
    let mut directory = File::from(
        open(Path::new("/"), directory_flags(), Mode::empty()).map_err(std::io::Error::from)?,
    );
    for component in path.components() {
        match component {
            Component::Normal(name) => directory = child(&directory, name, create, policy)?,
            Component::RootDir | Component::CurDir => {}
            _ => return Err(AppError::InvalidInput("Unsafe directory path".into())),
        }
    }
    Ok(directory)
}

pub(super) fn child(
    parent: &File,
    name: &OsStr,
    create: bool,
    policy: AccessPolicy,
) -> Result<File> {
    match openat(parent, Path::new(name), directory_flags(), Mode::empty()) {
        Ok(directory) => Ok(File::from(directory)),
        Err(Errno::NOENT) if create => {
            let mode = match policy {
                AccessPolicy::Private => 0o700,
                AccessPolicy::Shared => 0o755,
            };
            match mkdirat(parent, Path::new(name), Mode::from_raw_mode(mode)) {
                Ok(()) => sync_directory(parent)?,
                Err(Errno::EXIST) => {}
                Err(error) => return Err(std::io::Error::from(error).into()),
            }
            Ok(File::from(
                openat(parent, Path::new(name), directory_flags(), Mode::empty())
                    .map_err(std::io::Error::from)?,
            ))
        }
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

pub(super) fn open_regular(parent: &File, name: &OsStr) -> Result<File> {
    let file = File::from(
        openat(
            parent,
            Path::new(name),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    require_regular(&file)?;
    Ok(file)
}

pub(super) fn open_pinned_regular(parent: &File, name: &OsStr) -> Result<File> {
    // Unix has no Windows-style deny-delete sharing. Retain the descriptor and
    // validate the pathname identity at each database connection boundary.
    open_regular(parent, name)
}

pub(super) fn create_new(parent: &File, name: &OsStr, policy: AccessPolicy) -> Result<File> {
    let mode = match policy {
        AccessPolicy::Private => 0o600,
        AccessPolicy::Shared => 0o644,
    };
    Ok(File::from(
        openat(
            parent,
            Path::new(name),
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(mode),
        )
        .map_err(std::io::Error::from)?,
    ))
}

fn identity_from_metadata(metadata: &std::fs::Metadata) -> FileIdentity {
    let mut id = [0; 16];
    id[..8].copy_from_slice(&metadata.ino().to_le_bytes());
    FileIdentity {
        volume: metadata.dev(),
        id,
    }
}

pub(super) fn identity(file: &File) -> Result<FileIdentity> {
    Ok(identity_from_metadata(&file.metadata()?))
}

fn require_regular(file: &File) -> Result<std::fs::Metadata> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(AppError::InvalidInput(
            "Only regular files are supported".into(),
        ));
    }
    Ok(metadata)
}

fn timestamp(seconds: i64, nanos: i64) -> Result<Timestamp> {
    let nanos = u32::try_from(nanos)
        .ok()
        .filter(|value| *value < 1_000_000_000)
        .ok_or_else(|| AppError::Unsupported("Invalid filesystem timestamp".into()))?;
    Ok(Timestamp { seconds, nanos })
}

pub(super) fn snapshot(file: &File) -> Result<FileSnapshot> {
    let metadata = require_regular(file)?;
    Ok(FileSnapshot {
        identity: identity_from_metadata(&metadata),
        size: metadata.len(),
        modified: timestamp(metadata.mtime(), metadata.mtime_nsec())?,
        changed: timestamp(metadata.ctime(), metadata.ctime_nsec())?,
    })
}

pub(super) fn publish_noreplace(
    parent: &File,
    staged: &File,
    staging_name: &OsStr,
    target: &OsStr,
) -> Result<PublishResult> {
    require_regular(staged)?;
    let current = open_regular(parent, staging_name)?;
    if identity(&current)? != identity(staged)? {
        return Err(AppError::Conflict("Staged file identity changed".into()));
    }
    match renameat_with(
        parent,
        Path::new(staging_name),
        parent,
        Path::new(target),
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => Ok(PublishResult::Published),
        Err(Errno::EXIST) => Ok(PublishResult::AlreadyExists),
        Err(Errno::NOSYS | Errno::INVAL | Errno::NOTSUP) => Err(AppError::Unsupported(
            "The filesystem cannot publish files without replacement".into(),
        )),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

pub(super) fn remove_if_identity(
    parent: &File,
    name: &OsStr,
    expected: FileIdentity,
) -> Result<bool> {
    let current = match open_regular(parent, name) {
        Ok(file) => file,
        Err(AppError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    if identity(&current)? != expected {
        return Ok(false);
    }
    match unlinkat(parent, Path::new(name), AtFlags::empty()) {
        Ok(()) => Ok(true),
        Err(Errno::NOENT) => Ok(false),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

pub(super) fn free_space(parent: &File) -> Result<u64> {
    let stats = fstatvfs(parent).map_err(std::io::Error::from)?;
    stats
        .f_bavail
        .checked_mul(stats.f_frsize)
        .ok_or_else(|| AppError::Unsupported("Filesystem capacity exceeds supported range".into()))
}

pub(super) fn make_private(file: &File, read_only: bool) -> Result<()> {
    let metadata = file.metadata()?;
    let mode = if metadata.is_dir() {
        0o700
    } else if metadata.is_file() {
        if read_only { 0o400 } else { 0o600 }
    } else {
        return Err(AppError::InvalidInput(
            "Unsupported private file type".into(),
        ));
    };
    file.set_permissions(Permissions::from_mode(mode))?;
    file.sync_all()?;
    Ok(())
}

pub(super) fn is_private_read_only(file: &File) -> Result<bool> {
    let metadata = require_regular(file)?;
    Ok(metadata.permissions().mode() & 0o777 == 0o400)
}

pub(super) fn is_private_read_write(file: &File) -> Result<bool> {
    let metadata = require_regular(file)?;
    Ok(metadata.permissions().mode() & 0o777 == 0o600
        && metadata.uid() == rustix::process::getuid().as_raw())
}

pub(super) fn make_private_inheritable(directory: &File) -> Result<()> {
    if !directory.metadata()?.is_dir() {
        return Err(AppError::InvalidInput(
            "A private profile must be a directory".into(),
        ));
    }
    // SQLite creates WAL/SHM with the private main database's permission bits.
    // Owner-only directory access protects all those names during creation.
    make_private(directory, false)
}

pub(super) fn is_private_inheritable(directory: &File) -> Result<bool> {
    let metadata = directory.metadata()?;
    Ok(metadata.is_dir()
        && metadata.permissions().mode() & 0o777 == 0o700
        && metadata.uid() == rustix::process::getuid().as_raw())
}

pub(super) fn is_private_sqlite_file(file: &File) -> Result<bool> {
    is_private_read_write(file)
}

pub(super) fn sync_directory(parent: &File) -> Result<()> {
    rustix::fs::fsync(parent).map_err(std::io::Error::from)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write, os::unix::fs::symlink};

    use super::*;
    use crate::secure_fs::SecureDir;

    fn fixture() -> Result<(tempfile::TempDir, SecureDir)> {
        let temporary = tempfile::tempdir()?;
        // macOS system /tmp is a symlink. Establish the system temporary root
        // before the strict walker; links inside the fixture remain forbidden.
        let root = temporary.path().canonicalize()?;
        let directory = SecureDir::open(&root, false, AccessPolicy::Private)?;
        Ok((temporary, directory))
    }

    #[test]
    fn sqlite_profile_policy_is_private_without_repair_on_validation() -> Result<()> {
        let (temporary, directory) = fixture()?;
        make_private_inheritable(directory.as_file())?;
        assert!(is_private_inheritable(directory.as_file())?);
        let file = directory.create_new(OsStr::new("database"), AccessPolicy::Private)?;
        assert!(is_private_sqlite_file(&file)?);
        drop(file);
        let guard = directory.open_pinned_regular(OsStr::new("database"))?;
        assert!(is_private_sqlite_file(&guard)?);
        fs::set_permissions(temporary.path(), Permissions::from_mode(0o755))?;
        assert!(!is_private_inheritable(directory.as_file())?);
        assert_eq!(
            fs::metadata(temporary.path())?.permissions().mode() & 0o777,
            0o755
        );
        Ok(())
    }

    #[test]
    fn publication_never_overwrites_and_rejects_a_replaced_stage() -> Result<()> {
        let (temporary, directory) = fixture()?;
        let mut stage = directory.create_new(OsStr::new("stage"), AccessPolicy::Private)?;
        stage.write_all(b"proposed")?;
        fs::write(temporary.path().join("target"), b"existing")?;
        assert_eq!(
            directory.publish_noreplace(&stage, OsStr::new("stage"), OsStr::new("target"))?,
            PublishResult::AlreadyExists
        );
        assert_eq!(fs::read(temporary.path().join("target"))?, b"existing");
        fs::rename(temporary.path().join("stage"), temporary.path().join("old"))?;
        fs::write(temporary.path().join("stage"), b"replacement")?;
        assert!(matches!(
            directory.publish_noreplace(&stage, OsStr::new("stage"), OsStr::new("new")),
            Err(AppError::Conflict(_))
        ));
        assert!(!temporary.path().join("new").exists());
        Ok(())
    }

    #[test]
    fn links_and_non_regular_sources_are_rejected() -> Result<()> {
        let (temporary, directory) = fixture()?;
        fs::create_dir(temporary.path().join("folder"))?;
        fs::write(temporary.path().join("book"), b"book")?;
        symlink("book", temporary.path().join("link"))?;
        symlink("folder", temporary.path().join("linked-folder"))?;
        assert!(directory.open_regular(OsStr::new("link")).is_err());
        assert!(directory.open_regular(OsStr::new("folder")).is_err());
        assert!(
            directory
                .child(OsStr::new("linked-folder"), false, AccessPolicy::Private)
                .is_err()
        );
        assert!(
            SecureDir::open(
                &temporary.path().join("linked-folder"),
                false,
                AccessPolicy::Private
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn snapshots_detect_in_place_changes_and_cleanup_preserves_replacements() -> Result<()> {
        let (temporary, directory) = fixture()?;
        let mut file = directory.create_new(OsStr::new("stage"), AccessPolicy::Private)?;
        file.write_all(b"first")?;
        let before = snapshot(&file)?;
        file.write_all(b"second")?;
        assert!(matches!(before.verify(&file), Err(AppError::Conflict(_))));
        let expected = identity(&file)?;
        fs::rename(temporary.path().join("stage"), temporary.path().join("old"))?;
        fs::write(temporary.path().join("stage"), b"replacement")?;
        assert!(!directory.remove_if_identity(OsStr::new("stage"), expected)?);
        assert_eq!(fs::read(temporary.path().join("stage"))?, b"replacement");
        assert!(directory.remove_if_identity(OsStr::new("old"), expected)?);
        assert!(!directory.remove_if_identity(OsStr::new("old"), expected)?);
        Ok(())
    }

    #[test]
    fn private_policy_and_new_directory_preserve_existing_parent_permissions() -> Result<()> {
        let (temporary, directory) = fixture()?;
        let original_mode = directory.as_file().metadata()?.permissions().mode();
        let child = directory.child(OsStr::new("private"), true, AccessPolicy::Private)?;
        assert_eq!(
            child.as_file().metadata()?.permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            directory.as_file().metadata()?.permissions().mode(),
            original_mode
        );
        let file = child.create_new(OsStr::new("original"), AccessPolicy::Private)?;
        assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
        assert!(is_private_read_write(&file)?);
        make_private(&file, true)?;
        assert!(is_private_read_only(&file)?);
        assert!(!is_private_read_write(&file)?);
        assert!(
            child
                .create_new(OsStr::new("original"), AccessPolicy::Private)
                .is_err()
        );
        child.sync()?;
        assert!(directory.free_space()? > 0);
        assert!(temporary.path().join("private/original").is_file());
        Ok(())
    }

    #[test]
    fn mutable_private_check_refuses_broad_permissions_without_repairing_them() -> Result<()> {
        let (_temporary, directory) = fixture()?;
        let file = directory.create_new(OsStr::new("runtime.lock"), AccessPolicy::Private)?;
        file.set_permissions(Permissions::from_mode(0o644))?;
        assert!(!is_private_read_write(&file)?);
        assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o644);
        Ok(())
    }
}
