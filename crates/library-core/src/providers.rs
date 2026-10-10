//! Provider adapters: live catalogues, isolated authenticated HTTP, native secret store.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{Client, Method, StatusCode, header};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Semaphore};
use url::Url;

use crate::database::Database;
use crate::error::{AppError, Result};
use crate::models::{
    CatalogSource, ConnectionMode, ErrorCode, Model, ModelCatalog, Provider, ProviderId,
    ProviderStatus, PublicError,
};

const PROVIDERS: [ProviderId; 6] = [
    ProviderId::Zai,
    ProviderId::Kimi,
    ProviderId::Minimax,
    ProviderId::Codex,
    ProviderId::Claude,
    ProviderId::Mistral,
];
const TIMEOUT: Duration = Duration::from_secs(45);
const CACHE_HOURS: i64 = 24;
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROMPT_BYTES: usize = 512 * 1024;
const MAX_MODELS: usize = 1_000;
const MAX_PAGES: usize = 10;
const OUTPUT_TOKENS: u64 = 8_192;
const SERVICE_NAME: &str = "com.qrcommunication.library-manager";

trait SecretVault: Send + Sync {
    fn get(&self, id: ProviderId) -> Result<Option<String>>;
    fn set(&self, id: ProviderId, secret: &str) -> Result<()>;
    fn remove(&self, id: ProviderId) -> Result<()>;
}
struct NativeVault;
impl NativeVault {
    fn entry(id: ProviderId) -> Result<keyring::Entry> {
        keyring::Entry::new(SERVICE_NAME, id.as_str()).map_err(|_| vault_error())
    }
}
impl SecretVault for NativeVault {
    fn get(&self, id: ProviderId) -> Result<Option<String>> {
        match Self::entry(id)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(vault_error()),
        }
    }
    fn set(&self, id: ProviderId, secret: &str) -> Result<()> {
        Self::entry(id)?
            .set_password(secret)
            .map_err(|_| vault_error())
    }
    fn remove(&self, id: ProviderId) -> Result<()> {
        match Self::entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(vault_error()),
        }
    }
}
fn vault_error() -> AppError {
    AppError::Provider(
        "The system secret store is unavailable or locked; session-only keys remain available"
            .into(),
    )
}

#[derive(Default)]
struct Credentials {
    secrets: HashMap<ProviderId, String>,
    persistent: HashSet<ProviderId>,
    unavailable: HashSet<ProviderId>,
    generations: HashMap<ProviderId, u64>,
}

#[derive(Clone)]
pub struct ProviderService {
    database: Database,
    client: Client,
    vault: Arc<dyn SecretVault>,
    credentials: Arc<RwLock<Credentials>>,
    catalogue_locks: Arc<HashMap<ProviderId, Mutex<()>>>,
    request_limit: Arc<Semaphore>,
    #[cfg(test)]
    fixture: Option<Url>,
}
impl ProviderService {
    pub fn new(database: Database) -> Result<Self> {
        Self::with_vault(database, Arc::new(NativeVault))
    }

