use thiserror::Error;

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("File operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Database operation failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("JSON processing failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Archive processing failed: {0}")]
    Archive(#[from] zip::result::ZipError),
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    #[error("Unsupported operation or format: {0}")]
    Unsupported(String),
    #[error("Operation conflicts with existing data: {0}")]
    Conflict(String),
    #[error("The book was modified by another operation; reload it before applying changes")]
    RevisionConflict,
    #[error("This library profile is already open in another Library Manager process")]
    ProfileInUse,
    #[error("Resource not found: {0}")]
    NotFound(String),
    #[error("Network operation failed: {0}")]
    Network(String),
    #[error("AI provider operation failed: {0}")]
    Provider(String),
    #[error("Operation cancelled")]
    Cancelled,
}

impl AppError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "io_error",
            Self::Database(_) => "database_error",
            Self::Json(_) => "json_error",
            Self::Archive(_) => "archive_error",
            Self::InvalidInput(_) => "invalid_input",
            Self::Unsupported(_) => "unsupported",
            Self::Conflict(_) => "conflict",
            Self::RevisionConflict => "revision_conflict",
            Self::ProfileInUse => "profile_in_use",
            Self::NotFound(_) => "not_found",
            Self::Network(_) => "network_error",
            Self::Provider(_) => "provider_error",
            Self::Cancelled => "cancelled",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_lock_error_is_distinct_from_a_book_revision_conflict() {
        assert_eq!(AppError::ProfileInUse.code(), "profile_in_use");
        assert_ne!(
            AppError::ProfileInUse.code(),
            AppError::Conflict("revision".into()).code()
        );
        assert_eq!(
            AppError::ProfileInUse.to_string(),
            "This library profile is already open in another Library Manager process"
        );
    }

    #[test]
    fn book_revision_error_is_distinct_from_profile_and_file_conflicts() {
        assert_eq!(AppError::RevisionConflict.code(), "revision_conflict");
        assert_ne!(
            AppError::RevisionConflict.code(),
            AppError::ProfileInUse.code()
        );
        assert_ne!(
            AppError::RevisionConflict.code(),
            AppError::Conflict("file collision".into()).code()
        );
    }
}
