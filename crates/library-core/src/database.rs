use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use crate::error::{AppError, Result};

const SCHEMA_VERSION: i64 = 1;
const DATABASE_NAME: &str = "library.sqlite3";
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const INITIAL_MIGRATION: &str = include_str!("../migrations/001_initial.sql");

#[derive(Clone, Debug)]
pub struct Database {
    path: PathBuf,
}

impl Database {
    pub fn new(data_dir: &Path) -> Result<Self> {
        prepare_directory(data_dir)?;
        let data_dir = fs::canonicalize(data_dir)?;
        let database = Self {
            path: data_dir.join(DATABASE_NAME),
        };
        let mut connection = database.open(true)?;
        reject_future_schema(&connection)?;
        configure_connection(&connection)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version = reject_future_schema(&transaction)?;
        if version < SCHEMA_VERSION {
            transaction.execute_batch(INITIAL_MIGRATION)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        transaction.commit()?;
        Ok(database)
    }

    pub fn connect(&self) -> Result<Connection> {
        let connection = self.open(false)?;
        let version = reject_future_schema(&connection)?;
        if version != SCHEMA_VERSION {
            return Err(AppError::InvalidInput(
                "The library database is not initialized".into(),
            ));
        }
        configure_connection(&connection)?;
        Ok(connection)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn open(&self, create: bool) -> Result<Connection> {
        let directory = self.path.parent().ok_or_else(|| {
            AppError::InvalidInput("The library database needs a profile directory".into())
        })?;
        require_directory(directory)?;
        validate_database_files(&self.path, create)?;
        let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        if create {
            flags |= OpenFlags::SQLITE_OPEN_CREATE;
        }
        let connection = Connection::open_with_flags(&self.path, flags)?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        make_file_private(&self.path)?;
        Ok(connection)
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

fn prepare_directory(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Err(AppError::InvalidInput(
            "The profile directory is empty".into(),
        ));
    }
    match fs::symlink_metadata(path) {
        Ok(_) => require_directory(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path)?;
            require_directory(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    if fs::canonicalize(path)?.parent().is_none() {
        return Err(AppError::InvalidInput(
            "The filesystem root cannot be a library profile".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn require_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppError::InvalidInput(
            "The library profile must be a directory, without a symbolic link".into(),
        ));
    }
    Ok(())
}

fn validate_database_files(path: &Path, allow_missing_database: bool) -> Result<()> {
    validate_regular_file(path, allow_missing_database)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        validate_regular_file(Path::new(&sidecar), true)?;
    }
    Ok(())
}

fn validate_regular_file(path: &Path, allow_missing: bool) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            AppError::InvalidInput("The library database files must be regular files".into()),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && allow_missing => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn make_file_private(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialization_reopens_without_losing_records() -> Result<()> {
        let temporary = tempfile::tempdir()?;
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
        let temporary = tempfile::tempdir()?;
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
        let temporary = tempfile::tempdir()?;
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
        let temporary = tempfile::tempdir()?;
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
        let temporary = tempfile::tempdir()?;
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
        let temporary = tempfile::tempdir()?;
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
        assert!(matches!(database.connect(), Err(AppError::InvalidInput(_))));
        assert_eq!(fs::read_to_string(target)?, "preserved");
        Ok(())
    }
}
