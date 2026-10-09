//! Validated application preferences, independent of provider credentials.

use std::{fs, path::Path};

use rusqlite::{OptionalExtension, params};

use crate::{AppError, Database, Result, Settings, optimizer};

const SETTINGS_KEY: &str = "application";
const MAX_SETTINGS_BYTES: usize = 64 * 1024;
const MAX_MODEL_ID_BYTES: usize = 256;
const DEVICE_PROFILES: &[&str] = &[
    "generic",
    "xteink",
    "kindle",
    "kobo",
    "pocketbook",
    "tolino",
];

#[derive(Debug, Clone)]
pub struct SettingsService {
    database: Database,
    library_root: String,
}

impl SettingsService {
    pub fn new(database: Database, library_root: &Path) -> Result<Self> {
        if !library_root.is_absolute() {
            return Err(invalid(
                "Library root must be an absolute managed directory",
            ));
        }
        let metadata = fs::symlink_metadata(library_root)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(invalid("Library root must be a real managed directory"));
        }
        let root = fs::canonicalize(library_root)?;
        let library_root = root
            .to_str()
            .ok_or_else(|| invalid("Library root must be valid UTF-8"))?
            .to_owned();
        Ok(Self {
            database,
            library_root,
        })
    }

    pub fn get(&self) -> Result<Settings> {
        let connection = self.database.connect()?;
        let value: Option<Option<String>> = connection
            .query_row(
                "SELECT CASE WHEN length(CAST(value_json AS BLOB)) <= ?1 THEN value_json ELSE NULL END FROM settings WHERE key = ?2",
                params![MAX_SETTINGS_BYTES as i64, SETTINGS_KEY],
                |row| row.get(0),
            )
            .optional()?;
        let mut settings = match value {
            None => Settings::default(),
            Some(Some(json)) => serde_json::from_str::<Settings>(&json)
                .map_err(|_| invalid("Saved application settings are invalid"))?,
            Some(None) => {
                return Err(invalid(
                    "Saved application settings exceed their size limit",
                ));
            }
        };
        // Paths are profile capabilities, never an editable persisted setting.
        if !settings.library_root.is_empty() && settings.library_root != self.library_root {
            return Err(invalid(
                "Saved settings cannot redirect the managed library",
            ));
        }
        settings.library_root = self.library_root.clone();
        validate(&settings)?;
        Ok(settings)
    }

    pub fn save(&self, settings: &Settings) -> Result<Settings> {
        if settings.library_root != self.library_root {
            return Err(invalid(
                "The managed library root cannot be changed in preferences",
            ));
        }
        validate(settings)?;
        let mut stored = settings.clone();
        // Moving a whole profile must not retain an obsolete filesystem path.
        stored.library_root.clear();
        let json = serde_json::to_string(&stored)?;
        if json.len() > MAX_SETTINGS_BYTES {
            return Err(invalid("Application settings exceed their size limit"));
        }
        let connection = self.database.connect()?;
        connection.execute(
            "INSERT INTO settings (key, value_json) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
            params![SETTINGS_KEY, json],
        )?;
        Ok(settings.clone())
    }
}

fn validate(settings: &Settings) -> Result<()> {
    if !valid_locale(&settings.language) {
        return Err(invalid(
            "Language must be system or a valid locale identifier",
        ));
    }
    // ProviderId and Theme are closed serde enums: unknown values never enter.
    match (&settings.provider_id, &settings.model_id) {
        (None, None) => {}
        (Some(_), Some(model)) if valid_model(model) => {}
        _ => {
            return Err(invalid(
                "Provider and nonempty model must be selected together",
            ));
        }
    }
    if !(1..=4).contains(&settings.max_concurrent_jobs) {
        return Err(invalid("Concurrent jobs must be between one and four"));
    }
    if !settings.auto_apply_confidence.is_finite()
        || !(0.0..=1.0).contains(&settings.auto_apply_confidence)
    {
        return Err(invalid(
            "Automatic enrichment confidence must be finite and between zero and one",
        ));
    }
    if !DEVICE_PROFILES.contains(&settings.default_device_profile.as_str()) {
        return Err(invalid("Unknown device profile"));
    }
    optimizer::profile(&settings.default_optimization_profile)
        .map_err(|_| invalid("Unknown optimization profile"))?;
    Ok(())
}

fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= MAX_MODEL_ID_BYTES
        && model.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'_' | b'.' | b'/' | b':' | b'@' | b'+')
        })
}