    fn with_vault(database: Database, vault: Arc<dyn SecretVault>) -> Result<Self> {
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TIMEOUT)
            .connect_timeout(Duration::from_secs(15))
            .user_agent(concat!("LibraryManager/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| AppError::Network("Cannot initialize provider HTTP client".into()))?;
        let mut credentials = Credentials::default();
        for id in PROVIDERS {
            match vault.get(id) {
                Ok(Some(secret)) if valid_secret(&secret) => {
                    credentials.secrets.insert(id, secret);
                    credentials.persistent.insert(id);
                }
                Ok(None) => {}
                Ok(Some(_)) | Err(_) => {
                    credentials.unavailable.insert(id);
                }
            }
        }
        Ok(Self {
            database,
            client,
            vault,
            credentials: Arc::new(RwLock::new(credentials)),
            catalogue_locks: Arc::new(
                PROVIDERS
                    .into_iter()
                    .map(|id| (id, Mutex::new(())))
                    .collect(),
            ),
            request_limit: Arc::new(Semaphore::new(2)),
            #[cfg(test)]
            fixture: None,
        })
    }

    pub fn list(&self) -> Vec<Provider> {
        let credentials = self
            .credentials
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        PROVIDERS
            .into_iter()
            .map(|id| {
                let configured = credentials.secrets.contains_key(&id);
                Provider {
                    id,
                    name: provider_name(id).into(),
                    configured,
                    connection_mode: ConnectionMode::Api,
                    supports_tools: true,
                    status: if configured {
                        ProviderStatus::Ready
                    } else if credentials.unavailable.contains(&id) {
                        ProviderStatus::Unavailable
                    } else {
                        ProviderStatus::NeedsKey
                    },
                }
            })
            .collect()
    }

    /// Persist only through the native secret store, never SQLite or configuration files.
    pub fn set_secret(&self, id: ProviderId, secret: &str, persist: bool) -> Result<()> {
        if !valid_secret(secret) {
            return Err(AppError::InvalidInput(
                "API keys require 1 to 4096 visible ASCII characters without whitespace".into(),
            ));
        }
        let mut credentials = self
            .credentials
            .write()
            .map_err(|_| AppError::Conflict("Provider credentials are busy".into()))?;
        self.database.connect()?.execute(
            "DELETE FROM provider_catalogs WHERE provider_id=?1",
            [id.as_str()],
        )?;
        if persist {
            self.vault.set(id, secret)?;
            credentials.persistent.insert(id);
        } else if credentials.persistent.contains(&id) {
            self.vault.remove(id)?;
            credentials.persistent.remove(&id);
        }
        credentials.secrets.insert(id, secret.into());
        credentials.unavailable.remove(&id);
        *credentials.generations.entry(id).or_default() += 1;
        Ok(())
    }

    pub fn clear_secret(&self, id: ProviderId) -> Result<()> {
        let mut credentials = self
            .credentials
            .write()
            .map_err(|_| AppError::Conflict("Provider credentials are busy".into()))?;
        credentials.secrets.remove(&id);
        *credentials.generations.entry(id).or_default() += 1;
        self.database.connect()?.execute(
            "DELETE FROM provider_catalogs WHERE provider_id=?1",
            [id.as_str()],
        )?;
        let result = self.vault.remove(id);
        if result.is_ok() {
            credentials.persistent.remove(&id);
            credentials.unavailable.remove(&id);
        } else {
            credentials.unavailable.insert(id);
        }
        result
    }

    fn secret(&self, id: ProviderId) -> Result<String> {
        self.credentials
            .read()
            .map_err(|_| AppError::Conflict("Provider credentials are busy".into()))?
            .secrets
            .get(&id)
            .cloned()
            .ok_or_else(|| {
                AppError::Provider("Configure an API key for this provider first".into())
            })
    }

    pub async fn models(&self, id: ProviderId, force: bool) -> Result<ModelCatalog> {
        let lock = self
            .catalogue_locks
            .get(&id)
            .ok_or_else(|| AppError::InvalidInput("Unknown provider".into()))?;
        let _guard = lock.lock().await;
        let generation = *self
            .credentials
            .read()
            .map_err(|_| AppError::Conflict("Provider credentials are busy".into()))?
            .generations
            .get(&id)
            .unwrap_or(&0);
        let cached = self.cached(id)?;
        if !force
            && let Some(catalogue) = &cached
            && !catalogue.stale
        {
            return Ok(catalogue.clone());
        }
        let result = tokio::time::timeout(TIMEOUT, self.fetch_models(id))
            .await
            .map_err(|_| AppError::Network("Provider catalogue timed out".into()))
            .and_then(std::convert::identity);
        let credentials = self
            .credentials
            .read()
            .map_err(|_| AppError::Conflict("Provider credentials are busy".into()))?;
        if *credentials.generations.get(&id).unwrap_or(&0) != generation {
            return Err(AppError::Conflict(
                "Provider key changed during catalogue refresh; retry the request".into(),
            ));
        }
        match result {
            Ok(models) => {
                let catalogue = ModelCatalog {
                    provider_id: id,
                    models,
                    source: if id == ProviderId::Zai {
                        CatalogSource::OfficialCatalog
                    } else {
                        CatalogSource::Api
                    },
                    fetched_at: Utc::now().to_rfc3339(),
                    stale: false,
                    error: None,
                };
                self.save_catalogue(&catalogue)?;
                Ok(catalogue)
            }
            Err(error) => {
                let public = public_provider_error(&error);
                self.database.connect()?.execute(
                    "UPDATE provider_catalogs SET last_error=?2 WHERE provider_id=?1",
                    params![id.as_str(), serde_json::to_string(&public)?],
                )?;
                Ok(cached
                    .map(|mut catalogue| {
                        catalogue.stale = true;
                        catalogue.error = Some(public.clone());
                        catalogue
                    })
                    .unwrap_or(ModelCatalog {
                        provider_id: id,
                        models: Vec::new(),
                        source: if id == ProviderId::Zai {
                            CatalogSource::OfficialCatalog
                        } else {
                            CatalogSource::Api
                        },
                        fetched_at: String::new(),
                        stale: true,
                        error: Some(public),
                    }))
            }
        }
    }

    fn cached(&self, id: ProviderId) -> Result<Option<ModelCatalog>> {
        let record: Option<(String, String, String, Option<String>)> = self.database.connect()?.query_row(
            "SELECT models_json,source,fetched_at,last_error FROM provider_catalogs WHERE provider_id=?1",[id.as_str()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?;
        record
            .map(|(models, source, fetched_at, last_error)| {
                let models: Vec<Model> = serde_json::from_str(&models)?;
                let age = DateTime::parse_from_rfc3339(&fetched_at)
                    .ok()
                    .map(|time| Utc::now().signed_duration_since(time));
                let stale = age
                    .is_none_or(|age| age.num_seconds() < 0 || age.num_hours() >= CACHE_HOURS)
                    || last_error.is_some();
                let source = source.parse().map_err(|_| {
                    AppError::InvalidInput("Invalid provider catalogue provenance".into())
                })?;
                Ok(ModelCatalog {
                    provider_id: id,
                    models,
                    source,
                    fetched_at,
                    stale,
                    error: last_error
                        .map(|value| serde_json::from_str(&value))
                        .transpose()?,
                })
            })
            .transpose()
    }

    fn save_catalogue(&self, catalogue: &ModelCatalog) -> Result<()> {
        self.database.connect()?.execute("INSERT INTO provider_catalogs(provider_id,models_json,source,fetched_at,last_error) VALUES(?1,?2,?3,?4,NULL) ON CONFLICT(provider_id) DO UPDATE SET models_json=excluded.models_json,source=excluded.source,fetched_at=excluded.fetched_at,last_error=NULL",params![catalogue.provider_id.as_str(),serde_json::to_string(&catalogue.models)?,catalogue.source.as_str(),catalogue.fetched_at])?;
        Ok(())
    }

    async fn fetch_models(&self, id: ProviderId) -> Result<Vec<Model>> {
        if id == ProviderId::Zai {
            let json = self
                .request(id, Method::GET, catalogue_url(id), None, None)
                .await?;
            return parse_models(id, &json);
        }
        let secret = self.secret(id)?;
        let mut models = Vec::new();
        let mut cursor: Option<String> = None;
        let mut visited = HashSet::new();
        for page in 0..MAX_PAGES {
            let mut url = Url::parse(catalogue_url(id))
                .map_err(|_| AppError::InvalidInput("Invalid provider catalogue URL".into()))?;
            if id == ProviderId::Claude {
                url.query_pairs_mut().append_pair("limit", "100");
                if let Some(cursor) = &cursor {
                    url.query_pairs_mut().append_pair("after_id", cursor);
                }
            }
            let response = self
                .request(id, Method::GET, url.as_str(), None, Some(&secret))
                .await?;
            models.extend(parse_models(id, &response)?);
            if models.len() > MAX_MODELS {
                return Err(AppError::Unsupported(
                    "Provider catalogue exceeds 1000 models".into(),
                ));
            }
            if id != ProviderId::Claude
                || response.get("has_more").and_then(Value::as_bool) != Some(true)
            {
                return normalized_models(models);
            }
            let next = response
                .get("last_id")
                .and_then(Value::as_str)
                .filter(|value| valid_model_id(value))
                .ok_or_else(|| {
                    AppError::Provider("Provider returned an invalid pagination cursor".into())
                })?;
            if !visited.insert(next.to_owned()) || page + 1 == MAX_PAGES {
                return Err(AppError::Provider(
                    "Provider catalogue pagination is incomplete".into(),
                ));
            }
            cursor = Some(next.to_owned());
        }
        Err(AppError::Provider(
            "Provider catalogue pagination is incomplete".into(),
        ))
    }

    pub async fn complete(
        &self,
        id: ProviderId,
        model: &str,
        system: &str,
        user: &str,
    ) -> Result<String> {
        if !valid_model_id(model)
            || system.len().saturating_add(user.len()) > MAX_PROMPT_BYTES
            || user.trim().is_empty()
        {
            return Err(AppError::InvalidInput(
                "Invalid model or prompt; maximum prompt size is 512 KiB".into(),
            ));
        }
        let (secret, generation) = {
            let credentials = self
                .credentials
                .read()
                .map_err(|_| AppError::Conflict("Provider credentials are busy".into()))?;
            let secret = credentials.secrets.get(&id).cloned().ok_or_else(|| {
                AppError::Provider("Configure an API key for this provider first".into())
            })?;
            (secret, *credentials.generations.get(&id).unwrap_or(&0))
        };
        let catalogue = self.models(id, false).await?;
        if !catalogue
            .models
            .iter()
            .any(|candidate| candidate.id == model)
        {
            return Err(AppError::InvalidInput(
                "Select a conversational model from the provider's catalogue".into(),
            ));
        }
        self.require_generation(id, generation)?;
        let body = completion_payload(id, model, system, user);
        let response = tokio::time::timeout(
            TIMEOUT,
            self.request(
                id,
                Method::POST,
                completion_url(id),
                Some(&body),
                Some(&secret),
            ),
        )
        .await
        .map_err(|_| AppError::Network("Provider completion timed out".into()))??;
        self.require_generation(id, generation)?;
        completion_text(id, &response)
    }

    fn require_generation(&self, id: ProviderId, expected: u64) -> Result<()> {
        let credentials = self
            .credentials
            .read()
            .map_err(|_| AppError::Conflict("Provider credentials are busy".into()))?;
        if *credentials.generations.get(&id).unwrap_or(&0) != expected {
            return Err(AppError::Conflict(
                "Provider key changed during the request; retry the operation".into(),
            ));
        }
        Ok(())
    }

    async fn request(
        &self,
        id: ProviderId,
        method: Method,
        url: &str,
        body: Option<&Value>,
        secret: Option<&str>,
    ) -> Result<Value> {
        let endpoint = Url::parse(url)
            .map_err(|_| AppError::InvalidInput("Invalid provider endpoint".into()))?;
        if !allowed_endpoint(id, &endpoint, secret.is_some()) {
            return Err(AppError::InvalidInput(
                "Provider endpoint is outside its fixed API".into(),
            ));
        }
        #[cfg(test)]
        let endpoint = if let Some(fixture) = &self.fixture {
            let mut endpoint = endpoint;
            endpoint
                .set_scheme("http")
                .map_err(|_| AppError::InvalidInput("Invalid fixture".into()))?;
            endpoint
                .set_host(fixture.host_str())
                .map_err(|_| AppError::InvalidInput("Invalid fixture".into()))?;
            endpoint
                .set_port(fixture.port())
                .map_err(|_| AppError::InvalidInput("Invalid fixture".into()))?;
            endpoint
        } else {
            endpoint
        };
        let _permit = self
            .request_limit
            .acquire()
            .await
            .map_err(|_| AppError::Cancelled)?;
        for attempt in 0..=2_u32 {
            let mut request = self
                .client
                .request(method.clone(), endpoint.clone())
                .header(header::ACCEPT, "application/json")
                .header(header::ACCEPT_ENCODING, "identity");
            if let Some(secret) = secret {
                let mut value = header::HeaderValue::from_str(if id == ProviderId::Claude {
                    secret
                } else {
                    ""
                })
                .map_err(|_| AppError::InvalidInput("Invalid API key".into()))?;
                if id == ProviderId::Claude {
                    value.set_sensitive(true);
                    request = request
                        .header("x-api-key", value)
                        .header("anthropic-version", "2023-06-01");
                } else {
                    let mut value = header::HeaderValue::from_str(&format!("Bearer {secret}"))
                        .map_err(|_| AppError::InvalidInput("Invalid API key".into()))?;
                    value.set_sensitive(true);
                    request = request.header(header::AUTHORIZATION, value);
                }
            }
            if let Some(body) = body {
                request = request.json(body);
            }
            let mut response = request
                .send()
                .await
                .map_err(|_| AppError::Network("Provider connection failed".into()))?;
            let status = response.status();
            if (status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()) && attempt < 2
            {
                let delay = retry_delay(
                    response
                        .headers()
                        .get(header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok()),
                    attempt,
                );
                drop(response);
                tokio::time::sleep(delay).await;
                continue;
            }
            if !status.is_success() {
                return Err(status_error(status));
            }
            if response
                .content_length()
                .is_some_and(|length| length > MAX_BODY_BYTES as u64)
            {
                return Err(AppError::Unsupported(
                    "Provider response exceeds 2 MiB".into(),
                ));
            }
            if response
                .headers()
                .get(header::CONTENT_ENCODING)
                .is_some_and(|value| value.as_bytes() != b"identity")
            {
                return Err(AppError::Unsupported(
                    "Compressed provider responses are not accepted".into(),
                ));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| AppError::Network("Provider response was interrupted".into()))?
            {
                if bytes.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
                    return Err(AppError::Unsupported(
                        "Provider response exceeds 2 MiB".into(),
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            return serde_json::from_slice(&bytes)
                .map_err(|_| AppError::Provider("Provider returned malformed JSON".into()));
        }
        Err(AppError::Provider("Provider retry limit exceeded".into()))
    }
}

fn valid_secret(secret: &str) -> bool {
    !secret.is_empty()
        && secret.len() <= 4096
        && secret.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}
fn valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
}
fn provider_name(id: ProviderId) -> &'static str {
    match id {
        ProviderId::Zai => "Z.ai",
        ProviderId::Kimi => "Kimi",
        ProviderId::Minimax => "MiniMax",
        ProviderId::Codex => "Codex / OpenAI",
        ProviderId::Claude => "Claude",
        ProviderId::Mistral => "Mistral",
    }
}
fn catalogue_url(id: ProviderId) -> &'static str {
    match id {
        ProviderId::Zai => "https://docs.z.ai/openapi.json",
        ProviderId::Kimi => "https://api.moonshot.ai/v1/models",
        ProviderId::Minimax => "https://api.minimax.io/v1/models",
        ProviderId::Codex => "https://api.openai.com/v1/models",
        ProviderId::Claude => "https://api.anthropic.com/v1/models",
        ProviderId::Mistral => "https://api.mistral.ai/v1/models",
    }
}
fn completion_url(id: ProviderId) -> &'static str {
    match id {
        ProviderId::Zai => "https://api.z.ai/api/paas/v4/chat/completions",
        ProviderId::Kimi => "https://api.moonshot.ai/v1/chat/completions",
        ProviderId::Minimax => "https://api.minimax.io/v1/chat/completions",
        ProviderId::Codex => "https://api.openai.com/v1/responses",
        ProviderId::Claude => "https://api.anthropic.com/v1/messages",
        ProviderId::Mistral => "https://api.mistral.ai/v1/chat/completions",
    }
}
fn allowed_endpoint(id: ProviderId, url: &Url, authenticated: bool) -> bool {
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let allowed = [catalogue_url(id), completion_url(id)]
        .into_iter()
        .filter(|endpoint| !(authenticated && *endpoint == "https://docs.z.ai/openapi.json"));
    allowed
        .filter_map(|endpoint| Url::parse(endpoint).ok())
        .any(|endpoint| url.host_str() == endpoint.host_str() && url.path() == endpoint.path())
}
fn status_error(status: StatusCode) -> AppError {
    AppError::Provider(
        match status.as_u16() {
            401 | 403 => "Provider authentication or account permissions were rejected",
            400 => "Provider rejected the request parameters",
            429 => "Provider rate limit reached; retry later",
            300..=399 => "Provider redirects are refused to protect API keys",
            _ => "Provider request failed",
        }
        .into(),
    )
}
fn retry_delay(value: Option<&str>, attempt: u32) -> Duration {
    let seconds = value
        .and_then(|value| value.parse::<u64>().ok())
        .or_else(|| {
            value
                .and_then(|value| DateTime::parse_from_rfc2822(value).ok())
                .map(|date| date.signed_duration_since(Utc::now()).num_seconds().max(0) as u64)
        })
        .unwrap_or(1_u64 << attempt)
        .min(10);
    Duration::from_secs(seconds)
}
pub fn public_provider_error(error: &AppError) -> PublicError {
    if matches!(
        error,
        AppError::InvalidInput(_) | AppError::Conflict(_) | AppError::Cancelled
    ) {
        return PublicError::from(error);
    }
    let (code, message, retryable) = match error {
        AppError::Provider(message) if message.starts_with("The system secret store") => (
            ErrorCode::SecretStoreUnavailable,
            "The system secret store is unavailable or locked; use a session-only key or unlock the store",
            false,
        ),
        AppError::Provider(message) if message.contains("Configure an API key") => (
            ErrorCode::ProviderNotConfigured,
            "Configure an API key for this provider first",
            false,
        ),
        AppError::Network(_) => (
            ErrorCode::NetworkUnavailable,
            "Provider request could not be reached",
            true,
        ),
        AppError::Provider(message) if message.contains("rate limit") => {
            (ErrorCode::RateLimited, "Provider rate limit reached", true)
        }
        _ => (ErrorCode::ProviderError, "Provider request failed", false),
    };
    PublicError {
        code,
        message: message.into(),
        retryable,
        detail: provider_diagnostic(error)
            .filter(|detail| is_safe_provider_detail(code, detail))
            .map(str::to_owned),
    }
}

fn provider_diagnostic(error: &AppError) -> Option<&'static str> {
    let AppError::Provider(message) = error else {
        return None;
    };
    match message.as_str() {
        "Provider authentication or account permissions were rejected" => {
            Some("providerDiagnostics.authenticationRejected")
        }
        "Provider rejected the request parameters" => Some("providerDiagnostics.requestRejected"),
        "Provider request failed"
        | "Provider retry limit exceeded"
        | "Provider returned a completion error" => Some("providerDiagnostics.requestFailed"),
        "Provider response was incomplete"
        | "Provider response was incomplete or refused"
        | "Provider returned incomplete reasoning" => {
            Some("providerDiagnostics.responseIncomplete")
        }
        "Provider returned malformed JSON" => Some("providerDiagnostics.responseMalformed"),
        "Provider returned no assistant response"
        | "Provider returned no usable final answer"
        | "Provider returned no usable assistant text" => Some("providerDiagnostics.noFinalAnswer"),
        "The AI provider returned an invalid metadata response" => {
            Some("providerDiagnostics.metadataInvalid")
        }
        _ => None,
    }
}

pub fn is_safe_provider_detail(code: ErrorCode, detail: &str) -> bool {
    code == ErrorCode::ProviderError
        && matches!(
            detail,
            "providerDiagnostics.authenticationRejected"
                | "providerDiagnostics.requestRejected"
                | "providerDiagnostics.requestFailed"
                | "providerDiagnostics.responseIncomplete"
                | "providerDiagnostics.responseMalformed"
                | "providerDiagnostics.noFinalAnswer"
                | "providerDiagnostics.metadataInvalid"
        )
}

fn parse_models(id: ProviderId, response: &Value) -> Result<Vec<Model>> {
    let mut models = Vec::new();
    if id == ProviderId::Zai {
        for name in ["ChatCompletionTextRequest", "ChatCompletionVisionRequest"] {
            if let Some(values) = response
                .pointer(&format!("/components/schemas/{name}/properties/model/enum"))
                .and_then(Value::as_array)
            {
                for value in values {
                    if let Some(id) = value.as_str().filter(|value| valid_model_id(value)) {
                        models.push(Model {
                            id: id.into(),
                            name: id.into(),
                            description:
                                "Official public catalogue; account access is checked by the API"
                                    .into(),
                            context_window: None,
                            supports_tools: None,
                        });
                    }
                }
            }
        }
    } else {
        let data = response
            .get("data")
            .and_then(Value::as_array)
            .or_else(|| response.as_array())
            .ok_or_else(|| {
                AppError::Provider("Provider returned an invalid model catalogue".into())
            })?;
        if data.len() > MAX_MODELS {
            return Err(AppError::Unsupported(
                "Provider catalogue exceeds 1000 models".into(),
            ));
        }
        for value in data {
            let Some(model) = value
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| valid_model_id(value))
            else {
                continue;
            };
            let capabilities = value.get("capabilities");
            let conversational = match id {
                ProviderId::Mistral => {
                    capabilities
                        .and_then(|caps| caps.get("completion_chat"))
                        .and_then(Value::as_bool)
                        == Some(true)
                }
                ProviderId::Codex => openai_conversational(model),
                ProviderId::Claude => {
                    value.get("lifecycle").and_then(Value::as_str) != Some("retired")
                }
                _ => !non_conversational(model),
            };
            if !conversational || value.get("archived").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            models.push(Model {
                id: model.into(),
                name: value
                    .get("display_name")
                    .or_else(|| value.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or(model)
                    .chars()
                    .take(300)
                    .collect(),
                description: value
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(1000)
                    .collect(),
                context_window: value
                    .get("max_input_tokens")
                    .or_else(|| value.get("max_context_length"))
                    .or_else(|| value.get("context_length"))
                    .and_then(Value::as_u64)
                    .filter(|value| *value > 0),
                supports_tools: capabilities
                    .and_then(|caps| caps.get("function_calling"))
                    .and_then(Value::as_bool),
            });
        }
    }
    if id == ProviderId::Claude && models.is_empty() {
        Ok(models)
    } else {
        normalized_models(models)
    }
}
fn non_conversational(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    [
        "embedding",
        "rerank",
        "whisper",
        "tts",
        "speech",
        "audio",
        "realtime",
        "image",
        "dall-e",
        "sora",
        "moderation",
        "transcribe",
        "instruct",
    ]
    .iter()
    .any(|part| id.contains(part))
}
fn openai_conversational(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    !non_conversational(&lower)
        && (lower.starts_with("gpt-")
            || lower.starts_with("chatgpt-")
            || lower.starts_with("codex-")
            || (lower.starts_with('o') && lower.as_bytes().get(1).is_some_and(u8::is_ascii_digit))
            || lower
                .strip_prefix("ft:")
                .is_some_and(|id| id.starts_with("gpt-")))
}
fn normalized_models(models: Vec<Model>) -> Result<Vec<Model>> {
    let models: BTreeMap<String, Model> = models
        .into_iter()
        .map(|model| (model.id.clone(), model))
        .collect();
    if models.is_empty() {
        return Err(AppError::Provider(
            "Provider catalogue contains no conversational models".into(),
        ));
    }
    if models.len() > MAX_MODELS {
        return Err(AppError::Unsupported(
            "Provider catalogue exceeds 1000 models".into(),
        ));
    }
    Ok(models.into_values().collect())
}
fn completion_payload(id: ProviderId, model: &str, system: &str, user: &str) -> Value {
    match id {
        ProviderId::Claude => {
            json!({"model":model,"system":system,"messages":[{"role":"user","content":user}],"max_tokens":OUTPUT_TOKENS,"stream":false})
        }
        ProviderId::Codex => {
            json!({"model":model,"instructions":system,"input":user,"max_output_tokens":OUTPUT_TOKENS,"store":false,"stream":false})
        }
        ProviderId::Minimax => {
            json!({"model":model,"messages":[{"role":"system","content":system},{"role":"user","content":user}],"max_completion_tokens":OUTPUT_TOKENS,"reasoning_split":true,"stream":false})
        }
        _ => {
            json!({"model":model,"messages":[{"role":"system","content":system},{"role":"user","content":user}],"max_tokens":OUTPUT_TOKENS,"stream":false})
        }
    }
}
fn completion_text(id: ProviderId, response: &Value) -> Result<String> {
    if response.get("error").is_some_and(|error| !error.is_null())
        || response
            .pointer("/base_resp/status_code")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0)
    {
        return Err(AppError::Provider(
            "Provider returned a completion error".into(),
        ));
    }
    let mut text = match id {
        ProviderId::Codex => {
            if response
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| status != "completed")
            {
                return Err(AppError::Provider(
                    "Provider response was incomplete".into(),
                ));
            }
            response
                .get("output")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
                .flat_map(|item| {
                    item.get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("output_text"))
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        }
        ProviderId::Claude => {
            if response
                .get("stop_reason")
                .and_then(Value::as_str)
                .is_some_and(|reason| {
                    matches!(reason, "max_tokens" | "tool_use" | "pause_turn" | "refusal")
                })
            {
                return Err(AppError::Provider(
                    "Provider response was incomplete or refused".into(),
                ));
            }
            response
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => {
            let choice = response.pointer("/choices/0").ok_or_else(|| {
                AppError::Provider("Provider returned no assistant response".into())
            })?;
            if choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .is_some_and(|reason| matches!(reason, "length" | "content_filter" | "tool_calls"))
            {
                return Err(AppError::Provider(
                    "Provider response was incomplete or refused".into(),
                ));
            }
            match choice.pointer("/message/content") {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            }
        }
    };
    // Older MiniMax responses can prepend reasoning to content despite the split request.
    // Only consume a complete leading block; tags inside the final answer are data.
    if id == ProviderId::Minimax
        && let Some(reasoning) = text.trim_start().strip_prefix("<think>")
    {
        let (reasoning, answer) = reasoning
            .split_once("</think>")
            .ok_or_else(|| AppError::Provider("Provider returned incomplete reasoning".into()))?;
        if reasoning.contains("<think>")
            || answer.trim_start().starts_with("<think>")
            || answer.trim_start().starts_with("</think>")
        {
            return Err(AppError::Provider(
                "Provider returned no usable final answer".into(),
            ));
        }
        text = answer.trim_start().to_owned();
    }
    if text.trim().is_empty() {
        return Err(AppError::Provider(
            "Provider returned no usable assistant text".into(),
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    };
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[derive(Default)]
    struct MemoryVault {
        values: StdMutex<HashMap<ProviderId, String>>,
        fail: AtomicBool,
    }
    impl SecretVault for MemoryVault {
        fn get(&self, id: ProviderId) -> Result<Option<String>> {
            if self.fail.load(Ordering::Relaxed) {
                Err(vault_error())
            } else {
                Ok(self.values.lock().unwrap().get(&id).cloned())
            }
        }
        fn set(&self, id: ProviderId, secret: &str) -> Result<()> {
            if self.fail.load(Ordering::Relaxed) {
                Err(vault_error())
            } else {
                self.values.lock().unwrap().insert(id, secret.into());
                Ok(())
            }
        }
        fn remove(&self, id: ProviderId) -> Result<()> {
            if self.fail.load(Ordering::Relaxed) {
                Err(vault_error())
            } else {
                self.values.lock().unwrap().remove(&id);
                Ok(())
            }
        }
    }
    fn service() -> (TempDir, ProviderService, Arc<MemoryVault>) {
        let directory = tempfile::tempdir().unwrap();
        let vault = Arc::new(MemoryVault::default());
        let service =
            ProviderService::with_vault(Database::new(directory.path()).unwrap(), vault.clone())
                .unwrap();
        (directory, service, vault)
    }
    fn model_list(id: &str) -> Value {
        json!({"data":[{"id":id,"capabilities":{"completion_chat":true}}]})
    }
    fn zai_list() -> Value {
        json!({"components":{"schemas":{"ChatCompletionTextRequest":{"properties":{"model":{"enum":["glm-current","glm-current"]}}},"ChatCompletionVisionRequest":{"properties":{"model":{"enum":["glm-vision"]}}}}}})
    }
    #[derive(Clone)]
    struct Reply {
        status: u16,
        body: String,
        headers: Vec<(String, String)>,
        no_length: bool,
        delay: Duration,
    }
    impl Reply {
        fn json(value: Value) -> Self {
            Self {
                status: 200,
                body: value.to_string(),
                headers: Vec::new(),
                no_length: false,
                delay: Duration::ZERO,
            }
        }
        fn error(status: u16) -> Self {
            Self {
                status,
                body: "DO NOT ECHO THIS PRIVATE ERROR".into(),
                headers: vec![("Retry-After".into(), "0".into())],
                no_length: false,
                delay: Duration::ZERO,
            }
        }
    }
    struct Server {
        url: Url,
        routes: Arc<StdMutex<HashMap<String, VecDeque<Reply>>>>,
        requests: Arc<StdMutex<Vec<String>>>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    impl Server {
        async fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
            let routes = Arc::new(StdMutex::new(HashMap::<String, VecDeque<Reply>>::new()));
            let requests = Arc::new(StdMutex::new(Vec::new()));
            let routes_clone = routes.clone();
            let requests_clone = requests.clone();
            let task = tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let routes = routes_clone.clone();
                    let requests = requests_clone.clone();
                    tokio::spawn(async move {
                        let mut bytes = Vec::new();
                        let mut chunk = [0u8; 8192];
                        let mut expected = None;
                        loop {
                            let count = socket.read(&mut chunk).await.unwrap_or(0);
                            if count == 0 {
                                break;
                            }
                            bytes.extend_from_slice(&chunk[..count]);
                            if expected.is_none()
                                && let Some(header_end) =
                                    bytes.windows(4).position(|part| part == b"\r\n\r\n")
                            {
                                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                                let length = headers
                                    .lines()
                                    .find_map(|line| {
                                        line.to_ascii_lowercase()
                                            .strip_prefix("content-length:")
                                            .and_then(|length| length.trim().parse::<usize>().ok())
                                    })
                                    .unwrap_or(0);
                                expected = Some(header_end + 4 + length);
                            }
                            if expected.is_some_and(|length| bytes.len() >= length)
                                || bytes.len() > MAX_PROMPT_BYTES + 8192
                            {
                                break;
                            }
                        }
                        let request = String::from_utf8_lossy(&bytes).into_owned();
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .to_owned();
                        requests.lock().unwrap().push(request);
                        let reply = {
                            let mut routes = routes.lock().unwrap();
                            let route = routes.get_mut(&path);
                            match route {
                                Some(replies) if replies.len() > 1 => replies.pop_front().unwrap(),
                                Some(replies) => {
                                    replies.front().cloned().unwrap_or(Reply::error(404))
                                }
                                None => Reply::error(404),
                            }
                        };
                        tokio::time::sleep(reply.delay).await;
                        let mut head = format!(
                            "HTTP/1.1 {} Response\r\nConnection: close\r\nContent-Type: application/json\r\n",
                            reply.status
                        );
                        if !reply.no_length {
                            head.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
                        }
                        for (key, value) in reply.headers {
                            head.push_str(&format!("{key}: {value}\r\n"));
                        }
                        head.push_str("\r\n");
                        let _ = socket.write_all(head.as_bytes()).await;
                        let _ = socket.write_all(reply.body.as_bytes()).await;
                        let _ = socket.shutdown().await;
                    });
                }
            });
            Self {
                url,
                routes,
                requests,
                task,
            }
        }
        fn route(&self, path: &str, replies: Vec<Reply>) {
            self.routes
                .lock()
                .unwrap()
                .insert(path.into(), replies.into());
        }
        fn attach(&self, service: &mut ProviderService) {
            service.fixture = Some(self.url.clone());
        }
    }

