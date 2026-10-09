//! Desktop input/output boundary; all library work belongs to `library-core`.

use std::{
    fs,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use library_core::{
    AppError, Book, BookFile, BookFormat, BookPage, BookPatch, BookQuery, ChatMessage,
    Conversation, ConversionCapabilities, Device, DeviceTransport, ErrorCode, Job, LibraryFacets,
    LibraryManager, ManagerEventSink, ModelCatalog, Operation, OptimizationProfile, Provider,
    ProviderId, PublicError, ReaderManifest, ReaderSection, Settings, optimizer,
    providers::public_provider_error,
};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};

type IpcResult<T> = std::result::Result<T, PublicError>;

const MOBITOOL_NAME: &str = "library-manager-mobitool";
const EVENT_NAMES: &[&str] = &[
    "library:changed",
    "devices:changed",
    "job:updated",
    "chat:delta",
    "reader:progress",
];

/// Initialization failure is retained so the WebView can show and retry its UI.
/// The profile lock remains owned by the manager for the application's lifetime.
pub struct AppState {
    manager: IpcResult<Arc<LibraryManager>>,
    shutdown: Arc<AtomicBool>,
    background_started: AtomicBool,
}

impl AppState {
    pub fn new(app: AppHandle) -> Self {
        let initialize = || -> library_core::Result<LibraryManager> {
            let root = app.path().app_data_dir().map_err(|_| {
                AppError::InvalidInput("Cannot locate the application profile".into())
            })?;
            let root = application_root(&root)?;
            let executable = std::env::current_exe()?;
            #[cfg(debug_assertions)]
            let development_engine = option_env!("LIBRARY_MANAGER_MOBITOOL").map(Path::new);
            #[cfg(not(debug_assertions))]
            let development_engine = None;
            let engine = bundled_engine(&executable, development_engine)?;
            let sink: ManagerEventSink = Arc::new(move |name, payload| {
                if EVENT_NAMES.contains(&name) {
                    // Emission never turns a committed operation into a failure.
                    let _ = app.emit(name, payload);
                }
            });
            LibraryManager::new(&root, engine, sink)
        };
        Self::from_initialization(initialize().map_err(|error| public_error(&error)))
    }

    fn from_initialization(manager: IpcResult<LibraryManager>) -> Self {
        Self {
            manager: manager.map(Arc::new),
            shutdown: Arc::new(AtomicBool::new(false)),
            background_started: AtomicBool::new(false),
        }
    }

    fn manager(&self) -> IpcResult<Arc<LibraryManager>> {
        self.manager.clone()
    }

    pub fn start_background(&self, _app: AppHandle) {
        let Ok(manager) = self.manager() else {
            return;
        };
        if self.background_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let shutdown = Arc::clone(&self.shutdown);
        // setup() runs on GTK's thread, which does not have a Tokio context.
        tauri::async_runtime::spawn(async move {
            let worker = manager.start_background(shutdown);
            let _ = worker.await;
        });
    }
}

impl Drop for AppState {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

fn application_root(path: &Path) -> library_core::Result<PathBuf> {
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(component, Component::ParentDir | Component::CurDir)
                || component
                    .as_os_str()
                    .as_encoded_bytes()
                    .iter()
                    .any(|byte| byte.is_ascii_control())
        })
    {
        return Err(AppError::InvalidInput(
            "The application profile path is invalid".into(),
        ));
    }
    Ok(path.to_owned())
}

fn bundled_engine(executable: &Path, development: Option<&Path>) -> library_core::Result<PathBuf> {
    let parent = executable
        .parent()
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            AppError::InvalidInput("The application executable path is invalid".into())
        })?;
    let packaged = parent.join(MOBITOOL_NAME);
    match fs::symlink_metadata(&packaged) {
        Ok(_) => return validate_engine(&packaged),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if let Some(path) = development {
        return validate_engine(path);
    }
    Err(AppError::NotFound(
        "The bundled conversion engine is missing".into(),
    ))
}

fn validate_engine(path: &Path) -> library_core::Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)?;
    if !path.is_absolute() || !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(AppError::InvalidInput(
            "The bundled conversion engine is invalid".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(AppError::InvalidInput(
                "The bundled conversion engine is not executable".into(),
            ));
        }
    }
    Ok(path.to_owned())
}