fn valid_locale(locale: &str) -> bool {
    if locale == "system" {
        return true;
    }
    if locale.len() > 64 {
        return false;
    }
    let mut parts = locale.split('-');
    let Some(language) = parts.next() else {
        return false;
    };
    (2..=3).contains(&language.len())
        && language.bytes().all(|byte| byte.is_ascii_lowercase())
        && parts.all(|part| {
            !part.is_empty()
                && part.len() <= 8
                && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

fn invalid(message: &str) -> AppError {
    AppError::InvalidInput(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProviderId, Theme};
    use tempfile::TempDir;

    fn fixture() -> (TempDir, SettingsService) {
        let directory = TempDir::new().unwrap();
        let database = Database::new(&directory.path().join("profile")).unwrap();
        let root = directory.path().join("books");
        fs::create_dir(&root).unwrap();
        (directory, SettingsService::new(database, &root).unwrap())
    }

    #[test]
    fn defaults_and_persistence_keep_fixed_root_and_other_settings_keys() {
        let (_directory, service) = fixture();
        let mut settings = service.get().unwrap();
        assert!(Path::new(&settings.library_root).is_absolute());
        assert_eq!(settings.language, "system");
        let connection = service.database.connect().unwrap();
        connection.execute("INSERT INTO settings(key,value_json) VALUES('provider-private-config', '\"unchanged\"')", []).unwrap();
        settings.language = "fr".into();
        settings.theme = Theme::Dark;
        settings.provider_id = Some(ProviderId::Claude);
        settings.model_id = Some("claude-sonnet-4-5".into());
        settings.auto_enrich = false;
        settings.web_enabled = false;
        settings.auto_apply_confidence = 0.0;
        settings.max_concurrent_jobs = 4;
        settings.default_device_profile = "xteink".into();
        settings.default_optimization_profile = "textOnly".into();
        assert_eq!(service.save(&settings).unwrap(), settings);
        assert_eq!(service.get().unwrap(), settings);
        assert_eq!(
            connection
                .query_row(
                    "SELECT value_json FROM settings WHERE key='provider-private-config'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "\"unchanged\""
        );
        let json = connection
            .query_row(
                "SELECT value_json FROM settings WHERE key=?1",
                [SETTINGS_KEY],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Settings>(&json)
                .unwrap()
                .library_root,
            ""
        );
        let reopened =
            SettingsService::new(service.database.clone(), Path::new(&settings.library_root))
                .unwrap();
        assert_eq!(reopened.get().unwrap(), settings);
    }

    #[test]
    fn invalid_changes_never_overwrite_valid_preferences() {
        let (_directory, service) = fixture();
        let original = service.get().unwrap();
        service.save(&original).unwrap();
        let mut cases = Vec::new();
        let mut value = original.clone();
        value.library_root = "/outside".into();
        cases.push(value);
        let mut value = original.clone();
        value.language = "../../fr".into();
        cases.push(value);
        let mut value = original.clone();
        value.provider_id = Some(ProviderId::Mistral);
        cases.push(value);
        let mut value = original.clone();
        value.model_id = Some("mistral-small".into());
        cases.push(value);
        let mut value = original.clone();
        value.provider_id = Some(ProviderId::Kimi);
        value.model_id = Some(" ".into());
        cases.push(value);
        let mut value = original.clone();
        value.max_concurrent_jobs = 0;
        cases.push(value);
        let mut value = original.clone();
        value.max_concurrent_jobs = 5;
        cases.push(value);
        let mut value = original.clone();
        value.auto_apply_confidence = f64::NAN;
        cases.push(value);
        let mut value = original.clone();
        value.auto_apply_confidence = 1.01;
        cases.push(value);
        let mut value = original.clone();
        value.default_device_profile = "invented".into();
        cases.push(value);
        let mut value = original.clone();
        value.default_optimization_profile = "invented".into();
        cases.push(value);
        for value in cases {
            assert!(service.save(&value).is_err());
            assert_eq!(service.get().unwrap(), original);
        }
        for locale in ["fr", "en", "de", "pt-BR", "zh-Hant-TW", "ar"] {
            let mut value = original.clone();
            value.language = locale.into();
            service.save(&value).unwrap();
            assert_eq!(service.get().unwrap().language, locale);
        }
    }

    #[test]
    fn corrupt_json_fields_are_reported_without_reset_or_path_redirection() {
        let (_directory, service) = fixture();
        let connection = service.database.connect().unwrap();
        for json in [
            r#"{"theme":"sepia"}"#,
            r#"{"providerId":"unknown"}"#,
            r#"{"autoEnrich":1}"#,
            r#"{"libraryRoot":"/outside"}"#,
            r#"{"apiKey":"not-a-preference"}"#,
            r#"{"maxConcurrentJobs":9}"#,
        ] {
            connection.execute("INSERT INTO settings(key,value_json) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json", params![SETTINGS_KEY,json]).unwrap();
            assert!(service.get().is_err());
            assert_eq!(
                connection
                    .query_row(
                        "SELECT value_json FROM settings WHERE key=?1",
                        [SETTINGS_KEY],
                        |row| row.get::<_, String>(0)
                    )
                    .unwrap(),
                json
            );
        }
        let oversized = format!("{{\"language\":\"{}\"}}", "x".repeat(MAX_SETTINGS_BYTES));
        connection
            .execute(
                "UPDATE settings SET value_json=?1 WHERE key=?2",
                params![oversized, SETTINGS_KEY],
            )
            .unwrap();
        assert!(service.get().is_err());
    }

    #[test]
    fn unsafe_roots_are_rejected_without_creating_directories() {
        let (directory, service) = fixture();
        assert!(SettingsService::new(service.database.clone(), Path::new("relative")).is_err());
        let link = directory.path().join("redirect");
        std::os::unix::fs::symlink(&service.library_root, &link).unwrap();
        assert!(SettingsService::new(service.database.clone(), &link).is_err());
        let missing = directory.path().join("missing");
        assert!(SettingsService::new(service.database, &missing).is_err());
        assert!(!missing.exists());
    }
}