    #[test]
    fn six_catalogues_are_parsed_filtered_deduplicated_and_never_invented() {
        assert_eq!(parse_models(ProviderId::Zai, &zai_list()).unwrap().len(), 2);
        for id in [ProviderId::Kimi, ProviderId::Minimax] {
            assert_eq!(
                parse_models(
                    id,
                    &json!({"data":[{"id":"chat-current"},{"id":"text-embedding"}]})
                )
                .unwrap()
                .len(),
                1
            );
        }
        assert_eq!(parse_models(ProviderId::Mistral,&json!({"data":[{"id":"mistral-current","capabilities":{"completion_chat":true},"max_context_length":32000},{"id":"codestral-fim","capabilities":{"completion_chat":false}}]})).unwrap()[0].context_window,Some(32000));
        let models=parse_models(ProviderId::Codex,&json!({"data":[{"id":"gpt-current"},{"id":"codex-current"},{"id":"o7"},{"id":"text-embedding"},{"id":"gpt-audio"},{"id":"whisper-1"}]})).unwrap();
        assert_eq!(models.len(), 3);
        let models=parse_models(ProviderId::Claude,&json!({"data":[{"id":"claude-current","display_name":"Current Claude","max_input_tokens":200000,"lifecycle":"active"},{"id":"claude-old","lifecycle":"retired"}]})).unwrap();
        assert_eq!(models[0].name, "Current Claude");
        assert_eq!(models[0].context_window, Some(200000));
        assert!(parse_models(ProviderId::Zai, &json!({})).is_err());
        assert!(parse_models(ProviderId::Codex, &model_list("embedding-model")).is_err());
        assert!(
            parse_models(ProviderId::Kimi, &json!({"data":[{"id":"key\ninjection"}]})).is_err()
        );
    }

