//! JSON contracts shared by the standalone engine and the desktop application.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::error::AppError;

macro_rules! string_enum {
    ($name:ident { $($(#[$attribute:meta])* $variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$attribute])* #[serde(rename = $value)] $variant),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant)),+,
                    _ => Err(format!("Unknown {}: {value}", stringify!($name))),
                }
            }
        }
    };
}

string_enum!(BookFormat {
    #[default] Epub => "epub", Mobi => "mobi", Azw3 => "azw3", Fb2 => "fb2",
    Txt => "txt", Html => "html", Pdf => "pdf", Cbz => "cbz",
});
string_enum!(ReadStatus {
    #[default] Unread => "unread", Reading => "reading", Finished => "finished",
});
string_enum!(MetadataStatus {
    #[default] Pending => "pending", Verified => "verified",
    NeedsReview => "needsReview", Failed => "failed",
});
string_enum!(BookSort {
    Title => "title", Author => "author", Series => "series",
    #[default] Added => "added", Updated => "updated", Size => "size",
    Progress => "progress", Published => "published", Rating => "rating",
});
string_enum!(FileVariant {
    #[default] Original => "original", Normalized => "normalized",
    Optimized => "optimized", Converted => "converted",
});
string_enum!(DeviceTransport {
    #[default] Usb => "usb", Crosspoint => "crosspoint", CalibreWireless => "calibreWireless",
});
string_enum!(JobKind {
    #[default] Import => "import", Enrich => "enrich", Optimize => "optimize",
    Convert => "convert", DeviceIndex => "deviceIndex", Transfer => "transfer", Chat => "chat",
});
string_enum!(JobStatus {
    #[default] Queued => "queued", Running => "running",
    WaitingForConfiguration => "waitingForConfiguration", WaitingForNetwork => "waitingForNetwork",
    Completed => "completed", Failed => "failed", Cancelled => "cancelled",
});
string_enum!(OptimizationPreset {
    Lossless => "lossless", #[default] Balanced => "balanced", Xteink => "xteink", TextOnly => "textOnly",
});
string_enum!(OperationStatus {
    #[default] Applied => "applied", Reverted => "reverted", Failed => "failed",
});
string_enum!(ProviderId {
    Zai => "zai", Kimi => "kimi", Minimax => "minimax",
    #[default] Codex => "codex", Claude => "claude", Mistral => "mistral",
});
string_enum!(ConnectionMode {
    #[default] Api => "api", LocalCli => "localCli",
});
string_enum!(ProviderStatus {
    Ready => "ready", #[default] NeedsKey => "needsKey", Unavailable => "unavailable",
});
string_enum!(CatalogSource {
    #[default] Api => "api", OfficialCatalog => "officialCatalog", LocalCli => "localCli", Cache => "cache",
});
string_enum!(ChatRole {
    #[default] User => "user", Assistant => "assistant", System => "system",
});
string_enum!(Theme {
    #[default] System => "system", Light => "light", Dark => "dark",
});
string_enum!(ErrorCode {
    InvalidInput => "invalidInput", NotFound => "notFound", UnsupportedFormat => "unsupportedFormat",
    EncryptedBook => "encryptedBook", InvalidEpub => "invalidEpub", UnsafePath => "unsafePath",
    RevisionConflict => "revisionConflict", DeviceDisconnected => "deviceDisconnected",
    InsufficientSpace => "insufficientSpace", NetworkUnavailable => "networkUnavailable",
    ProviderNotConfigured => "providerNotConfigured", ProviderError => "providerError",
    RateLimited => "rateLimited", SecretStoreUnavailable => "secretStoreUnavailable",
    ConversionFailed => "conversionFailed", OperationConflict => "operationConflict",
    Cancelled => "cancelled", ProfileInUse => "profileInUse", #[default] Internal => "internal",
});

/// Bibliographic values only; file inspection details belong to the format module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct BookMetadata {
    pub title: String,
    pub authors: Vec<String>,
    pub author_sort: String,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    pub language: String,
    pub description: String,
    pub isbn: Option<String>,
    pub publisher: Option<String>,
    pub published: Option<String>,
}

impl Default for BookMetadata {
    fn default() -> Self {
        Self {
            title: String::new(),
            authors: Vec::new(),
            author_sort: String::new(),
            series: None,
            series_index: None,
            genres: Vec::new(),
            tags: Vec::new(),
            language: "und".into(),
            description: String::new(),
            isbn: None,
            publisher: None,
            published: None,
        }
    }
}