fn system_language(raw: &str) -> String {
    let locale = raw.split(['.', '@']).next().unwrap_or_default();
    let segments: Vec<_> = locale.split(['_', '-']).collect();
    if raw.len() > 64
        || segments.is_empty()
        || !matches!(segments[0].len(), 2..=8)
        || segments[0].eq_ignore_ascii_case("posix")
        || segments.iter().any(|segment| {
            segment.is_empty()
                || segment.len() > 8
                || !segment.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
    {
        return "en".into();
    }
    segments.join("-")
}

fn public_error(error: &AppError) -> PublicError {
    match error {
        AppError::Provider(_) => public_provider_error(error),
        _ => PublicError::from(error),
    }
}

async fn blocking<T: Send + 'static>(
    state: &AppState,
    operation: impl FnOnce(&LibraryManager) -> library_core::Result<T> + Send + 'static,
) -> IpcResult<T> {
    let manager = state.manager()?;
    tauri::async_runtime::spawn_blocking(move || operation(&manager))
        .await
        .map_err(|_| PublicError {
            code: ErrorCode::Internal,
            message: "A background operation could not finish".into(),
            retryable: false,
            detail: None,
        })?
        .map_err(|error| public_error(&error))
}

#[tauri::command]
pub async fn app_bootstrap(state: State<'_, AppState>) -> IpcResult<Value> {
    blocking(state.inner(), |manager| {
        let mut bootstrap = manager.bootstrap()?;
        let language = system_language(
            bootstrap
                .get("systemLanguage")
                .and_then(Value::as_str)
                .unwrap_or("en"),
        );
        bootstrap["systemLanguage"] = language.into();
        Ok(bootstrap)
    })
    .await
}

#[tauri::command]
pub async fn library_list(state: State<'_, AppState>, query: BookQuery) -> IpcResult<BookPage> {
    blocking(state.inner(), move |manager| manager.library_list(&query)).await
}

#[tauri::command]
pub async fn library_facets(state: State<'_, AppState>) -> IpcResult<LibraryFacets> {
    blocking(state.inner(), LibraryManager::library_facets).await
}

#[tauri::command]
pub async fn book_get(state: State<'_, AppState>, id: String) -> IpcResult<Book> {
    blocking(state.inner(), move |manager| manager.book_get(&id)).await
}

#[tauri::command]
pub async fn book_files(state: State<'_, AppState>, id: String) -> IpcResult<Vec<BookFile>> {
    blocking(state.inner(), move |manager| manager.library.files(&id)).await
}

#[tauri::command]
pub async fn import_books(state: State<'_, AppState>, paths: Vec<String>) -> IpcResult<Job> {
    blocking(state.inner(), move |manager| manager.import_books(&paths)).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn book_update(
    state: State<'_, AppState>,
    id: String,
    patch: BookPatch,
    expected_revision: u64,
) -> IpcResult<Book> {
    blocking(state.inner(), move |manager| {
        manager.book_update(&id, &patch, expected_revision)
    })
    .await
}

#[tauri::command]
pub async fn book_enrich(state: State<'_, AppState>, id: String) -> IpcResult<Job> {
    blocking(state.inner(), move |manager| manager.book_enrich(&id)).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn book_optimize(
    state: State<'_, AppState>,
    id: String,
    profile_id: String,
) -> IpcResult<Job> {
    blocking(state.inner(), move |manager| {
        manager.book_optimize(&id, &profile_id)
    })
    .await
}

#[tauri::command]
pub async fn book_convert(
    state: State<'_, AppState>,
    id: String,
    format: BookFormat,
) -> IpcResult<Job> {
    blocking(state.inner(), move |manager| {
        manager.book_convert(&id, format)
    })
    .await
}

#[tauri::command]
pub async fn conversion_capabilities(
    state: State<'_, AppState>,
) -> IpcResult<ConversionCapabilities> {
    blocking(state.inner(), |manager| {
        Ok(manager.conversion_capabilities())
    })
    .await
}

#[tauri::command]
pub async fn optimization_profiles(
    state: State<'_, AppState>,
) -> IpcResult<Vec<OptimizationProfile>> {
    blocking(state.inner(), |_| Ok(optimizer::profiles())).await
}

#[tauri::command]
pub async fn devices_scan(state: State<'_, AppState>) -> IpcResult<Vec<Device>> {
    state
        .manager()?
        .devices_scan()
        .await
        .map_err(|error| public_error(&error))
}

#[tauri::command]
pub async fn device_index(state: State<'_, AppState>, id: String) -> IpcResult<Job> {
    blocking(state.inner(), move |manager| manager.device_index(&id)).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn device_inventory(
    state: State<'_, AppState>,
    id: String,
    offset: u64,
    limit: u64,
    unknown_only: bool,
) -> IpcResult<library_core::devices::DeviceInventoryPage> {
    blocking(state.inner(), move |manager| {
        manager.device_inventory(&id, offset, limit, unknown_only)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn device_import(
    state: State<'_, AppState>,
    id: String,
    relative_paths: Option<Vec<String>>,
) -> IpcResult<Job> {
    blocking(state.inner(), move |manager| {
        manager.device_import(&id, relative_paths)
    })
    .await
}

#[tauri::command]
pub async fn device_connect_wireless(
    state: State<'_, AppState>,
    address: String,
    transport: DeviceTransport,
    label: String,
    password: Option<String>,
) -> IpcResult<Device> {
    state
        .manager()?
        .device_connect_wireless(&address, transport, &label, password.as_deref())
        .await
        .map_err(|error| public_error(&error))
}

#[tauri::command]
pub async fn device_disconnect(state: State<'_, AppState>, id: String) -> IpcResult<()> {
    state
        .manager()?
        .device_disconnect(&id)
        .await
        .map_err(|error| public_error(&error))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn device_transfer(
    state: State<'_, AppState>,
    id: String,
    book_ids: Vec<String>,
    profile_id: Option<String>,
) -> IpcResult<Job> {
    blocking(state.inner(), move |manager| {
        manager.device_transfer(&id, &book_ids, profile_id.as_deref())
    })
    .await
}

#[tauri::command]
pub async fn reader_open(state: State<'_, AppState>, id: String) -> IpcResult<ReaderManifest> {
    blocking(state.inner(), move |manager| manager.reader.open(&id)).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn reader_section(
    state: State<'_, AppState>,
    id: String,
    section_index: u64,
) -> IpcResult<ReaderSection> {
    blocking(state.inner(), move |manager| {
        manager.reader.section(&id, section_index)
    })
    .await
}

#[tauri::command]
pub async fn reader_save_progress(
    state: State<'_, AppState>,
    id: String,
    location: String,
    progress: f64,
) -> IpcResult<()> {
    blocking(state.inner(), move |manager| {
        manager.reader_save_progress(&id, &location, progress)
    })
    .await
}

#[tauri::command]
pub async fn providers_list(state: State<'_, AppState>) -> IpcResult<Vec<Provider>> {
    blocking(state.inner(), |manager| Ok(manager.providers.list())).await
}

#[tauri::command]
pub async fn provider_models(
    state: State<'_, AppState>,
    id: ProviderId,
    force: bool,
) -> IpcResult<ModelCatalog> {
    state
        .manager()?
        .providers
        .models(id, force)
        .await
        .map_err(|error| public_error(&error))
}

#[tauri::command]
pub async fn provider_set_secret(
    state: State<'_, AppState>,
    id: ProviderId,
    secret: String,
    persist: bool,
) -> IpcResult<()> {
    blocking(state.inner(), move |manager| {
        manager.provider_set_secret(id, &secret, persist)
    })
    .await
}

#[tauri::command]
pub async fn provider_clear_secret(state: State<'_, AppState>, id: ProviderId) -> IpcResult<()> {
    blocking(state.inner(), move |manager| {
        manager.provider_clear_secret(id)
    })
    .await
}

#[tauri::command]
pub async fn conversations_list(state: State<'_, AppState>) -> IpcResult<Vec<Conversation>> {
    blocking(state.inner(), |manager| manager.chat.conversations()).await
}

#[tauri::command]
pub async fn conversation_messages(
    state: State<'_, AppState>,
    id: String,
) -> IpcResult<Vec<ChatMessage>> {
    blocking(state.inner(), move |manager| manager.chat.messages(&id)).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn chat_send(
    state: State<'_, AppState>,
    conversation_id: Option<String>,
    text: String,
    book_ids: Vec<String>,
) -> IpcResult<Job> {
    state
        .manager()?
        .chat_send(conversation_id.as_deref(), &text, &book_ids)
        .await
        .map_err(|error| public_error(&error))
}

#[tauri::command]
pub async fn settings_get(state: State<'_, AppState>) -> IpcResult<Settings> {
    blocking(state.inner(), |manager| manager.settings.get()).await
}

#[tauri::command]
pub async fn settings_save(state: State<'_, AppState>, settings: Settings) -> IpcResult<Settings> {
    blocking(state.inner(), move |manager| {
        manager.settings_save(&settings)
    })
    .await
}

#[tauri::command]
pub async fn jobs_list(state: State<'_, AppState>) -> IpcResult<Vec<Job>> {
    blocking(state.inner(), |manager| manager.jobs.list()).await
}

#[tauri::command]
pub async fn job_cancel(state: State<'_, AppState>, id: String) -> IpcResult<Job> {
    blocking(state.inner(), move |manager| manager.job_cancel(&id)).await
}

#[tauri::command]
pub async fn operations_list(state: State<'_, AppState>) -> IpcResult<Vec<Operation>> {
    blocking(state.inner(), |manager| manager.library.operations()).await
}

#[tauri::command]
pub async fn operation_undo(state: State<'_, AppState>, id: String) -> IpcResult<Operation> {
    blocking(state.inner(), move |manager| manager.operation_undo(&id)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::AtomicU64,
        time::{SystemTime, UNIX_EPOCH},
    };

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let unique = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "library-manager-shell-{timestamp}-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn engine(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, b"test fixture, never executed").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn locale_uses_system_value_with_safe_fallback_and_supports_additional_languages() {
        for (raw, expected) in [
            ("fr_FR.UTF-8", "fr-FR"),
            ("en_GB.UTF-8", "en-GB"),
            ("de_DE@euro", "de-DE"),
            ("zh_Hant_TW", "zh-Hant-TW"),
            ("C", "en"),
            ("POSIX", "en"),
            ("", "en"),
            ("fr\nprivate", "en"),
            ("../../fr", "en"),
        ] {
            assert_eq!(system_language(raw), expected);
        }
        assert_eq!(system_language(&"x".repeat(65)), "en");
    }

    #[test]
    fn profile_root_rejects_relative_parent_and_control_paths() {
        assert_eq!(
            application_root(Path::new("/tmp/library-manager-profile")).unwrap(),
            PathBuf::from("/tmp/library-manager-profile")
        );
        for path in ["relative/profile", "/tmp/../other", "/tmp/profile\nprivate"] {
            assert!(application_root(Path::new(path)).is_err());
        }
    }

    #[test]
    fn engine_prefers_packaged_location_and_only_uses_explicit_development_fallback() {
        let directory = TestDirectory::new();
        let development = directory.engine("development-engine");
        let executable = directory.0.join("library-manager");
        assert!(bundled_engine(&executable, None).is_err());
        assert_eq!(
            bundled_engine(&executable, Some(&development)).unwrap(),
            development
        );
        let packaged = directory.engine(MOBITOOL_NAME);
        assert_eq!(
            bundled_engine(&executable, Some(&development)).unwrap(),
            packaged
        );
        assert!(bundled_engine(Path::new("relative/executable"), Some(&development)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn engine_refuses_symlinks_nonexecutables_and_unsafe_packaged_shadowing() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = TestDirectory::new();
        let development = directory.engine("development-engine");
        let executable = directory.0.join("library-manager");
        let packaged = directory.0.join(MOBITOOL_NAME);
        symlink(&development, &packaged).unwrap();
        assert!(bundled_engine(&executable, Some(&development)).is_err());
        fs::remove_file(&packaged).unwrap();
        let packaged = directory.engine(MOBITOOL_NAME);
        fs::set_permissions(packaged, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(bundled_engine(&executable, Some(&development)).is_err());
        assert!(validate_engine(&directory.0).is_err());
    }

    #[test]
    fn initialization_error_remains_visible_without_an_empty_success_and_shutdown_signals_drop() {
        let error = public_error(&AppError::Io(std::io::Error::other("private-profile-path")));
        let state = AppState::from_initialization(Err(error.clone()));
        let shutdown = Arc::clone(&state.shutdown);
        assert_eq!(state.manager().err(), Some(error));
        let serialized = serde_json::to_string(&state.manager().err()).unwrap();
        assert!(serialized.contains("internal"));
        assert!(!serialized.contains("private-profile-path"));
        assert!(!shutdown.load(Ordering::Acquire));
        drop(state);
        assert!(shutdown.load(Ordering::Acquire));
    }

    #[test]
    fn provider_errors_keep_useful_codes_without_keys_urls_or_raw_details() {
        for (raw, code) in [
            (
                "Configure an API key Bearer private-token",
                ErrorCode::ProviderNotConfigured,
            ),
            (
                "The system secret store locked Authorization private-token",
                ErrorCode::SecretStoreUnavailable,
            ),
            (
                "rate limit https://provider/?key=private-token",
                ErrorCode::RateLimited,
            ),
            (
                "Authorization: Bearer private-token",
                ErrorCode::ProviderError,
            ),
        ] {
            let error = public_error(&AppError::Provider(raw.into()));
            assert_eq!(error.code, code);
            assert_eq!(error.detail, None);
            let value = serde_json::to_string(&error).unwrap();
            assert!(!value.contains("private-token") && !value.contains("Authorization"));
        }
        assert_eq!(
            public_error(&AppError::Unsupported("format".into())).code,
            ErrorCode::UnsupportedFormat
        );
    }
}