    #[test]
    fn native_completion_payloads_and_nested_texts_preserve_provider_contracts() {
        for id in PROVIDERS {
            let body = completion_payload(id, "chat-current", "System", "User");
            assert_eq!(body["model"], "chat-current");
            match id {
                ProviderId::Codex => {
                    assert_eq!(body["store"], false);
                    assert_eq!(body["instructions"], "System");
                    assert_eq!(body["max_output_tokens"], OUTPUT_TOKENS);
                }
                ProviderId::Claude => {
                    assert_eq!(body["system"], "System");
                    assert_eq!(body["messages"][0]["role"], "user");
                }
                ProviderId::Minimax => {
                    assert_eq!(body["max_completion_tokens"], OUTPUT_TOKENS);
                    assert_eq!(body["reasoning_split"], true);
                    assert!(body.get("max_tokens").is_none());
                }
                _ => assert_eq!(body["messages"][0]["role"], "system"),
            }
            if id != ProviderId::Minimax {
                assert!(body.get("reasoning_split").is_none());
            }
            let response = match id {
                ProviderId::Codex => {
                    json!({"status":"completed","output":[{"type":"reasoning","summary":[]},{"type":"message","content":[{"type":"output_text","text":"Answer"}]}]})
                }
                ProviderId::Claude => {
                    json!({"stop_reason":"end_turn","content":[{"type":"thinking","thinking":"Private reasoning"},{"type":"text","text":"Answer"}]})
                }
                _ => json!({"choices":[{"finish_reason":"stop","message":{"content":"Answer"}}]}),
            };
            assert_eq!(completion_text(id, &response).unwrap(), "Answer");
        }
        assert!(
            completion_text(
                ProviderId::Codex,
                &json!({"status":"incomplete","output":[]})
            )
            .is_err()
        );
        assert!(
            completion_text(
                ProviderId::Claude,
                &json!({"stop_reason":"max_tokens","content":[{"type":"text","text":"Partial"}]})
            )
            .is_err()
        );
        assert!(
            completion_text(
                ProviderId::Minimax,
                &json!({"base_resp":{"status_code":1004},"choices":[]})
            )
            .is_err()
        );
        assert!(
            completion_text(
                ProviderId::Kimi,
                &json!({"choices":[{"finish_reason":"length","message":{"content":"Partial"}}]})
            )
            .is_err()
        );
    }

