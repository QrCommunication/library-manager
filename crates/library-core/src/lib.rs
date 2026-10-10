//! Standalone ebook-library engine, independent of the desktop WebView.
//!
//! Persistent data and serializable contracts are shared by the desktop shell,
//! background jobs, and engine tests. Filesystem and network operations remain
//! inside focused services; callers receive structured errors.

// Native Windows filesystem guarantees require audited FFI in its adapter;
// all other engine modules continue to reject unsafe code by default.
#![deny(unsafe_code)]

pub mod book_repository;
pub mod calibre_wireless;
pub mod chat;
pub mod chat_tools;
pub mod conversion;
pub mod database;
pub mod devices;
pub mod enrichment;
pub mod epub;
pub mod error;
pub mod jobs;
pub mod library;
pub mod manager;
pub mod models;
pub mod optimizer;
pub mod providers;
pub mod reader;
pub(crate) mod secure_fs;
pub mod settings;
pub mod storage;
pub mod transfer;
pub mod web;

pub use book_repository::{BookRepository, StoredFile};
pub use calibre_wireless::CalibreService;
pub use chat::{ChatService, PreparedChat};
pub use conversion::{ConversionReport, Converter};
pub use database::Database;
pub use devices::{DeviceService, IndexedDeviceBook};
pub use enrichment::{EnrichmentOutcome, EnrichmentService};
pub use epub::{
    EpubDocument, EpubInspection, ManifestItem, TocEntry, inspect_for_import, text_for_inspection,
};
pub use error::{AppError, Result};
pub use jobs::{ClaimedJob, JobService};
pub use library::{ImportOutcome, LibraryService};
pub use manager::{LibraryManager, ManagerEventSink};
pub use models::*;
pub use providers::ProviderService;
pub use reader::ReaderService;
pub use settings::SettingsService;
pub use storage::{Storage, StoredArtifact, StoredOriginal};
pub use transfer::{TransferItem, TransferItemStatus, TransferReport, TransferService};
pub use web::WebClient;
