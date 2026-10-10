use std::ffi::OsStr;
use std::fs::{self, File};
use std::ops::{Deref, DerefMut};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use crate::error::{AppError, Result};
use crate::secure_fs::{self, AccessPolicy, FileIdentity, SecureDir};

const SCHEMA_VERSION: i64 = 1;
const DATABASE_NAME: &str = "library.sqlite3";
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const INITIAL_MIGRATION: &str = include_str!("../migrations/001_initial.sql");
const SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];

#[derive(Debug)]
struct ProfileAnchor {
    directory: SecureDir,
    directory_identity: FileIdentity,
    database_identity: FileIdentity,
}

#[derive(Clone, Debug)]
pub struct Database {
    path: PathBuf,
    anchor: Arc<ProfileAnchor>,
}

/// SQLite closes first; filesystem handles remain alive through its final WAL
/// checkpoint and sidecar cleanup, including when the Database was temporary.
pub struct ProfileConnection {
    connection: Connection,
    _database_file: File,
    _anchor: Arc<ProfileAnchor>,
}

impl Deref for ProfileConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl DerefMut for ProfileConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

impl Database {
    pub fn new(data_dir: &Path) -> Result<Self> {
        let (path, directory) = prepare_directory(data_dir)?;
        validate_sidecars(&directory)?;
        let name = OsStr::new(DATABASE_NAME);
        reject_non_regular_path(&path)?;
        match directory.open_regular(name) {
            Ok(file) => prepare_existing_database(&file)?,
            Err(AppError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                match directory.create_new(name, AccessPolicy::Private) {
                    Ok(file) => {
                        file.sync_all()?;
                        drop(file);
                        directory.sync()?;
                    }
                    Err(AppError::Io(error))
                        if error.kind() == std::io::ErrorKind::AlreadyExists =>
                    {
                        prepare_existing_database(&directory.open_regular(name)?)?;
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
        let database_file = directory.open_pinned_regular(name)?;
        require_private_file(&database_file)?;
        let anchor = Arc::new(ProfileAnchor {
            directory_identity: directory.identity()?,
            database_identity: secure_fs::identity(&database_file)?,
            directory,
        });
        let database = Self { path, anchor };
        let mut connection = database.open_with_guard(database_file)?;
        reject_future_schema(&connection)?;
        configure_connection(&connection)?;
        validate_sidecars(&database.anchor.directory)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version = reject_future_schema(&transaction)?;
        if version < SCHEMA_VERSION {
            transaction.execute_batch(INITIAL_MIGRATION)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        transaction.commit()?;
        validate_sidecars(&database.anchor.directory)?;
        Ok(database)
    }

    pub fn connect(&self) -> Result<ProfileConnection> {
        self.verify_directory()?;
        reject_non_regular_path(&self.path)?;
        validate_sidecars(&self.anchor.directory)?;
        let file = self
            .anchor
            .directory
            .open_pinned_regular(OsStr::new(DATABASE_NAME))?;
        require_private_file(&file)?;
        if secure_fs::identity(&file)? != self.anchor.database_identity {
            return Err(AppError::Conflict(
                "The library database was replaced".into(),
            ));
        }
        let connection = self.open_with_guard(file)?;
        let version = reject_future_schema(&connection)?;
        if version != SCHEMA_VERSION {
            return Err(AppError::InvalidInput(
                "The library database is not initialized".into(),
            ));
        }
        configure_connection(&connection)?;
        validate_sidecars(&self.anchor.directory)?;
        Ok(connection)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn verify_directory(&self) -> Result<()> {
        let path = self.path.parent().ok_or_else(|| {
            AppError::InvalidInput("The library database needs a profile directory".into())
        })?;
        let current = SecureDir::open(path, false, AccessPolicy::Private)?;
        if current.identity()? != self.anchor.directory_identity {
            return Err(AppError::Conflict(
                "The library profile was replaced".into(),
            ));
        }
        if !secure_fs::is_private_inheritable(current.as_file())? {
            return Err(AppError::InvalidInput(
                "The library profile permissions are not private".into(),
            ));
        }
        Ok(())
    }

    fn open_with_guard(&self, file: File) -> Result<ProfileConnection> {
        self.verify_directory()?;
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        // Creation was exclusive and private before SQLite sees the pathname.
        let connection = Connection::open_with_flags(&self.path, flags)?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        self.verify_directory()?;
        let current = self
            .anchor
            .directory
            .open_regular(OsStr::new(DATABASE_NAME))?;
        if secure_fs::identity(&current)? != self.anchor.database_identity {
            return Err(AppError::Conflict(
                "The library database was replaced".into(),
            ));
        }
        Ok(ProfileConnection {
            connection,
            _database_file: file,
            _anchor: self.anchor.clone(),
        })
    }
}

fn reject_future_schema(connection: &Connection) -> Result<i64> {
    let version =
        connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
    if version > SCHEMA_VERSION {
        return Err(AppError::Unsupported(format!(
            "Library schema {version} requires a newer Library Manager (supported: {SCHEMA_VERSION})"
        )));
    }
    if version < 0 {
        return Err(AppError::InvalidInput(
            "Invalid library schema version".into(),
        ));
    }
    Ok(version)
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    let mode =
        connection.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(AppError::Unsupported(
            "The library filesystem does not support SQLite WAL mode".into(),
        ));
    }
    Ok(())
}

fn prepare_directory(path: &Path) -> Result<(PathBuf, SecureDir)> {
    if path.as_os_str().is_empty() {
        return Err(AppError::InvalidInput(
            "The profile directory is empty".into(),
        ));
    }
    let path = secure_fs::absolute_path(path)?;
    if !path
        .components()
        .any(|part| matches!(part, Component::Normal(_)))
    {
        return Err(AppError::InvalidInput(
            "The filesystem root cannot be a library profile".into(),
        ));
    }
    if let Ok(metadata) = fs::symlink_metadata(&path)
        && (metadata.file_type().is_symlink() || !metadata.is_dir())
    {
        return Err(AppError::InvalidInput(
            "The library profile must be an ordinary directory".into(),
        ));
    }
    let directory = SecureDir::open(&path, true, AccessPolicy::Private)?;
    // The dedicated profile may contain SQLite-created WAL/SHM files. Their
    // permissions must be private at creation, rather than repaired after I/O.
    secure_fs::make_private_inheritable(directory.as_file())?;
    let canonical = fs::canonicalize(path)?;
    if SecureDir::open(&canonical, false, AccessPolicy::Private)?.identity()?
        != directory.identity()?
    {
        return Err(AppError::Conflict(
            "The library profile was replaced".into(),
        ));
    }
    Ok((canonical.join(DATABASE_NAME), directory))
}

fn prepare_existing_database(file: &File) -> Result<()> {
    // Preserve the established Unix profile migration to owner-only permissions.
    // Windows existing files are checked, never silently made writable.
    #[cfg(unix)]
    secure_fs::make_private(file, false)?;
    require_private_file(file)
}

fn require_private_file(file: &File) -> Result<()> {
    if !secure_fs::is_private_sqlite_file(file)? {
        return Err(AppError::InvalidInput(
            "The library database files must be private mutable regular files".into(),
        ));
    }
    Ok(())
}

fn reject_non_regular_path(path: &Path) -> Result<()> {
    // Preserve the public invalid-input category; handle-relative opens below
    // still enforce nofollow even if the entry changes after this observation.
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            AppError::InvalidInput("The library database files must be regular files".into()),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn validate_sidecars(directory: &SecureDir) -> Result<()> {
    for suffix in SIDECAR_SUFFIXES {
        let name = format!("{DATABASE_NAME}{suffix}");
        match directory.open_regular(OsStr::new(&name)) {
            Ok(file) => require_private_file(&file)?,
            Err(AppError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        // Do not retain sidecar handles: SQLite owns their deletion lifecycle.
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary() -> Result<tempfile::TempDir> {
        Ok(tempfile::Builder::new().tempdir_in(fs::canonicalize(std::env::temp_dir())?)?)
    }

    #[test]
    fn connection_retains_anchors_after_temporary_database_is_dropped() -> Result<()> {
        let temporary = temporary()?;
        let connection = Database::new(temporary.path())?.connect()?;
        assert_eq!(Arc::strong_count(&connection._anchor), 1);
        connection.execute(
            "INSERT INTO books(id,title) VALUES('anchored','Retained')",
            [],
        )?;
        let second = Database::new(temporary.path())?.connect()?;
        let count: i64 = second.query_row("SELECT COUNT(*) FROM books", [], |row| row.get(0))?;
        assert_eq!(count, 1);
        drop(second);
        drop(connection);
        let reopened = Database::new(temporary.path())?;
        assert_eq!(
            reopened
                .connect()?
                .query_row("SELECT COUNT(*) FROM books", [], |row| row.get::<_, i64>(0))?,
            1
        );
        Ok(())
    }

    #[test]
    fn replacing_the_main_database_is_refused_by_existing_handle_authority() -> Result<()> {
        let temporary = temporary()?;
        let database = Database::new(temporary.path())?;
        fs::rename(database.path(), temporary.path().join("prior.sqlite3"))?;
        let replacement = database
            .anchor
            .directory
            .create_new(OsStr::new(DATABASE_NAME), AccessPolicy::Private)?;
        drop(replacement);
        assert!(matches!(database.connect(), Err(AppError::Conflict(_))));
        assert_eq!(fs::metadata(database.path())?.len(), 0);
        Ok(())
    }

    #[test]
    fn directory_identity_survives_or_refuses_a_path_replacement() -> Result<()> {
        let temporary = temporary()?;
        let profile = temporary.path().join("profile");
        let database = Database::new(&profile)?;
        let retained = temporary.path().join("retained-profile");
        #[cfg(unix)]
        {
            fs::rename(&profile, &retained)?;
            fs::create_dir(&profile)?;
            assert!(matches!(database.connect(), Err(AppError::Conflict(_))));
            assert!(!profile.join(DATABASE_NAME).exists());
        }
        #[cfg(windows)]
        {
            // The directory handle denies delete-sharing, so Windows must pin
            // it rather than allow a replacement while SQLite owns the path.
            assert!(fs::rename(&profile, &retained).is_err());
            assert!(database.connect().is_ok());
        }
        Ok(())
    }

    #[test]
    fn sidecar_handles_are_not_retained_across_sqlite_cleanup() -> Result<()> {
        let temporary = temporary()?;
        let database = Database::new(temporary.path())?;
        let first = database.connect()?;
        first.execute("INSERT INTO books(id,title) VALUES('wal','Checkpoint')", [])?;
        let second = database.connect()?;
        for suffix in ["-wal", "-shm"] {
            let file = database
                .anchor
                .directory
                .open_regular(OsStr::new(&format!("{DATABASE_NAME}{suffix}")))?;
            require_private_file(&file)?;
        }
        drop(second);
        drop(first);
        // Last close checkpoints and removes sidecars. Their disappearance is
        // evidence that validation did not keep a deny-delete handle alive.
        assert!(
            !temporary
                .path()
                .join(format!("{DATABASE_NAME}-wal"))
                .exists()
        );
        assert!(
            !temporary
                .path()
                .join(format!("{DATABASE_NAME}-shm"))
                .exists()
        );
        assert!(database.connect().is_ok());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn connecting_refuses_changed_profile_permissions_without_repairing_them() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let temporary = temporary()?;
        let database = Database::new(temporary.path())?;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o755))?;
        assert!(matches!(database.connect(), Err(AppError::InvalidInput(_))));
        assert_eq!(
            fs::metadata(temporary.path())?.permissions().mode() & 0o777,
            0o755
        );
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn junctions_cannot_redirect_profile_or_sidecar_access() -> Result<()> {
        use std::process::Command;
        let temporary = temporary()?;
        let target = temporary.path().join("target");
        fs::create_dir(&target)?;
        fs::write(target.join("sentinel"), b"preserved")?;
        let link = temporary.path().join("linked-profile");
        assert!(
            Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(&target)
                .status()?
                .success()
        );
        assert!(Database::new(&link).is_err());
        let profile = temporary.path().join("profile");
        let database = Database::new(&profile)?;
        let sidecar = profile.join(format!("{DATABASE_NAME}-journal"));
        assert!(
            Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&sidecar)
                .arg(&target)
                .status()?
                .success()
        );
        assert!(database.connect().is_err());
        assert_eq!(fs::read(target.join("sentinel"))?, b"preserved");
        fs::remove_dir(link)?;
        fs::remove_dir(sidecar)?;
        Ok(())
    }

    #[test]
    fn initialization_reopens_without_losing_records() -> Result<()> {
        let temporary =
            tempfile::Builder::new().tempdir_in(fs::canonicalize(std::env::temp_dir())?)?;
        let profile = temporary.path().join("private/profile");
        let database = Database::new(&profile)?;
        database.connect()?.execute(
            "INSERT INTO books(id, title) VALUES ('book', 'A preserved title')",
            [],
        )?;
        let reopened = Database::new(&profile)?;
        let title: String = reopened.connect()?.query_row(
            "SELECT title FROM books WHERE id = 'book'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(title, "A preserved title");
        assert!(reopened.path().is_absolute());
        assert_eq!(reopened.clone().path(), reopened.path());
        Ok(())
    }

    #[test]
    fn every_connection_enforces_constraints_and_durability() -> Result<()> {
        let temporary =
            tempfile::Builder::new().tempdir_in(fs::canonicalize(std::env::temp_dir())?)?;
        let database = Database::new(temporary.path())?;
        for _ in 0..2 {
            let connection = database.connect()?;
            let foreign_keys: i64 =
                connection.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
            let synchronous: i64 =
                connection.pragma_query_value(None, "synchronous", |row| row.get(0))?;
            let journal_mode: String =
                connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
            assert_eq!(foreign_keys, 1);
            assert_eq!(synchronous, 2);
            assert_eq!(journal_mode, "wal");
            assert!(connection.execute(
                "INSERT INTO books(id, title, authors_json) VALUES ('invalid', 'Title', 'invalid JSON')",
                [],
            ).is_err());
            assert!(connection.execute(
                "INSERT INTO messages(id, conversation_id, role, content) VALUES ('message', 'missing', 'user', 'Text')",
                [],
            ).is_err());
        }
        Ok(())
    }

    #[test]
    fn newer_schema_is_refused_and_preserved() -> Result<()> {
        let temporary =
            tempfile::Builder::new().tempdir_in(fs::canonicalize(std::env::temp_dir())?)?;
        let database = Database::new(temporary.path())?;
        let connection = database.connect()?;
        connection.pragma_update(None, "user_version", 2)?;
        assert!(matches!(database.connect(), Err(AppError::Unsupported(_))));
        assert!(matches!(
            Database::new(temporary.path()),
            Err(AppError::Unsupported(_))
        ));
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, 2);
        Ok(())
    }

    #[test]
    fn a_file_cannot_be_used_as_a_profile() -> Result<()> {
        let file = tempfile::NamedTempFile::new()?;
        assert!(matches!(
            Database::new(file.path()),
            Err(AppError::InvalidInput(_))
        ));
        Ok(())
    }

    #[test]
    fn connecting_does_not_recreate_a_missing_database() -> Result<()> {
        let temporary =
            tempfile::Builder::new().tempdir_in(fs::canonicalize(std::env::temp_dir())?)?;
        let database = Database::new(temporary.path())?;
        fs::remove_file(database.path())?;
        assert!(matches!(
            database.connect(),
            Err(AppError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound
        ));
        assert!(!database.path().exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn profile_and_database_permissions_are_private() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let temporary =
            tempfile::Builder::new().tempdir_in(fs::canonicalize(std::env::temp_dir())?)?;
        let profile = temporary.path().join("profile");
        let database = Database::new(&profile)?;
        assert_eq!(fs::metadata(&profile)?.permissions().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(database.path())?.permissions().mode() & 0o777,
            0o600
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_cannot_redirect_database_access() -> Result<()> {
        use std::os::unix::fs::symlink;
        let temporary =
            tempfile::Builder::new().tempdir_in(fs::canonicalize(std::env::temp_dir())?)?;
        let profile = temporary.path().join("profile");
        fs::create_dir(&profile)?;
        let linked_profile = temporary.path().join("linked-profile");
        symlink(&profile, &linked_profile)?;
        assert!(matches!(
            Database::new(&linked_profile),
            Err(AppError::InvalidInput(_))
        ));
        let target = temporary.path().join("preserved-file");
        fs::write(&target, "preserved")?;
        let database_path = profile.join(DATABASE_NAME);
        symlink(&target, &database_path)?;
        assert!(matches!(
            Database::new(&profile),
            Err(AppError::InvalidInput(_))
        ));
        fs::remove_file(&database_path)?;
        let database = Database::new(&profile)?;
        let mut sidecar = database.path().as_os_str().to_os_string();
        sidecar.push("-journal");
        symlink(&target, Path::new(&sidecar))?;
        assert!(database.connect().is_err());
        assert_eq!(fs::read_to_string(target)?, "preserved");
        Ok(())
    }
}