    #[test]
    fn minimax_legacy_reasoning_requires_a_complete_prefix_and_preserves_final_content() {
        let answer = r#"{"patch":{"description":"literal <think> text"},"confidence":0.9,"evidence":[],"warnings":[]}"#;
        let response = |content: &str| json!({"choices":[{"finish_reason":"stop","message":{"content":content}}]});
        assert_eq!(
            completion_text(ProviderId::Minimax, &response(answer)).unwrap(),
            answer
        );
        assert_eq!(
            completion_text(
                ProviderId::Minimax,
                &response(&format!(" \n<think>Private reasoning</think>\n{answer}"))
            )
            .unwrap(),
            answer
        );
        for incomplete in [
            "<think>Private reasoning",
            "<think>Private reasoning</think>",
            "<think>First</think><think>Second</think>Answer",
        ] {
            let error = completion_text(ProviderId::Minimax, &response(incomplete)).unwrap_err();
            assert!(matches!(error, AppError::Provider(_)));
            assert!(!error.to_string().contains("Private reasoning"));
        }
        let original = "<think>Other provider text</think>Answer";
        assert_eq!(
            completion_text(ProviderId::Kimi, &response(original)).unwrap(),
            original
        );
    }

    #[test]
    fn minimax_nested_reasoning_and_stray_closing_blocks_are_rejected() {
        for content in [
            "<think>Private outer<think>Private inner</think>Private tail</think>Answer",
            "<think>Private reasoning</think></think>Answer",
        ] {
            let response =
                json!({"choices":[{"finish_reason":"stop","message":{"content":content}}]});
            let error = completion_text(ProviderId::Minimax, &response).unwrap_err();
            assert!(matches!(error, AppError::Provider(_)));
            assert!(!error.to_string().contains("Private"));
        }
    }