pub type EpubMetadata = BookMetadata;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Book {
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub author_sort: String,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    pub language: String,
    pub description: String,
    pub isbn: Option<String>,
    pub publisher: Option<String>,
    pub published: Option<String>,
    pub cover_path: Option<String>,
    pub format: BookFormat,
    pub size_bytes: u64,
    pub added_at: String,
    pub updated_at: String,
    pub read_status: ReadStatus,
    pub reading_progress: f64,
    pub favorite: bool,
    pub rating: Option<f64>,
    pub notes: String,
    pub metadata_status: MetadataStatus,
    pub metadata_confidence: Option<f64>,
    pub revision: u64,
    pub on_device_ids: Vec<String>,
}

/// `None`: unchanged; `Some(None)`: clear; `Some(Some(value))`: replace.
pub type NullablePatch<T> = Option<Option<T>>;

fn deserialize_nullable_patch<'de, T, D>(deserializer: D) -> Result<NullablePatch<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct BookPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_sort: Option<String>,
    #[serde(
        deserialize_with = "deserialize_nullable_patch",
        skip_serializing_if = "Option::is_none"
    )]
    pub series: NullablePatch<String>,
    #[serde(
        deserialize_with = "deserialize_nullable_patch",
        skip_serializing_if = "Option::is_none"
    )]
    pub series_index: NullablePatch<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genres: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(
        deserialize_with = "deserialize_nullable_patch",
        skip_serializing_if = "Option::is_none"
    )]
    pub isbn: NullablePatch<String>,
    #[serde(
        deserialize_with = "deserialize_nullable_patch",
        skip_serializing_if = "Option::is_none"
    )]
    pub publisher: NullablePatch<String>,
    #[serde(
        deserialize_with = "deserialize_nullable_patch",
        skip_serializing_if = "Option::is_none"
    )]
    pub published: NullablePatch<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_status: Option<ReadStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favorite: Option<bool>,
    #[serde(
        deserialize_with = "deserialize_nullable_patch",
        skip_serializing_if = "Option::is_none"
    )]
    pub rating: NullablePatch<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct BookQuery {
    pub search: String,
    pub authors: Vec<String>,
    pub series: Vec<String>,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    pub languages: Vec<String>,
    pub formats: Vec<BookFormat>,
    pub device_id: Option<String>,
    pub on_device: Option<bool>,
    pub read_status: Option<ReadStatus>,
    pub favorite: Option<bool>,
    pub metadata_status: Option<MetadataStatus>,
    pub missing_cover: Option<bool>,
    pub min_size_bytes: Option<u64>,
    pub max_size_bytes: Option<u64>,
    pub sort: BookSort,
    pub descending: bool,
    pub offset: u64,
    pub limit: u64,
}