    #[test]
    fn session_persistence_and_unavailable_secret_store_have_no_plaintext_fallback() {
        let (_directory, service, vault) = service();
        assert!(
            service
                .list()
                .iter()
                .all(|provider| provider.status == ProviderStatus::NeedsKey)
        );
        for bad in ["", "bad key", "bad\nheader", "clé"] {
            assert!(service.set_secret(ProviderId::Kimi, bad, false).is_err());
        }
        service
            .set_secret(ProviderId::Kimi, "session-only-key", false)
            .unwrap();
        assert!(vault.values.lock().unwrap().is_empty());
        assert!(
            service
                .list()
                .iter()
                .find(|provider| provider.id == ProviderId::Kimi)
                .unwrap()
                .configured
        );
        assert!(
            !serde_json::to_string(&service.list())
                .unwrap()
                .contains("session-only-key")
        );
        service
            .set_secret(ProviderId::Claude, "stored-secret", true)
            .unwrap();
        let reopened =
            ProviderService::with_vault(service.database.clone(), vault.clone()).unwrap();
        assert_eq!(
            reopened.secret(ProviderId::Claude).unwrap(),
            "stored-secret"
        );
        assert!(reopened.secret(ProviderId::Kimi).is_err());
        service
            .set_secret(ProviderId::Claude, "session-now", false)
            .unwrap();
        assert!(
            !vault
                .values
                .lock()
                .unwrap()
                .contains_key(&ProviderId::Claude)
        );
        vault.fail.store(true, Ordering::Relaxed);
        assert!(
            service
                .set_secret(ProviderId::Mistral, "do-not-store-plain", true)
                .is_err()
        );
        assert!(service.secret(ProviderId::Mistral).is_err());
        assert_eq!(
            public_provider_error(&vault_error()).code,
            ErrorCode::SecretStoreUnavailable
        );
        service
            .set_secret(ProviderId::Mistral, "session-available", false)
            .unwrap();
        assert_eq!(
            service.secret(ProviderId::Mistral).unwrap(),
            "session-available"
        );
        assert!(service.clear_secret(ProviderId::Mistral).is_err());
        assert!(service.secret(ProviderId::Mistral).is_err());
        let keys: i64 = service
            .database
            .connect()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(keys, 0);
    }

    #[test]
    fn provider_diagnostic_keys_require_exact_whitelist_members_and_provider_error_code() {
        for key in [
            "providerDiagnostics.authenticationRejected",
            "providerDiagnostics.requestRejected",
            "providerDiagnostics.requestFailed",
            "providerDiagnostics.responseIncomplete",
            "providerDiagnostics.responseMalformed",
            "providerDiagnostics.noFinalAnswer",
            "providerDiagnostics.metadataInvalid",
        ] {
            assert!(is_safe_provider_detail(ErrorCode::ProviderError, key));
            for code in [
                ErrorCode::InvalidInput,
                ErrorCode::NetworkUnavailable,
                ErrorCode::ProviderNotConfigured,
                ErrorCode::RateLimited,
                ErrorCode::SecretStoreUnavailable,
            ] {
                assert!(!is_safe_provider_detail(code, key));
            }
            for unsafe_key in [
                format!(" {key}"),
                format!("{key}\n"),
                format!("{key}.private-token"),
            ] {
                assert!(!is_safe_provider_detail(
                    ErrorCode::ProviderError,
                    &unsafe_key
                ));
            }
        }
        assert!(!is_safe_provider_detail(
            ErrorCode::ProviderError,
            "providerDiagnostics.unknown"
        ));
        assert!(!is_safe_provider_detail(
            ErrorCode::ProviderError,
            "Bearer private-token /private/profile"
        ));
    }

    #[test]
    fn public_provider_diagnostics_classify_only_exact_known_static_messages() {
        for (message, detail) in [
            (
                "Provider authentication or account permissions were rejected",
                "providerDiagnostics.authenticationRejected",
            ),
            (
                "Provider rejected the request parameters",
                "providerDiagnostics.requestRejected",
            ),
            (
                "Provider request failed",
                "providerDiagnostics.requestFailed",
            ),
            (
                "Provider retry limit exceeded",
                "providerDiagnostics.requestFailed",
            ),
            (
                "Provider returned a completion error",
                "providerDiagnostics.requestFailed",
            ),
            (
                "Provider response was incomplete",
                "providerDiagnostics.responseIncomplete",
            ),
            (
                "Provider response was incomplete or refused",
                "providerDiagnostics.responseIncomplete",
            ),
            (
                "Provider returned incomplete reasoning",
                "providerDiagnostics.responseIncomplete",
            ),
            (
                "Provider returned malformed JSON",
                "providerDiagnostics.responseMalformed",
            ),
            (
                "Provider returned no assistant response",
                "providerDiagnostics.noFinalAnswer",
            ),
            (
                "Provider returned no usable final answer",
                "providerDiagnostics.noFinalAnswer",
            ),
            (
                "Provider returned no usable assistant text",
                "providerDiagnostics.noFinalAnswer",
            ),
            (
                "The AI provider returned an invalid metadata response",
                "providerDiagnostics.metadataInvalid",
            ),
        ] {
            let error = public_provider_error(&AppError::Provider(message.into()));
            assert_eq!(error.code, ErrorCode::ProviderError);
            assert_eq!(error.message, "Provider request failed");
            assert!(!error.retryable);
            assert_eq!(error.detail.as_deref(), Some(detail));
            let unknown = public_provider_error(&AppError::Provider(format!(
                "{message}: private-token /private/profile"
            )));
            assert_eq!(unknown.detail, None);
            assert!(
                !serde_json::to_string(&unknown)
                    .unwrap()
                    .contains("private-token")
            );
        }
    }

    #[test]
    fn public_provider_diagnostics_preserve_configuration_network_and_rate_limit_routing() {
        for (error, code, retryable) in [
            (
                AppError::Provider("Configure an API key for this provider first".into()),
                ErrorCode::ProviderNotConfigured,
                false,
            ),
            (vault_error(), ErrorCode::SecretStoreUnavailable, false),
            (
                AppError::Network("Provider connection failed: private-token".into()),
                ErrorCode::NetworkUnavailable,
                true,
            ),
            (
                status_error(StatusCode::TOO_MANY_REQUESTS),
                ErrorCode::RateLimited,
                true,
            ),
        ] {
            let public = public_provider_error(&error);
            assert_eq!(public.code, code);
            assert_eq!(public.retryable, retryable);
            assert_eq!(public.detail, None);
        }
        let rejected = public_provider_error(&status_error(StatusCode::BAD_REQUEST));
        assert_eq!(rejected.code, ErrorCode::ProviderError);
        assert_eq!(
            rejected.detail.as_deref(),
            Some("providerDiagnostics.requestRejected")
        );
        let unauthorized = public_provider_error(&status_error(StatusCode::UNAUTHORIZED));
        assert_eq!(unauthorized.code, ErrorCode::ProviderError);
        assert_eq!(
            unauthorized.detail.as_deref(),
            Some("providerDiagnostics.authenticationRejected")
        );
        let unavailable = public_provider_error(&status_error(StatusCode::SERVICE_UNAVAILABLE));
        assert_eq!(unavailable.code, ErrorCode::ProviderError);
        assert_eq!(
            unavailable.detail.as_deref(),
            Some("providerDiagnostics.requestFailed")
        );
    }

    #[tokio::test]
    async fn catalogue_cache_provenance_refresh_failure_and_account_switch_are_explicit() {
        let (_directory, mut service, _vault) = service();
        let server = Server::new().await;
        server.attach(&mut service);
        server.route("/v1/models", vec![Reply::json(model_list("kimi-current"))]);
        let absent = service.models(ProviderId::Kimi, false).await.unwrap();
        assert!(absent.models.is_empty());
        assert_eq!(absent.error.unwrap().code, ErrorCode::ProviderNotConfigured);
        service
            .set_secret(ProviderId::Kimi, "test-api-key", false)
            .unwrap();
        let live = service.models(ProviderId::Kimi, false).await.unwrap();
        assert_eq!(live.source, CatalogSource::Api);
        assert!(!live.stale);
        let cached = service.models(ProviderId::Kimi, false).await.unwrap();
        assert_eq!(cached.fetched_at, live.fetched_at);
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        server.route("/v1/models", vec![Reply::error(401)]);
        let stale = service.models(ProviderId::Kimi, true).await.unwrap();
        assert!(stale.stale);
        assert!(stale.error.is_some());
        assert_eq!(stale.models.len(), 1);
        assert_eq!(stale.source, CatalogSource::Api);
        assert!(
            !serde_json::to_string(&stale)
                .unwrap()
                .contains("PRIVATE ERROR")
        );
        service
            .set_secret(ProviderId::Kimi, "different-account", false)
            .unwrap();
        assert!(service.cached(ProviderId::Kimi).unwrap().is_none());
        server.route("/openapi.json", vec![Reply::json(zai_list())]);
        let public = service.models(ProviderId::Zai, true).await.unwrap();
        assert_eq!(public.source, CatalogSource::OfficialCatalog);
        let requests = server.requests.lock().unwrap();
        let documentation = requests.last().unwrap().to_ascii_lowercase();
        assert!(!documentation.contains("authorization:"));
        assert!(!documentation.contains("x-api-key:"));
    }

    #[tokio::test]
    async fn claude_pagination_is_bounded_and_cycles_do_not_publish_partial_catalogues() {
        let (_directory, mut service, _vault) = service();
        let server = Server::new().await;
        server.attach(&mut service);
        service
            .set_secret(ProviderId::Claude, "test-key", false)
            .unwrap();
        server.route(
            "/v1/models?limit=100",
            vec![Reply::json(
                json!({"data":[{"id":"claude-first"}],"has_more":true,"last_id":"claude-first"}),
            )],
        );
        server.route(
            "/v1/models?limit=100&after_id=claude-first",
            vec![Reply::json(
                json!({"data":[{"id":"claude-second"}],"has_more":false,"last_id":"claude-second"}),
            )],
        );
        let catalogue = service.models(ProviderId::Claude, true).await.unwrap();
        assert_eq!(catalogue.models.len(), 2);
        assert!(server.requests.lock().unwrap().iter().all(|request| {
            request.to_ascii_lowercase().contains("x-api-key: test-key")
                && request.contains("anthropic-version: 2023-06-01")
        }));
        server.route(
            "/v1/models?limit=100&after_id=claude-first",
            vec![Reply::json(
                json!({"data":[{"id":"claude-second"}],"has_more":true,"last_id":"claude-first"}),
            )],
        );
        let stale = service.models(ProviderId::Claude, true).await.unwrap();
        assert!(stale.stale);
        assert_eq!(stale.models.len(), 2);
    }