impl Default for BookQuery {
    fn default() -> Self {
        Self {
            search: String::new(),
            authors: Vec::new(),
            series: Vec::new(),
            genres: Vec::new(),
            tags: Vec::new(),
            languages: Vec::new(),
            formats: Vec::new(),
            device_id: None,
            on_device: None,
            read_status: None,
            favorite: None,
            metadata_status: None,
            missing_cover: None,
            min_size_bytes: None,
            max_size_bytes: None,
            sort: BookSort::Added,
            descending: true,
            offset: 0,
            limit: 100,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookPage {
    pub items: Vec<Book>,
    pub total: u64,
    pub offset: u64,
    pub limit: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facet {
    pub value: String,
    pub count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryFacets {
    pub authors: Vec<Facet>,
    pub series: Vec<Facet>,
    pub genres: Vec<Facet>,
    pub tags: Vec<Facet>,
    pub languages: Vec<Facet>,
    pub formats: Vec<Facet>,
    pub devices: Vec<Facet>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookFile {
    pub id: String,
    pub book_id: String,
    pub format: BookFormat,
    pub variant: FileVariant,
    pub profile: Option<String>,
    pub size_bytes: u64,
    pub sha256: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub label: String,
    pub transport: DeviceTransport,
    pub connected: bool,
    pub writable: bool,
    pub profile: String,
    pub mount_path: Option<String>,
    pub address: Option<String>,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub book_count: u64,
    pub matched_book_count: u64,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub kind: JobKind,
    pub status: JobStatus,
    pub progress: f64,
    pub message: String,
    pub book_ids: Vec<String>,
    pub result: Option<Value>,
    pub error: Option<PublicError>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationProfile {
    pub id: String,
    pub name: String,
    pub max_image_width: Option<u32>,
    pub max_image_height: Option<u32>,
    pub jpeg_quality: u8,
    pub grayscale: bool,
    pub remove_images: bool,
    pub remove_embedded_fonts: bool,
    pub simplify_css: bool,
    pub compression_level: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationReport {
    pub book_id: String,
    pub file_id: String,
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub images_changed: u64,
    pub images_removed: u64,
    pub fonts_removed: u64,
    pub chapters_before: u64,
    pub chapters_after: u64,
    pub text_preserved: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub id: String,
    pub kind: String,
    pub status: OperationStatus,
    pub description: String,
    pub reversible: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: ProviderId,
    pub name: String,
    pub configured: bool,
    pub connection_mode: ConnectionMode,
    pub supports_tools: bool,
    pub status: ProviderStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub name: String,
    pub description: String,
    pub context_window: Option<u64>,
    pub supports_tools: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalog {
    pub provider_id: ProviderId,
    pub models: Vec<Model>,
    pub source: CatalogSource,
    pub fetched_at: String,
    pub stale: bool,
    pub error: Option<PublicError>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSource {
    pub url: String,
    pub title: String,
    pub excerpt: String,
    pub retrieved_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetadataEvidence {
    pub field: String,
    pub value: String,
    pub confidence: f64,
    pub source_urls: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetadataProposal {
    pub book_id: String,
    pub patch: BookPatch,
    pub confidence: f64,
    pub evidence: Vec<MetadataEvidence>,
    pub warnings: Vec<String>,
    pub provider_id: ProviderId,
    pub model_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub conversation_id: String,
    pub role: ChatRole,
    pub content: String,
    pub sources: Vec<WebSource>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub language: String,
    pub theme: Theme,
    pub library_root: String,
    pub provider_id: Option<ProviderId>,
    pub model_id: Option<String>,
    pub auto_enrich: bool,
    pub web_enabled: bool,
    pub auto_apply_confidence: f64,
    pub default_device_profile: String,
    pub default_optimization_profile: String,
    pub max_concurrent_jobs: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: "system".into(),
            theme: Theme::System,
            library_root: String::new(),
            provider_id: None,
            model_id: None,
            auto_enrich: true,
            web_enabled: true,
            auto_apply_confidence: 0.9,
            default_device_profile: "generic".into(),
            default_optimization_profile: "balanced".into(),
            max_concurrent_jobs: 2,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub detail: Option<String>,
}

impl From<&AppError> for PublicError {
    fn from(error: &AppError) -> Self {
        let (code, message, retryable) = match error {
            AppError::Io(_) | AppError::Database(_) | AppError::Json(_) => {
                (ErrorCode::Internal, "An internal operation failed", false)
            }
            AppError::Archive(_) => (ErrorCode::InvalidEpub, "The book archive is invalid", false),
            AppError::InvalidInput(_) => (
                ErrorCode::InvalidInput,
                "The supplied input is invalid",
                false,
            ),
            AppError::Unsupported(_) => (
                ErrorCode::UnsupportedFormat,
                "The format or operation is unsupported",
                false,
            ),
            AppError::Conflict(_) => (
                ErrorCode::OperationConflict,
                "The operation conflicts with current data",
                false,
            ),
            AppError::RevisionConflict => (
                ErrorCode::RevisionConflict,
                "The book was modified by another operation; reload it before applying changes",
                false,
            ),
            AppError::ProfileInUse => (
                ErrorCode::ProfileInUse,
                "This library profile is already open in another Library Manager process",
                false,
            ),
            AppError::NotFound(_) => (
                ErrorCode::NotFound,
                "The requested resource was not found",
                false,
            ),
            AppError::Network(_) => (
                ErrorCode::NetworkUnavailable,
                "The network request failed",
                true,
            ),
            AppError::Provider(_) => (
                ErrorCode::ProviderError,
                "The AI provider request failed",
                false,
            ),
            AppError::Cancelled => (ErrorCode::Cancelled, "The operation was cancelled", false),
        };
        Self {
            code,
            message: message.into(),
            retryable,
            detail: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReaderTocItem {
    pub label: String,
    pub section_index: u64,
    pub fragment: Option<String>,
    pub children: Vec<Self>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReaderSectionInfo {
    pub index: u64,
    pub title: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReaderManifest {
    pub book_id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub sections: Vec<ReaderSectionInfo>,
    pub toc: Vec<ReaderTocItem>,
    pub saved_location: Option<String>,
    pub saved_progress: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReaderResource {
    pub id: String,
    pub url: String,
    pub media_type: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReaderSection {
    pub book_id: String,
    pub section_index: u64,
    pub html: String,
    pub resources: Vec<ReaderResource>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub version: String,
    pub system_language: String,
    pub settings: Settings,
    pub devices: Vec<Device>,
    pub pending_jobs: Vec<Job>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionCapabilities {
    pub inputs: Vec<BookFormat>,
    pub outputs: Vec<BookFormat>,
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nullable_patch_distinguishes_absence_clear_and_replacement() {
        let unchanged: BookPatch = serde_json::from_value(json!({"title": "Corrected"})).unwrap();
        assert_eq!(unchanged.series, None);
        let clear: BookPatch =
            serde_json::from_value(json!({"series": null, "seriesIndex": null})).unwrap();
        assert_eq!(clear.series, Some(None));
        assert_eq!(clear.series_index, Some(None));
        let replacement: BookPatch =
            serde_json::from_value(json!({"series": "Saga", "seriesIndex": 0})).unwrap();
        assert_eq!(replacement.series, Some(Some("Saga".into())));
        assert_eq!(replacement.series_index, Some(Some(0.0)));
        assert_eq!(
            serde_json::to_value(clear).unwrap(),
            json!({"series": null, "seriesIndex": null})
        );
        assert_eq!(
            serde_json::to_value(BookPatch::default()).unwrap(),
            json!({})
        );
    }

    #[test]
    fn partial_settings_preserve_safe_defaults() {
        let settings: Settings = serde_json::from_value(json!({"language": "fr"})).unwrap();
        assert_eq!(settings.language, "fr");
        assert!(settings.auto_enrich && settings.web_enabled);
        assert_eq!(settings.theme, Theme::System);
        assert_eq!(settings.max_concurrent_jobs, 2);
        assert_eq!(settings.provider_id, None);
    }

    #[test]
    fn public_errors_do_not_expose_provider_secrets() {
        let error = AppError::Provider("request Authorization: Bearer private-token".into());
        let value = serde_json::to_value(PublicError::from(&error)).unwrap();
        assert_eq!(value["code"], "providerError");
        assert!(!value.to_string().contains("private-token"));
        assert_eq!(value["detail"], Value::Null);
    }

    #[test]
    fn profile_in_use_has_a_dedicated_public_code_without_a_book_revision_warning() {
        let error = PublicError::from(&AppError::ProfileInUse);
        let value = serde_json::to_value(error).unwrap();
        assert_eq!(value["code"], "profileInUse");
        assert_eq!(value["retryable"], false);
        assert_eq!(value["detail"], Value::Null);
        assert!(value["message"].as_str().unwrap().contains("already open"));
        assert_ne!(value["code"], "revisionConflict");
        assert_ne!(value["code"], "operationConflict");
    }

    #[test]
    fn book_revision_and_operation_conflicts_keep_distinct_public_codes() {
        let revision = PublicError::from(&AppError::RevisionConflict);
        assert_eq!(revision.code, ErrorCode::RevisionConflict);
        assert_eq!(
            serde_json::to_value(&revision).unwrap()["code"],
            "revisionConflict"
        );
        assert!(!revision.retryable);
        assert_eq!(revision.detail, None);
        let collision = PublicError::from(&AppError::Conflict("private/path collision".into()));
        assert_eq!(collision.code, ErrorCode::OperationConflict);
        assert!(
            !serde_json::to_string(&collision)
                .unwrap()
                .contains("private/path")
        );
    }

    #[test]
    fn contract_rejects_unknown_patch_fields_and_uses_camel_case() {
        assert!(serde_json::from_value::<BookPatch>(json!({"shell": "anything"})).is_err());
        let value = serde_json::to_value(Book::default()).unwrap();
        assert!(value.get("readingProgress").is_some());
        assert!(value.get("onDeviceIds").is_some());
        assert!(value.get("reading_progress").is_none());
        assert_eq!(ProviderId::from_str("minimax").unwrap().as_str(), "minimax");
        assert_eq!(
            JobStatus::WaitingForConfiguration.as_str(),
            "waitingForConfiguration"
        );
        assert!(BookFormat::from_str("unsupported").is_err());
    }
}