    #[tokio::test]
    async fn completion_uses_selected_catalogue_and_retries_only_bounded_server_failures() {
        let (_directory, mut service, _vault) = service();
        let final_json = r#"{"patch":{},"confidence":0.9,"evidence":[],"warnings":[]}"#;
        let server = Server::new().await;
        server.attach(&mut service);
        service
            .set_secret(ProviderId::Minimax, "test-key", false)
            .unwrap();
        server.route(
            "/v1/models",
            vec![Reply::json(model_list("MiniMax-current"))],
        );
        server.route(
            "/v1/chat/completions",
            vec![
                Reply::error(429),
                Reply::error(503),
                Reply::json(
                    json!({"choices":[{"finish_reason":"stop","message":{"reasoning_content":"Private reasoning must not enter the metadata contract","content":final_json}}]}),
                ),
            ],
        );
        assert_eq!(
            service
                .complete(ProviderId::Minimax, "MiniMax-current", "System", "User")
                .await
                .unwrap(),
            final_json
        );
        let requests = server.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 4);
        let body: Value =
            serde_json::from_str(requests.last().unwrap().split_once("\r\n\r\n").unwrap().1)
                .unwrap();
        assert_eq!(body["max_completion_tokens"], OUTPUT_TOKENS);
        assert_eq!(body["reasoning_split"], true);
        assert!(
            service
                .complete(ProviderId::Minimax, "embedding-unknown", "System", "User")
                .await
                .is_err()
        );
        assert_eq!(server.requests.lock().unwrap().len(), 4);
        server.route("/v1/chat/completions", vec![Reply::error(429)]);
        assert!(
            service
                .complete(ProviderId::Minimax, "MiniMax-current", "System", "User")
                .await
                .is_err()
        );
        assert_eq!(server.requests.lock().unwrap().len(), 7);
        server.route("/v1/chat/completions", vec![Reply::error(401)]);
        let error = service
            .complete(ProviderId::Minimax, "MiniMax-current", "System", "User")
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("PRIVATE ERROR"));
        assert!(!error.to_string().contains("test-key"));
        assert_eq!(server.requests.lock().unwrap().len(), 8);
        assert_eq!(retry_delay(Some("999999"), 0), Duration::from_secs(10));
    }

    #[tokio::test]
    async fn redirects_body_caps_and_endpoint_allowlist_protect_authentication() {
        let (_directory, mut service, _vault) = service();
        let server = Server::new().await;
        server.attach(&mut service);
        service
            .set_secret(ProviderId::Kimi, "test-key", false)
            .unwrap();
        let mut redirect = Reply::error(302);
        redirect
            .headers
            .push(("Location".into(), "http://127.0.0.1/steal".into()));
        server.route("/v1/models", vec![redirect]);
        assert!(
            service
                .models(ProviderId::Kimi, true)
                .await
                .unwrap()
                .models
                .is_empty()
        );
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        let mut huge = Reply::json(json!({}));
        huge.body = "x".repeat(MAX_BODY_BYTES + 1);
        server.route("/v1/models", vec![huge.clone()]);
        assert!(
            service
                .request(
                    ProviderId::Kimi,
                    Method::GET,
                    catalogue_url(ProviderId::Kimi),
                    None,
                    Some("test-key")
                )
                .await
                .is_err()
        );
        huge.no_length = true;
        server.route("/v1/models", vec![huge]);
        assert!(
            service
                .request(
                    ProviderId::Kimi,
                    Method::GET,
                    catalogue_url(ProviderId::Kimi),
                    None,
                    Some("test-key")
                )
                .await
                .is_err()
        );
        assert!(!allowed_endpoint(
            ProviderId::Kimi,
            &Url::parse("https://api.moonshot.ai.evil.com/v1/models").unwrap(),
            true
        ));
        assert!(!allowed_endpoint(
            ProviderId::Zai,
            &Url::parse(catalogue_url(ProviderId::Zai)).unwrap(),
            true
        ));
        assert!(!allowed_endpoint(
            ProviderId::Claude,
            &Url::parse("http://api.anthropic.com/v1/models").unwrap(),
            true
        ));
        assert!(!allowed_endpoint(
            ProviderId::Codex,
            &Url::parse("https://api.openai.com/v1/files").unwrap(),
            true
        ));
    }

    #[tokio::test]
    async fn changing_accounts_during_refresh_cannot_publish_the_previous_catalogue() {
        let (_directory, mut service, _vault) = service();
        let server = Server::new().await;
        server.attach(&mut service);
        service
            .set_secret(ProviderId::Kimi, "old-account", false)
            .unwrap();
        let mut delayed = Reply::json(model_list("old-account-model"));
        delayed.delay = Duration::from_millis(100);
        server.route("/v1/models", vec![delayed]);
        let cloned = service.clone();
        let pending = tokio::spawn(async move { cloned.models(ProviderId::Kimi, true).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while server.requests.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        service
            .set_secret(ProviderId::Kimi, "new-account", false)
            .unwrap();
        assert!(matches!(pending.await.unwrap(), Err(AppError::Conflict(_))));
        assert!(service.cached(ProviderId::Kimi).unwrap().is_none());
    }

    #[tokio::test]
    #[ignore = "Anonymous live public documentation check, requires Internet"]
    async fn live_zai_catalogue_uses_public_documentation_without_secrets() {
        let (_directory, service, _vault) = service();
        let catalogue = service.models(ProviderId::Zai, true).await.unwrap();
        assert!(!catalogue.models.is_empty(), "public catalogue unavailable");
        assert!(catalogue.error.is_none());
        assert_eq!(catalogue.source, CatalogSource::OfficialCatalog);
        println!("zai_models={}", catalogue.models.len());
    }

    #[tokio::test]
    async fn public_futures_are_send_and_cached_dates_are_validated() {
        let (_directory, service, _vault) = service();
        fn require_send<T: Send>(_: T) {}
        require_send(service.models(ProviderId::Kimi, false));
        require_send(service.complete(ProviderId::Kimi, "chat", "system", "user"));
        assert!(service.fixture.is_none());
        let catalogue = ModelCatalog {
            provider_id: ProviderId::Kimi,
            models: vec![Model {
                id: "chat-current".into(),
                ..Model::default()
            }],
            source: CatalogSource::Api,
            fetched_at: "2099-01-01T00:00:00Z".into(),
            stale: false,
            error: None,
        };
        service.save_catalogue(&catalogue).unwrap();
        assert!(service.cached(ProviderId::Kimi).unwrap().unwrap().stale);
        service
            .database
            .connect()
            .unwrap()
            .execute("UPDATE provider_catalogs SET fetched_at='invalid'", [])
            .unwrap();
        assert!(service.cached(ProviderId::Kimi).unwrap().unwrap().stale);
    }
}
