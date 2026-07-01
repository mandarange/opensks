use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::Path,
    time::{Duration, Instant},
};

use opensks_contracts::{
    CapabilityRequirements, ConversationTurnSettings, HealthState, LatencyBucket,
    MODEL_CATALOG_ENTRY_SCHEMA, MODEL_PROFILE_SCHEMA, ModelCapabilities, ModelCatalogEntry,
    ModelLimits, ModelProfile, ModelRejection, ModelRole, ModelRouteReceipt,
    PROVIDER_CONNECTION_SCHEMA, PROVIDER_MUTATION_SCHEMA, PROVIDER_PROBE_RECEIPT_SCHEMA,
    ProviderConnection, ProviderDescriptor, ProviderHealthSnapshot, ProviderMutationKind,
    ProviderMutationReceipt, ProviderProbeHttpCategory, ProviderProbeReceipt, RoleScore,
    RoutingDecision, RoutingStatus, SECRET_REF_SCHEMA, SecretlessConfigRef,
};
use opensks_policy::{PermissionPolicy, PermissionScope};
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

pub const ROUTING_DECISION_SCHEMA: &str = opensks_contracts::ROUTING_DECISION_SCHEMA;
pub const PROVIDER_DESCRIPTOR_SCHEMA: &str = opensks_contracts::PROVIDER_DESCRIPTOR_SCHEMA;
pub const PROVIDER_DB_RELATIVE_PATH: &str = ".opensks/runtime/providers.sqlite3";
const PROVIDER_DB_MIGRATION_VERSION: i32 = 1;
pub const CODEX_LB_PROVIDER_ID: &str = "provider-codex-lb-env";
pub const CODEX_LB_API_KEY_ENV_VAR: &str = "CODEX_LB_API_KEY";
pub const CODEX_LB_BASE_URL_ENV_VAR: &str = "CODEX_LB_BASE_URL";
const DEFAULT_CODEX_LB_BASE_URL: &str = "https://codex.hyper-lab.xyz/backend-api/codex";
const CODEX_LB_CATALOG_REVISION: &str = "codex-lb-env-seed-v1";
const PROVIDER_PROBE_MAX_BODY_BYTES: u64 = 4 * 1024 * 1024; // 4 MiB cap on /models response body
const MAX_PROVIDER_MODEL_CATALOG_ENTRIES: usize = 5_000;
const PROBE_RECEIPT_RETENTION_PER_PROVIDER: i64 = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHttpStatus {
    pub http_code: String,
    pub diagnostic: String,
}

pub fn native_http_get_status(
    endpoint: &str,
    bearer_token: Option<&str>,
    timeout: Duration,
) -> std::result::Result<ProviderHttpStatus, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| error.to_string())?;
    let mut request = client
        .get(endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            reqwest::header::USER_AGENT,
            concat!(
                "opensks-provider/",
                env!("CARGO_PKG_VERSION"),
                " provider-check"
            ),
        );
    if let Some(token) = bearer_token {
        request = request.bearer_auth(token);
    }
    let response = request.send().map_err(|error| error.to_string())?;
    let status = response.status();
    Ok(ProviderHttpStatus {
        http_code: status.as_u16().to_string(),
        diagnostic: if status.is_success() {
            String::new()
        } else {
            status
                .canonical_reason()
                .unwrap_or("http status not successful")
                .to_string()
        },
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderProbeOutcome {
    pub receipt: ProviderProbeReceipt,
    pub models: Vec<ModelCatalogEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct OpenAiModelsResponse {
    #[serde(default)]
    data: Vec<OpenAiModelRecord>,
}

#[derive(Debug, serde::Deserialize)]
struct OpenAiModelRecord {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    architecture: Option<OpenAiModelArchitecture>,
    #[serde(default)]
    supported_parameters: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize)]
struct OpenAiModelArchitecture {
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
}

pub fn probe_openai_compatible_provider(
    connection: &ProviderConnection,
    bearer_token: &str,
    now_ms: u64,
    timeout: Duration,
) -> Result<ProviderProbeOutcome> {
    validate_provider_connection(connection)?;
    if bearer_token.trim().is_empty() {
        return Err(ProviderError::InvalidProviderConfig(
            "provider credential resolved empty".to_string(),
        ));
    }
    let endpoint = provider_models_endpoint(connection)?;
    let host = redacted_endpoint_host(&endpoint);
    let started = Instant::now();
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| ProviderError::InvalidProviderConfig(error.to_string()))?;
    let response = client
        .get(&endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            reqwest::header::USER_AGENT,
            concat!(
                "opensks-provider/",
                env!("CARGO_PKG_VERSION"),
                " provider-catalog-probe"
            ),
        )
        .bearer_auth(bearer_token)
        .send();

    let elapsed = started.elapsed();
    match response {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                let mut limited =
                    std::io::Read::take(response, PROVIDER_PROBE_MAX_BODY_BYTES + 1);
                let mut buf = Vec::new();
                let read_result = std::io::Read::read_to_end(&mut limited, &mut buf);
                let body = match read_result {
                    Ok(_) if buf.len() as u64 > PROVIDER_PROBE_MAX_BODY_BYTES => None,
                    Ok(_) => Some(String::from_utf8_lossy(&buf).into_owned()),
                    Err(_) => None,
                };
                match body.as_deref().map(|body| parse_openai_compatible_models(connection, body))
                {
                    Some(Ok(models)) => Ok(ProviderProbeOutcome {
                        receipt: ProviderProbeReceipt {
                            schema: PROVIDER_PROBE_RECEIPT_SCHEMA.to_string(),
                            provider_id: connection.id.clone(),
                            endpoint_host_redacted: host,
                            http_category: ProviderProbeHttpCategory::Success,
                            latency_bucket: latency_bucket(elapsed),
                            auth_accepted: true,
                            model_list_available: true,
                            catalog_count: Some(models.len() as u32),
                            occurred_at_ms: now_ms,
                            reason_code: if models.is_empty() {
                                "model_catalog_empty".to_string()
                            } else {
                                "probe_ok".to_string()
                            },
                            diagnostic_ref: None,
                        },
                        models,
                    }),
                    Some(Err(_)) => Ok(ProviderProbeOutcome {
                        receipt: ProviderProbeReceipt {
                            schema: PROVIDER_PROBE_RECEIPT_SCHEMA.to_string(),
                            provider_id: connection.id.clone(),
                            endpoint_host_redacted: host,
                            http_category: ProviderProbeHttpCategory::Success,
                            latency_bucket: latency_bucket(elapsed),
                            auth_accepted: true,
                            model_list_available: false,
                            catalog_count: Some(0),
                            occurred_at_ms: now_ms,
                            reason_code: "model_catalog_parse_failed".to_string(),
                            diagnostic_ref: Some("invalid_models_json".to_string()),
                        },
                        models: Vec::new(),
                    }),
                    None => Ok(ProviderProbeOutcome {
                        receipt: ProviderProbeReceipt {
                            schema: PROVIDER_PROBE_RECEIPT_SCHEMA.to_string(),
                            provider_id: connection.id.clone(),
                            endpoint_host_redacted: host,
                            http_category: ProviderProbeHttpCategory::Success,
                            latency_bucket: latency_bucket(elapsed),
                            auth_accepted: true,
                            model_list_available: false,
                            catalog_count: Some(0),
                            occurred_at_ms: now_ms,
                            reason_code: "model_catalog_response_too_large".to_string(),
                            diagnostic_ref: Some("response_too_large".to_string()),
                        },
                        models: Vec::new(),
                    }),
                }
            } else {
                Ok(ProviderProbeOutcome {
                    receipt: ProviderProbeReceipt {
                        schema: PROVIDER_PROBE_RECEIPT_SCHEMA.to_string(),
                        provider_id: connection.id.clone(),
                        endpoint_host_redacted: host,
                        http_category: http_category(status.as_u16()),
                        latency_bucket: latency_bucket(elapsed),
                        auth_accepted: !matches!(status.as_u16(), 401 | 403),
                        model_list_available: false,
                        catalog_count: None,
                        occurred_at_ms: now_ms,
                        reason_code: http_reason_code(status.as_u16()).to_string(),
                        diagnostic_ref: status
                            .canonical_reason()
                            .map(|reason| format!("http_{}", reason.to_ascii_lowercase())),
                    },
                    models: Vec::new(),
                })
            }
        }
        Err(_) => Ok(ProviderProbeOutcome {
            receipt: ProviderProbeReceipt {
                schema: PROVIDER_PROBE_RECEIPT_SCHEMA.to_string(),
                provider_id: connection.id.clone(),
                endpoint_host_redacted: host,
                http_category: ProviderProbeHttpCategory::NetworkError,
                latency_bucket: latency_bucket(elapsed),
                auth_accepted: false,
                model_list_available: false,
                catalog_count: None,
                occurred_at_ms: now_ms,
                reason_code: "provider_network_error".to_string(),
                diagnostic_ref: Some("transport_error".to_string()),
            },
            models: Vec::new(),
        }),
    }
}

pub fn health_snapshot_from_probe_receipt(
    receipt: &ProviderProbeReceipt,
) -> ProviderHealthSnapshot {
    let (state, reason_code) = match receipt.http_category {
        ProviderProbeHttpCategory::Success
            if receipt.auth_accepted
                && receipt.model_list_available
                && receipt.catalog_count.unwrap_or(0) > 0 =>
        {
            (HealthState::Healthy, "probe_ok")
        }
        ProviderProbeHttpCategory::Success if receipt.model_list_available => {
            (HealthState::Degraded, "model_catalog_empty")
        }
        ProviderProbeHttpCategory::Success => (HealthState::Degraded, "model_catalog_unavailable"),
        ProviderProbeHttpCategory::AuthRejected => {
            (HealthState::Unavailable, "credential_rejected")
        }
        ProviderProbeHttpCategory::RateLimited => (HealthState::Degraded, "provider_rate_limited"),
        ProviderProbeHttpCategory::ClientError => (HealthState::Degraded, "provider_client_error"),
        ProviderProbeHttpCategory::ServerError => (HealthState::Degraded, "provider_server_error"),
        ProviderProbeHttpCategory::NetworkError => {
            (HealthState::Degraded, "provider_network_error")
        }
        ProviderProbeHttpCategory::NotSent => (HealthState::Unknown, "probe_not_sent"),
    };
    ProviderHealthSnapshot {
        state,
        circuit_open: false,
        checked_at_ms: Some(receipt.occurred_at_ms),
        reason_code: reason_code.to_string(),
        diagnostic_ref: receipt.diagnostic_ref.clone(),
    }
}

fn provider_models_endpoint(connection: &ProviderConnection) -> Result<String> {
    let base = connection.endpoint.base_url.trim();
    let parsed = reqwest::Url::parse(base).map_err(|error| {
        ProviderError::InvalidProviderConfig(format!("invalid provider endpoint: {error}"))
    })?;
    if parsed.scheme() == "http"
        && !connection.endpoint.allow_insecure_http
        && !is_loopback_host(parsed.host_str())
    {
        return Err(ProviderError::InvalidProviderConfig(
            "insecure HTTP provider endpoint must be local or explicitly allowed".to_string(),
        ));
    }
    Ok(format!("{}/models", base.trim_end_matches('/')))
}

fn is_loopback_host(host: Option<&str>) -> bool {
    matches!(host, Some("localhost") | Some("127.0.0.1") | Some("::1"))
}

fn redacted_endpoint_host(endpoint: &str) -> String {
    reqwest::Url::parse(endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| "unknown-host".to_string())
}

pub fn parse_openai_compatible_models(
    connection: &ProviderConnection,
    body: &str,
) -> Result<Vec<ModelCatalogEntry>> {
    let parsed: OpenAiModelsResponse = serde_json::from_str(body)?;
    if parsed.data.len() > MAX_PROVIDER_MODEL_CATALOG_ENTRIES {
        return Err(ProviderError::InvalidProviderConfig(format!(
            "provider model catalog too large: {} entries exceeds limit of {}",
            parsed.data.len(),
            MAX_PROVIDER_MODEL_CATALOG_ENTRIES
        )));
    }
    let catalog_revision = stable_hash(body.as_bytes());
    let mut models = Vec::new();
    for model in parsed.data {
        let remote_model_id = model.id.trim();
        if remote_model_id.is_empty() {
            continue;
        }
        let capabilities = model_capabilities(&model);
        let mut role_scores = BTreeMap::new();
        role_scores.insert(
            ModelRole::General,
            RoleScore {
                score: 0.62,
                evidence_refs: vec!["provider_models_endpoint".to_string()],
            },
        );
        if capabilities.code {
            role_scores.insert(
                ModelRole::Code,
                RoleScore {
                    score: 0.68,
                    evidence_refs: vec!["provider_models_endpoint".to_string()],
                },
            );
        }
        if capabilities.image_output {
            role_scores.insert(
                ModelRole::Image,
                RoleScore {
                    score: 0.68,
                    evidence_refs: vec!["provider_models_endpoint".to_string()],
                },
            );
        }
        if capabilities.vision_input {
            role_scores.insert(
                ModelRole::Vision,
                RoleScore {
                    score: 0.66,
                    evidence_refs: vec!["provider_models_endpoint".to_string()],
                },
            );
        }
        models.push(ModelCatalogEntry {
            schema: MODEL_CATALOG_ENTRY_SCHEMA.to_string(),
            id: format!("{}/{}", connection.id, remote_model_id),
            provider_id: connection.id.clone(),
            remote_model_id: remote_model_id.to_string(),
            display_name: model
                .name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(remote_model_id)
                .to_string(),
            enabled: true,
            capabilities,
            limits: ModelLimits {
                max_input_tokens: model.context_length,
                max_output_tokens: None,
                requests_per_minute: connection.concurrency.requests_per_minute,
                tokens_per_minute: connection.concurrency.tokens_per_minute,
                max_concurrency: Some(connection.concurrency.max_concurrent_requests),
            },
            pricing: None,
            health: HealthState::Healthy,
            role_scores,
            catalog_revision: catalog_revision.clone(),
        });
    }
    Ok(models)
}

fn model_capabilities(model: &OpenAiModelRecord) -> ModelCapabilities {
    let id = model.id.to_ascii_lowercase();
    let input_modalities = model
        .architecture
        .as_ref()
        .map(|architecture| architecture.input_modalities.as_slice())
        .unwrap_or(&[]);
    let output_modalities = model
        .architecture
        .as_ref()
        .map(|architecture| architecture.output_modalities.as_slice())
        .unwrap_or(&[]);
    let supports = model.supported_parameters.as_deref().unwrap_or(&[]);
    let has_input_image = input_modalities
        .iter()
        .any(|value| value.eq_ignore_ascii_case("image"));
    let has_output_image = output_modalities
        .iter()
        .any(|value| value.eq_ignore_ascii_case("image"));
    let image_output = has_output_image || id.contains("image") || id.contains("dall-e");
    let vision_input = has_input_image || id.contains("vision") || id.contains("gpt-4o");
    let tool_use = supports
        .iter()
        .any(|value| matches!(value.as_str(), "tools" | "tool_choice" | "function_calling"));
    let structured_output = supports
        .iter()
        .any(|value| matches!(value.as_str(), "response_format" | "json_schema"))
        || !image_output;
    ModelCapabilities {
        text: !image_output
            || output_modalities
                .iter()
                .any(|value| value.eq_ignore_ascii_case("text")),
        code: !image_output,
        vision_input: vision_input || image_output,
        image_output,
        image_edit: id.contains("edit"),
        tool_use,
        structured_output,
        long_context: model.context_length.unwrap_or(0) >= 32_000
            || id.contains("32k")
            || id.contains("128k")
            || id.contains("1m"),
        streaming: !image_output,
    }
}

fn http_category(status: u16) -> ProviderProbeHttpCategory {
    match status {
        401 | 403 => ProviderProbeHttpCategory::AuthRejected,
        429 => ProviderProbeHttpCategory::RateLimited,
        400..=499 => ProviderProbeHttpCategory::ClientError,
        500..=599 => ProviderProbeHttpCategory::ServerError,
        _ => ProviderProbeHttpCategory::ClientError,
    }
}

fn http_reason_code(status: u16) -> &'static str {
    match status {
        401 | 403 => "provider_auth_rejected",
        429 => "provider_rate_limited",
        400..=499 => "provider_client_error",
        500..=599 => "provider_server_error",
        _ => "provider_http_error",
    }
}

fn latency_bucket(duration: Duration) -> LatencyBucket {
    if duration < Duration::from_millis(250) {
        LatencyBucket::Under250Ms
    } else if duration < Duration::from_secs(1) {
        LatencyBucket::Under1S
    } else if duration < Duration::from_secs(5) {
        LatencyBucket::Under5S
    } else {
        LatencyBucket::Over5S
    }
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("model `{0}` is not registered")]
    UnknownModel(String),
    #[error("provider policy denied dispatch: {0}")]
    PolicyDenied(String),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("provider connection not found: {0}")]
    NotFound(String),
    #[error("provider revision conflict for `{provider_id}`: expected {expected}, actual {actual}")]
    RevisionConflict {
        provider_id: String,
        expected: u64,
        actual: u64,
    },
    #[error("invalid provider config: {0}")]
    InvalidProviderConfig(String),
}

type Result<T> = std::result::Result<T, ProviderError>;

pub fn sync_codex_lb_env_provider_to_registry(workspace: &Path, now_ms: u64) -> Result<()> {
    let credential_present = env::var(CODEX_LB_API_KEY_ENV_VAR)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_some();
    let endpoint = env::var(CODEX_LB_BASE_URL_ENV_VAR)
        .ok()
        .filter(|value| !value.trim().is_empty());
    sync_codex_lb_external_broker_provider_to_registry(
        workspace,
        now_ms,
        credential_present,
        endpoint,
    )
}

pub fn sync_codex_lb_external_broker_provider_to_registry(
    workspace: &Path,
    now_ms: u64,
    credential_present: bool,
    endpoint: Option<String>,
) -> Result<()> {
    if !credential_present {
        return Ok(());
    }

    let repo = ProviderRepository::open_workspace(workspace)?;
    if repo
        .list_connections()?
        .iter()
        .any(|connection| connection.kind == opensks_contracts::ProviderKind::CodexLb)
    {
        return Ok(());
    }

    let endpoint = endpoint.unwrap_or_else(|| DEFAULT_CODEX_LB_BASE_URL.to_string());
    let connection = ProviderConnection {
        schema: PROVIDER_CONNECTION_SCHEMA.to_string(),
        id: CODEX_LB_PROVIDER_ID.to_string(),
        kind: opensks_contracts::ProviderKind::CodexLb,
        display_name: "codex-lb".to_string(),
        enabled: true,
        endpoint: opensks_contracts::ProviderEndpoint {
            base_url: endpoint,
            allow_insecure_http: false,
        },
        auth: opensks_contracts::SecretRef {
            schema: SECRET_REF_SCHEMA.to_string(),
            store: opensks_contracts::SecretStoreKind::ExternalBroker,
            service: "env".to_string(),
            account: CODEX_LB_API_KEY_ENV_VAR.to_string(),
            version: 1,
        },
        organization_ref: None,
        project_ref: None,
        health: ProviderHealthSnapshot::unknown(),
        concurrency: opensks_contracts::ProviderConcurrencyPolicy {
            max_concurrent_requests: 16,
            requests_per_minute: None,
            tokens_per_minute: None,
        },
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
        revision: 1,
    };
    repo.upsert_connection(&connection, None, now_ms)?;
    repo.sync_models(
        CODEX_LB_PROVIDER_ID,
        &codex_lb_seed_model_records(CODEX_LB_PROVIDER_ID),
        now_ms,
    )?;
    Ok(())
}

fn codex_lb_seed_model_records(provider_id: &str) -> Vec<ModelCatalogEntry> {
    vec![
        codex_lb_seed_code_model(provider_id, "auto-code", "Default code model", 128_000),
        codex_lb_seed_code_model(provider_id, "gpt-5.5", "GPT-5.5", 400_000),
        codex_lb_seed_code_model(provider_id, "gpt-5.4-mini", "GPT-5.4 mini", 400_000),
        codex_lb_seed_code_model(provider_id, "gpt-5.4-nano", "GPT-5.4 nano", 400_000),
        ModelCatalogEntry {
            schema: MODEL_CATALOG_ENTRY_SCHEMA.to_string(),
            id: format!("{provider_id}/auto-image"),
            provider_id: provider_id.to_string(),
            remote_model_id: "auto-image".to_string(),
            display_name: "Auto image model".to_string(),
            enabled: true,
            capabilities: ModelCapabilities::image(),
            limits: ModelLimits::default(),
            pricing: None,
            health: HealthState::Unknown,
            role_scores: codex_lb_role_scores(&[ModelRole::Vision, ModelRole::Image]),
            catalog_revision: CODEX_LB_CATALOG_REVISION.to_string(),
        },
    ]
}

fn codex_lb_seed_code_model(
    provider_id: &str,
    remote_model_id: &str,
    display_name: &str,
    context_window: u64,
) -> ModelCatalogEntry {
    ModelCatalogEntry {
        schema: MODEL_CATALOG_ENTRY_SCHEMA.to_string(),
        id: format!("{provider_id}/{remote_model_id}"),
        provider_id: provider_id.to_string(),
        remote_model_id: remote_model_id.to_string(),
        display_name: display_name.to_string(),
        enabled: true,
        capabilities: ModelCapabilities {
            text: true,
            code: true,
            vision_input: false,
            image_output: false,
            image_edit: false,
            tool_use: true,
            structured_output: true,
            long_context: true,
            streaming: true,
        },
        limits: ModelLimits {
            max_input_tokens: Some(context_window),
            max_output_tokens: Some(16_000),
            requests_per_minute: None,
            tokens_per_minute: None,
            max_concurrency: Some(16),
        },
        pricing: None,
        health: HealthState::Unknown,
        role_scores: codex_lb_role_scores(&[
            ModelRole::General,
            ModelRole::Planning,
            ModelRole::Code,
            ModelRole::Verification,
            ModelRole::Arbiter,
        ]),
        catalog_revision: CODEX_LB_CATALOG_REVISION.to_string(),
    }
}

fn codex_lb_role_scores(roles: &[ModelRole]) -> BTreeMap<ModelRole, RoleScore> {
    roles
        .iter()
        .cloned()
        .map(|role| {
            (
                role,
                RoleScore {
                    score: 0.80,
                    evidence_refs: vec!["codex-lb-env-seed".to_string()],
                },
            )
        })
        .collect()
}

#[derive(Debug)]
pub struct ProviderRepository {
    conn: Connection,
}

impl ProviderRepository {
    pub fn open_workspace(workspace: &Path) -> Result<Self> {
        let path = workspace.join(PROVIDER_DB_RELATIVE_PATH);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Self::open_path(&path)
    }

    pub fn open_path(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::from_connection(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        let repo = Self { conn };
        repo.migrate()?;
        Ok(repo)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.pragma_update(None, "journal_mode", "WAL")?;
        self.conn
            .busy_timeout(std::time::Duration::from_millis(5000))?;
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS provider_connections (
                id TEXT PRIMARY KEY NOT NULL,
                kind TEXT NOT NULL,
                display_name TEXT NOT NULL,
                enabled INTEGER NOT NULL,
                revision INTEGER NOT NULL,
                secret_store TEXT NOT NULL,
                secret_service TEXT NOT NULL,
                secret_account TEXT NOT NULL,
                secret_version INTEGER NOT NULL,
                connection_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS model_catalog (
                id TEXT PRIMARY KEY NOT NULL,
                provider_id TEXT NOT NULL,
                enabled INTEGER NOT NULL,
                catalog_revision TEXT NOT NULL,
                entry_json TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                FOREIGN KEY(provider_id) REFERENCES provider_connections(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_model_catalog_provider
                ON model_catalog(provider_id);
            CREATE TABLE IF NOT EXISTS provider_probe_receipts (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                provider_id TEXT NOT NULL,
                occurred_at_ms INTEGER NOT NULL,
                receipt_json TEXT NOT NULL,
                FOREIGN KEY(provider_id) REFERENCES provider_connections(id) ON DELETE CASCADE
            );
            "#,
        )?;
        self.conn
            .pragma_update(None, "user_version", PROVIDER_DB_MIGRATION_VERSION)?;
        Ok(())
    }

    pub fn user_version(&self) -> Result<i32> {
        Ok(self
            .conn
            .query_row("SELECT user_version FROM pragma_user_version", [], |row| {
                row.get(0)
            })?)
    }

    pub fn upsert_connection(
        &self,
        connection: &ProviderConnection,
        expected_revision: Option<u64>,
        now_ms: u64,
    ) -> Result<ProviderMutationReceipt> {
        validate_provider_connection(connection)?;
        let json = serde_json::to_string(connection)?;
        let tx = self.conn.unchecked_transaction()?;
        if let Some(expected) = expected_revision {
            let actual: Option<u64> = tx
                .query_row(
                    "SELECT revision FROM provider_connections WHERE id = ?1",
                    params![connection.id],
                    |row| row.get(0),
                )
                .optional()?;
            if actual != Some(expected) {
                return Err(ProviderError::RevisionConflict {
                    provider_id: connection.id.clone(),
                    expected,
                    actual: actual.unwrap_or(0),
                });
            }
        }
        tx.execute(
            r#"
            INSERT INTO provider_connections (
                id, kind, display_name, enabled, revision,
                secret_store, secret_service, secret_account, secret_version,
                connection_json, created_at_ms, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
                kind = excluded.kind,
                display_name = excluded.display_name,
                enabled = excluded.enabled,
                revision = excluded.revision,
                secret_store = excluded.secret_store,
                secret_service = excluded.secret_service,
                secret_account = excluded.secret_account,
                secret_version = excluded.secret_version,
                connection_json = excluded.connection_json,
                updated_at_ms = excluded.updated_at_ms
            "#,
            params![
                connection.id,
                format!("{:?}", connection.kind),
                connection.display_name,
                connection.enabled,
                connection.revision,
                format!("{:?}", connection.auth.store),
                connection.auth.service,
                connection.auth.account,
                connection.auth.version,
                json,
                connection.created_at_ms,
                connection.updated_at_ms,
            ],
        )?;
        tx.commit()?;
        Ok(ProviderMutationReceipt {
            schema: PROVIDER_MUTATION_SCHEMA.to_string(),
            provider_id: connection.id.clone(),
            mutation: if expected_revision.is_some() {
                ProviderMutationKind::Updated
            } else {
                ProviderMutationKind::Created
            },
            revision: connection.revision,
            secret_ref: Some(connection.auth.clone()),
            secret_value_exposed: false,
            occurred_at_ms: now_ms,
            reason_code: "provider_connection_saved_secretless".to_string(),
        })
    }

    pub fn get_connection(&self, provider_id: &str) -> Result<ProviderConnection> {
        self.conn
            .query_row(
                "SELECT connection_json FROM provider_connections WHERE id = ?1",
                params![provider_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|json| serde_json::from_str(&json).map_err(ProviderError::from))
            .transpose()?
            .ok_or_else(|| ProviderError::NotFound(provider_id.to_string()))
    }

    pub fn list_connections(&self) -> Result<Vec<ProviderConnection>> {
        let mut stmt = self.conn.prepare(
            "SELECT connection_json FROM provider_connections ORDER BY display_name, id",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut connections = Vec::new();
        for row in rows {
            let json = row?;
            connections.push(serde_json::from_str(&json)?);
        }
        Ok(connections)
    }

    pub fn set_provider_enabled(
        &self,
        provider_id: &str,
        enabled: bool,
        expected_revision: u64,
        now_ms: u64,
    ) -> Result<ProviderMutationReceipt> {
        let mut connection = self.get_connection(provider_id)?;
        if connection.revision != expected_revision {
            return Err(ProviderError::RevisionConflict {
                provider_id: provider_id.to_string(),
                expected: expected_revision,
                actual: connection.revision,
            });
        }
        connection.enabled = enabled;
        connection.revision = connection.revision.saturating_add(1);
        connection.updated_at_ms = now_ms;
        let json = serde_json::to_string(&connection)?;
        let rows = self.conn.execute(
            "UPDATE provider_connections SET enabled = ?1, revision = ?2, connection_json = ?3, updated_at_ms = ?4 WHERE id = ?5 AND revision = ?6",
            params![
                connection.enabled,
                connection.revision,
                json,
                connection.updated_at_ms,
                provider_id,
                expected_revision,
            ],
        )?;
        if rows == 0 {
            let actual = self.connection_revision(provider_id)?.unwrap_or(0);
            return Err(ProviderError::RevisionConflict {
                provider_id: provider_id.to_string(),
                expected: expected_revision,
                actual,
            });
        }
        Ok(ProviderMutationReceipt {
            schema: PROVIDER_MUTATION_SCHEMA.to_string(),
            provider_id: provider_id.to_string(),
            mutation: if enabled {
                ProviderMutationKind::Enabled
            } else {
                ProviderMutationKind::Disabled
            },
            revision: connection.revision,
            secret_ref: Some(connection.auth.clone()),
            secret_value_exposed: false,
            occurred_at_ms: now_ms,
            reason_code: if enabled {
                "provider_enabled".to_string()
            } else {
                "provider_disabled".to_string()
            },
        })
    }

    pub fn delete_connection(
        &self,
        provider_id: &str,
        expected_revision: u64,
        now_ms: u64,
    ) -> Result<ProviderMutationReceipt> {
        let connection = self.get_connection(provider_id)?;
        if connection.revision != expected_revision {
            return Err(ProviderError::RevisionConflict {
                provider_id: provider_id.to_string(),
                expected: expected_revision,
                actual: connection.revision,
            });
        }
        self.conn.execute(
            "DELETE FROM model_catalog WHERE provider_id = ?1",
            params![provider_id],
        )?;
        self.conn.execute(
            "DELETE FROM provider_probe_receipts WHERE provider_id = ?1",
            params![provider_id],
        )?;
        self.conn.execute(
            "DELETE FROM provider_connections WHERE id = ?1",
            params![provider_id],
        )?;
        Ok(ProviderMutationReceipt {
            schema: PROVIDER_MUTATION_SCHEMA.to_string(),
            provider_id: provider_id.to_string(),
            mutation: ProviderMutationKind::Deleted,
            revision: expected_revision,
            secret_ref: Some(connection.auth),
            secret_value_exposed: false,
            occurred_at_ms: now_ms,
            reason_code: "provider_connection_deleted_secret_ref_only".to_string(),
        })
    }

    pub fn sync_models(
        &self,
        provider_id: &str,
        models: &[ModelCatalogEntry],
        now_ms: u64,
    ) -> Result<ProviderMutationReceipt> {
        let connection = self.get_connection(provider_id)?;
        let tx = self.conn.unchecked_transaction()?;
        for model in models {
            if model.provider_id != provider_id {
                return Err(ProviderError::InvalidProviderConfig(format!(
                    "model `{}` belongs to `{}` not `{provider_id}`",
                    model.id, model.provider_id
                )));
            }
            let mut stored_model = model.clone();
            if let Ok(existing) = self.get_model(&model.id) {
                stored_model.enabled = existing.enabled;
            }
            let json = serde_json::to_string(&stored_model)?;
            tx.execute(
                r#"
                INSERT INTO model_catalog (
                    id, provider_id, enabled, catalog_revision, entry_json, updated_at_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(id) DO UPDATE SET
                    provider_id = excluded.provider_id,
                    enabled = excluded.enabled,
                    catalog_revision = excluded.catalog_revision,
                    entry_json = excluded.entry_json,
                    updated_at_ms = excluded.updated_at_ms
                "#,
                params![
                    stored_model.id,
                    stored_model.provider_id,
                    stored_model.enabled,
                    stored_model.catalog_revision,
                    json,
                    now_ms,
                ],
            )?;
        }
        tx.commit()?;
        Ok(ProviderMutationReceipt {
            schema: PROVIDER_MUTATION_SCHEMA.to_string(),
            provider_id: provider_id.to_string(),
            mutation: ProviderMutationKind::ModelsSynced,
            revision: connection.revision,
            secret_ref: None,
            secret_value_exposed: false,
            occurred_at_ms: now_ms,
            reason_code: "model_catalog_synced".to_string(),
        })
    }

    pub fn set_provider_health(
        &self,
        provider_id: &str,
        health: ProviderHealthSnapshot,
        now_ms: u64,
    ) -> Result<ProviderConnection> {
        let mut connection = self.get_connection(provider_id)?;
        let expected_revision = connection.revision;
        connection.health = health;
        connection.revision = connection.revision.saturating_add(1);
        connection.updated_at_ms = now_ms;
        let _ = self.upsert_connection(&connection, Some(expected_revision), now_ms)?;
        Ok(connection)
    }

    pub fn list_models(&self, provider_id: &str) -> Result<Vec<ModelCatalogEntry>> {
        let mut stmt = self
            .conn
            .prepare("SELECT entry_json FROM model_catalog WHERE provider_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map(params![provider_id], |row| row.get::<_, String>(0))?;
        let mut models = Vec::new();
        for row in rows {
            models.push(serde_json::from_str(&row?)?);
        }
        Ok(models)
    }

    pub fn set_model_enabled(
        &self,
        model_id: &str,
        enabled: bool,
        now_ms: u64,
    ) -> Result<ModelCatalogEntry> {
        let mut entry = self.get_model(model_id)?;
        entry.enabled = enabled;
        let json = serde_json::to_string(&entry)?;
        self.conn.execute(
            "UPDATE model_catalog SET enabled = ?1, entry_json = ?2, updated_at_ms = ?3 WHERE id = ?4",
            params![enabled, json, now_ms, model_id],
        )?;
        Ok(entry)
    }

    pub fn get_model(&self, model_id: &str) -> Result<ModelCatalogEntry> {
        self.conn
            .query_row(
                "SELECT entry_json FROM model_catalog WHERE id = ?1",
                params![model_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|json| serde_json::from_str(&json).map_err(ProviderError::from))
            .transpose()?
            .ok_or_else(|| ProviderError::UnknownModel(model_id.to_string()))
    }

    pub fn record_probe_receipt(&self, receipt: &ProviderProbeReceipt) -> Result<()> {
        let _ = self.get_connection(&receipt.provider_id)?;
        let json = serde_json::to_string(receipt)?;
        self.conn.execute(
            "INSERT INTO provider_probe_receipts (provider_id, occurred_at_ms, receipt_json) VALUES (?1, ?2, ?3)",
            params![receipt.provider_id, receipt.occurred_at_ms, json],
        )?;
        self.conn.execute(
            r#"
            DELETE FROM provider_probe_receipts
            WHERE provider_id = ?1
              AND sequence NOT IN (
                  SELECT sequence FROM provider_probe_receipts
                  WHERE provider_id = ?1
                  ORDER BY sequence DESC
                  LIMIT ?2
              )
            "#,
            params![receipt.provider_id, PROBE_RECEIPT_RETENTION_PER_PROVIDER],
        )?;
        Ok(())
    }

    pub fn latest_probe_receipt(&self, provider_id: &str) -> Result<Option<ProviderProbeReceipt>> {
        self.conn
            .query_row(
                r#"
                SELECT receipt_json FROM provider_probe_receipts
                WHERE provider_id = ?1
                ORDER BY occurred_at_ms DESC, sequence DESC
                LIMIT 1
                "#,
                params![provider_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|json| serde_json::from_str(&json).map_err(ProviderError::from))
            .transpose()
    }

    fn connection_revision(&self, provider_id: &str) -> Result<Option<u64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT revision FROM provider_connections WHERE id = ?1",
                params![provider_id],
                |row| row.get::<_, u64>(0),
            )
            .optional()?)
    }

    #[cfg(test)]
    fn stored_connection_json(&self, provider_id: &str) -> Result<String> {
        Ok(self.conn.query_row(
            "SELECT connection_json FROM provider_connections WHERE id = ?1",
            params![provider_id],
            |row| row.get(0),
        )?)
    }
}

fn validate_provider_connection(connection: &ProviderConnection) -> Result<()> {
    if connection.schema != PROVIDER_CONNECTION_SCHEMA {
        return Err(ProviderError::InvalidProviderConfig(format!(
            "unexpected provider schema `{}`",
            connection.schema
        )));
    }
    if connection.auth.schema != SECRET_REF_SCHEMA {
        return Err(ProviderError::InvalidProviderConfig(format!(
            "unexpected secret ref schema `{}`",
            connection.auth.schema
        )));
    }
    if connection.id.trim().is_empty() {
        return Err(ProviderError::InvalidProviderConfig(
            "provider id is required".to_string(),
        ));
    }
    if connection.auth.service.trim().is_empty() || connection.auth.account.trim().is_empty() {
        return Err(ProviderError::InvalidProviderConfig(
            "secret ref service/account are required".to_string(),
        ));
    }
    let endpoint = connection.endpoint.base_url.trim();
    if endpoint.is_empty() {
        return Err(ProviderError::InvalidProviderConfig(
            "provider endpoint is required".to_string(),
        ));
    }
    let parsed = reqwest::Url::parse(endpoint).map_err(|error| {
        ProviderError::InvalidProviderConfig(format!("invalid provider endpoint: {error}"))
    })?;
    if parsed.scheme() == "http"
        && !connection.endpoint.allow_insecure_http
        && !is_loopback_host(parsed.host_str())
    {
        return Err(ProviderError::InvalidProviderConfig(
            "insecure HTTP provider endpoint must be local or explicitly allowed".to_string(),
        ));
    }
    if connection.endpoint.base_url.contains('@') {
        return Err(ProviderError::InvalidProviderConfig(
            "endpoint must not contain userinfo credentials".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct RoutingRequest {
    pub id: String,
    pub role: ModelRole,
    pub required: CapabilityRequirements,
    pub explicit_model_id: Option<String>,
    pub budget_allowed: bool,
}

impl RoutingRequest {
    pub fn for_code(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role: ModelRole::Code,
            required: CapabilityRequirements::code(),
            explicit_model_id: None,
            budget_allowed: true,
        }
    }

    pub fn for_image(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role: ModelRole::Image,
            required: CapabilityRequirements::image_output(),
            explicit_model_id: None,
            budget_allowed: true,
        }
    }

    pub fn for_vision(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role: ModelRole::Vision,
            required: CapabilityRequirements {
                vision_input: true,
                ..CapabilityRequirements::default()
            },
            explicit_model_id: None,
            budget_allowed: true,
        }
    }

    pub fn for_role(id: impl Into<String>, role: ModelRole) -> Self {
        let required = capability_requirements_for_role(&role);
        Self {
            id: id.into(),
            role,
            required,
            explicit_model_id: None,
            budget_allowed: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleRoutingRequest {
    pub id: String,
    pub roles: Vec<ModelRole>,
    pub prefer_distinct_models: bool,
    pub explicit_model_ids: BTreeMap<ModelRole, String>,
    pub budget_allowed: bool,
}

impl RoleRoutingRequest {
    pub fn hyperparallel_default(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            roles: vec![
                ModelRole::Planning,
                ModelRole::Code,
                ModelRole::Verification,
                ModelRole::Arbiter,
            ],
            prefer_distinct_models: true,
            explicit_model_ids: BTreeMap::new(),
            budget_allowed: true,
        }
    }

    pub fn with_roles(id: impl Into<String>, roles: Vec<ModelRole>) -> Self {
        Self {
            id: id.into(),
            roles,
            prefer_distinct_models: true,
            explicit_model_ids: BTreeMap::new(),
            budget_allowed: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleRouteAssignment {
    pub role: ModelRole,
    pub decision: RoutingDecision,
    pub reused_model: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleRoutingPlan {
    pub id: String,
    pub status: RoutingStatus,
    pub reason_code: String,
    pub model_snapshot_hash: String,
    pub assignments: Vec<RoleRouteAssignment>,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ModelRegistry {
    models: Vec<ModelProfile>,
    policy: PermissionPolicy,
}

impl ModelRegistry {
    pub fn new(models: Vec<ModelProfile>, policy: PermissionPolicy) -> Self {
        Self { models, policy }
    }

    pub fn models(&self) -> &[ModelProfile] {
        &self.models
    }

    pub fn snapshot_hash(&self) -> String {
        let mut normalized = self.models.clone();
        normalized.sort_by(|left, right| left.id.cmp(&right.id));
        stable_hash(
            serde_json::to_string(&normalized)
                .unwrap_or_else(|_| "unserializable-model-registry".to_string())
                .as_bytes(),
        )
    }

    pub fn route(&self, request: &RoutingRequest) -> RoutingDecision {
        let snapshot_hash = self.snapshot_hash();
        self.route_with_snapshot(request, &snapshot_hash, &BTreeSet::new())
    }

    pub fn route_roles(&self, request: &RoleRoutingRequest) -> RoleRoutingPlan {
        let snapshot_hash = self.snapshot_hash();
        let mut assigned_model_ids = BTreeSet::new();
        let mut assignments = Vec::new();
        let mut reused_any = false;

        for role in &request.roles {
            let mut routing_request =
                RoutingRequest::for_role(format!("{}:{:?}", request.id, role), role.clone());
            routing_request.explicit_model_id = request.explicit_model_ids.get(role).cloned();
            routing_request.budget_allowed = request.budget_allowed;

            let decision =
                if request.prefer_distinct_models && routing_request.explicit_model_id.is_none() {
                    self.route_with_snapshot(&routing_request, &snapshot_hash, &assigned_model_ids)
                } else {
                    self.route_with_snapshot(&routing_request, &snapshot_hash, &BTreeSet::new())
                };
            let selected_model_id = decision.selected_model_id.clone();
            let reused_model = selected_model_id
                .as_ref()
                .is_some_and(|model_id| assigned_model_ids.contains(model_id));
            if reused_model {
                reused_any = true;
            }
            if let Some(model_id) = selected_model_id {
                assigned_model_ids.insert(model_id);
            }
            assignments.push(RoleRouteAssignment {
                role: role.clone(),
                decision,
                reused_model,
            });
        }

        let status = role_plan_status(&assignments);
        let reason_code = if status.has_resolved_model() {
            if reused_any {
                "role_allocation_resolved_with_model_reuse"
            } else {
                "role_allocation_resolved_distinct_models"
            }
        } else {
            "role_allocation_blocked"
        }
        .to_string();
        let mut evidence_refs = vec![
            "provider:role-routing".to_string(),
            "provider:registry-role-scores".to_string(),
            "provider:health-cost-concurrency-scoring".to_string(),
        ];
        if reused_any {
            evidence_refs.push("provider:single-model-role-reuse".to_string());
        }

        RoleRoutingPlan {
            id: request.id.clone(),
            status,
            reason_code,
            model_snapshot_hash: snapshot_hash,
            assignments,
            evidence_refs,
        }
    }

    fn route_with_snapshot(
        &self,
        request: &RoutingRequest,
        snapshot_hash: &str,
        preferred_exclusions: &BTreeSet<String>,
    ) -> RoutingDecision {
        let permission = self.policy.decide(PermissionScope::ProviderCall);
        if !permission.allowed {
            let reason_code = permission.reason_code;
            return RoutingDecision {
                schema: ROUTING_DECISION_SCHEMA.to_string(),
                id: request.id.clone(),
                status: RoutingStatus::BlockedPolicy,
                selected_model_id: None,
                reason_code: reason_code.clone(),
                eligible_model_ids: Vec::new(),
                rejected_models: Vec::new(),
                model_snapshot_hash: snapshot_hash.to_string(),
                route_receipt: Some(blocked_route_receipt(
                    snapshot_hash,
                    &reason_code,
                    &request.required,
                )),
            };
        }

        if let Some(model_id) = &request.explicit_model_id {
            return self.route_explicit(model_id, request, snapshot_hash);
        }

        let mut eligible = Vec::new();
        let mut rejected = Vec::new();
        for model in &self.models {
            match eligibility_reason(model, request) {
                None => eligible.push(model),
                Some(reason_code) => rejected.push(ModelRejection {
                    model_id: model.id.clone(),
                    reason_code: reason_code.to_string(),
                }),
            }
        }

        if eligible.is_empty() {
            let status = if rejected
                .iter()
                .any(|item| item.reason_code == "model_disabled")
            {
                RoutingStatus::BlockedDisabled
            } else if rejected
                .iter()
                .any(|item| item.reason_code == "provider_health_open")
            {
                RoutingStatus::BlockedProviderHealth
            } else {
                RoutingStatus::BlockedMissingCapability
            };
            return RoutingDecision {
                schema: ROUTING_DECISION_SCHEMA.to_string(),
                id: request.id.clone(),
                status,
                selected_model_id: None,
                reason_code: "blocked_no_enabled_compatible_model".to_string(),
                eligible_model_ids: Vec::new(),
                rejected_models: rejected,
                model_snapshot_hash: snapshot_hash.to_string(),
                route_receipt: Some(blocked_route_receipt(
                    snapshot_hash,
                    "blocked_no_enabled_compatible_model",
                    &request.required,
                )),
            };
        }

        let selection_pool: Vec<&ModelProfile> = if preferred_exclusions.is_empty() {
            eligible.clone()
        } else {
            let distinct: Vec<&ModelProfile> = eligible
                .iter()
                .copied()
                .filter(|model| !preferred_exclusions.contains(&model.id))
                .collect();
            if distinct.is_empty() {
                eligible.clone()
            } else {
                distinct
            }
        };

        let selected = if selection_pool.len() == 1 {
            selection_pool[0]
        } else {
            selection_pool
                .iter()
                .max_by(|left, right| {
                    score_model(left, &request.role)
                        .partial_cmp(&score_model(right, &request.role))
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| right.id.cmp(&left.id))
                })
                .copied()
                .expect("eligible model")
        };

        let used_distinct_pool = !preferred_exclusions.is_empty()
            && eligible.len() != selection_pool.len()
            && !preferred_exclusions.contains(&selected.id);
        let reused_due_to_exhaustion =
            !preferred_exclusions.is_empty() && preferred_exclusions.contains(&selected.id);
        let reason_code = if eligible.len() == 1 {
            "single_enabled_compatible_model"
        } else if reused_due_to_exhaustion {
            "role_reuse_required_highest_weighted_score"
        } else if used_distinct_pool {
            "distinct_role_highest_weighted_score"
        } else {
            "highest_weighted_role_score"
        };

        RoutingDecision {
            schema: ROUTING_DECISION_SCHEMA.to_string(),
            id: request.id.clone(),
            status: RoutingStatus::Resolved,
            selected_model_id: Some(selected.id.clone()),
            reason_code: reason_code.to_string(),
            eligible_model_ids: eligible.iter().map(|model| model.id.clone()).collect(),
            rejected_models: rejected,
            model_snapshot_hash: snapshot_hash.to_string(),
            route_receipt: Some(route_receipt_for_model(
                selected,
                snapshot_hash,
                reason_code,
                &request.required,
                None,
            )),
        }
    }

    fn route_explicit(
        &self,
        model_id: &str,
        request: &RoutingRequest,
        snapshot_hash: &str,
    ) -> RoutingDecision {
        let mut rejected = Vec::new();
        let selected = self.models.iter().find(|model| model.id == model_id);
        if let Some(model) = selected {
            if let Some(reason_code) = eligibility_reason(model, request) {
                rejected.push(ModelRejection {
                    model_id: model.id.clone(),
                    reason_code: reason_code.to_string(),
                });
                let decision_reason = format!("explicit_model_{reason_code}");
                return RoutingDecision {
                    schema: ROUTING_DECISION_SCHEMA.to_string(),
                    id: request.id.clone(),
                    status: match reason_code {
                        "model_disabled" => RoutingStatus::BlockedDisabled,
                        "provider_health_open" => RoutingStatus::BlockedProviderHealth,
                        _ => RoutingStatus::BlockedMissingCapability,
                    },
                    selected_model_id: None,
                    reason_code: decision_reason.clone(),
                    eligible_model_ids: Vec::new(),
                    rejected_models: rejected,
                    model_snapshot_hash: snapshot_hash.to_string(),
                    route_receipt: Some(ModelRouteReceipt {
                        provider_id: Some(model.provider_id.clone()),
                        model_id: Some(model.id.clone()),
                        registry_revision: snapshot_hash.to_string(),
                        reason_code: decision_reason,
                        requested_capabilities: request.required.clone(),
                        effective_limits: model.limits.clone(),
                        fallback_index: None,
                    }),
                };
            }
            return RoutingDecision {
                schema: ROUTING_DECISION_SCHEMA.to_string(),
                id: request.id.clone(),
                status: RoutingStatus::Resolved,
                selected_model_id: Some(model.id.clone()),
                reason_code: "explicit_model_pin".to_string(),
                eligible_model_ids: vec![model.id.clone()],
                rejected_models: rejected,
                model_snapshot_hash: snapshot_hash.to_string(),
                route_receipt: Some(route_receipt_for_model(
                    model,
                    snapshot_hash,
                    "explicit_model_pin",
                    &request.required,
                    None,
                )),
            };
        }

        RoutingDecision {
            schema: ROUTING_DECISION_SCHEMA.to_string(),
            id: request.id.clone(),
            status: RoutingStatus::BlockedMissingCapability,
            selected_model_id: None,
            reason_code: "explicit_model_not_registered".to_string(),
            eligible_model_ids: Vec::new(),
            rejected_models: vec![ModelRejection {
                model_id: model_id.to_string(),
                reason_code: "not_registered".to_string(),
            }],
            model_snapshot_hash: snapshot_hash.to_string(),
            route_receipt: Some(ModelRouteReceipt {
                provider_id: None,
                model_id: Some(model_id.to_string()),
                registry_revision: snapshot_hash.to_string(),
                reason_code: "explicit_model_not_registered".to_string(),
                requested_capabilities: request.required.clone(),
                effective_limits: ModelLimits::default(),
                fallback_index: None,
            }),
        }
    }
}

/// Build the minimal routing provenance available at turn-accept time from the
/// persisted thread settings. This is intentionally not a live provider-health
/// or registry check; it records only the user's request so later supervisor
/// work can resolve it against the provider/model registry.
pub fn routing_decision_from_turn_settings(
    id: impl Into<String>,
    settings: &ConversationTurnSettings,
) -> RoutingDecision {
    let id = id.into();
    let requested = settings.model.model_id.clone();
    let snapshot_hash = stable_hash(
        serde_json::to_string(settings)
            .unwrap_or_else(|_| "unserializable-turn-settings".to_string())
            .as_bytes(),
    );
    match requested {
        Some(model_id) if !model_id.trim().is_empty() => RoutingDecision {
            schema: ROUTING_DECISION_SCHEMA.to_string(),
            id,
            status: RoutingStatus::Requested,
            selected_model_id: Some(model_id.clone()),
            reason_code: "explicit_thread_settings_model_requested".to_string(),
            eligible_model_ids: Vec::new(),
            rejected_models: Vec::new(),
            model_snapshot_hash: snapshot_hash.clone(),
            route_receipt: Some(ModelRouteReceipt {
                provider_id: None,
                model_id: Some(model_id),
                registry_revision: snapshot_hash,
                reason_code: "explicit_thread_settings_model_requested".to_string(),
                requested_capabilities: CapabilityRequirements::code(),
                effective_limits: ModelLimits::default(),
                fallback_index: None,
            }),
        },
        _ => RoutingDecision {
            schema: ROUTING_DECISION_SCHEMA.to_string(),
            id,
            status: RoutingStatus::BlockedMissingCapability,
            selected_model_id: None,
            reason_code: "thread_settings_model_not_selected".to_string(),
            eligible_model_ids: Vec::new(),
            rejected_models: Vec::new(),
            model_snapshot_hash: snapshot_hash.clone(),
            route_receipt: Some(blocked_route_receipt(
                &snapshot_hash,
                "thread_settings_model_not_selected",
                &CapabilityRequirements::code(),
            )),
        },
    }
}

pub fn resolve_routing_decision_from_repository(
    repo: &ProviderRepository,
    id: impl Into<String>,
    settings: &ConversationTurnSettings,
) -> Result<RoutingDecision> {
    let mut request = RoutingRequest::for_code(id);
    request.explicit_model_id = settings.model.model_id.clone();
    let registry = model_registry_from_repository(repo)?;
    Ok(registry.route(&request))
}

/// Build a [`ModelRegistry`] from the providers/models an operator has
/// already configured (enabled, disabled, or otherwise) in this workspace's
/// [`ProviderRepository`] via Provider Center. Configuring a provider
/// connection there is itself the explicit approval for this workspace to
/// dispatch conversation turns to it, so this registry opts in to
/// provider-call policy rather than relying on [`PermissionPolicy`]'s
/// deny-by-default (which still applies to any registry built from bare/
/// unconfigured model profiles, e.g. ad hoc `ModelRegistry::new` callers).
pub fn model_registry_from_repository(repo: &ProviderRepository) -> Result<ModelRegistry> {
    Ok(ModelRegistry::new(
        model_profiles_from_repository(repo)?,
        PermissionPolicy {
            allow_provider_call_without_approval: true,
            ..PermissionPolicy::default()
        },
    ))
}

fn model_profiles_from_repository(repo: &ProviderRepository) -> Result<Vec<ModelProfile>> {
    let mut profiles = Vec::new();
    for provider in repo.list_connections()? {
        for entry in repo.list_models(&provider.id)? {
            profiles.push(model_profile_from_catalog_entry(&provider, &entry));
        }
    }
    Ok(profiles)
}

fn model_profile_from_catalog_entry(
    provider: &ProviderConnection,
    entry: &ModelCatalogEntry,
) -> ModelProfile {
    let mut profile = ModelProfile {
        schema: MODEL_PROFILE_SCHEMA.to_string(),
        id: entry.id.clone(),
        provider_id: entry.provider_id.clone(),
        display_name: entry.display_name.clone(),
        enabled: entry.enabled && provider.enabled,
        capabilities: entry.capabilities.clone(),
        limits: entry.limits.clone(),
        pricing: entry.pricing.clone(),
        health: entry.health.clone(),
        role_scores: entry.role_scores.clone(),
        user_tags: vec!["provider-registry".to_string()],
        config_ref: SecretlessConfigRef {
            source: "provider-registry".to_string(),
            reference: format!("provider:{}:model:{}", provider.id, entry.remote_model_id),
        },
    };
    if matches!(
        provider.health.state,
        HealthState::Unavailable | HealthState::OpenCircuit
    ) {
        profile.health = HealthState::OpenCircuit;
    }
    profile
}

fn route_receipt_for_model(
    model: &ModelProfile,
    registry_revision: &str,
    reason_code: &str,
    requested_capabilities: &CapabilityRequirements,
    fallback_index: Option<u32>,
) -> ModelRouteReceipt {
    ModelRouteReceipt {
        provider_id: Some(model.provider_id.clone()),
        model_id: Some(model.id.clone()),
        registry_revision: registry_revision.to_string(),
        reason_code: reason_code.to_string(),
        requested_capabilities: requested_capabilities.clone(),
        effective_limits: model.limits.clone(),
        fallback_index,
    }
}

fn blocked_route_receipt(
    registry_revision: &str,
    reason_code: &str,
    requested_capabilities: &CapabilityRequirements,
) -> ModelRouteReceipt {
    ModelRouteReceipt {
        provider_id: None,
        model_id: None,
        registry_revision: registry_revision.to_string(),
        reason_code: reason_code.to_string(),
        requested_capabilities: requested_capabilities.clone(),
        effective_limits: ModelLimits::default(),
        fallback_index: None,
    }
}

pub fn fake_text_provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        schema: PROVIDER_DESCRIPTOR_SCHEMA.to_string(),
        id: "fake-local".to_string(),
        display_name: "Fake Local Provider".to_string(),
        enabled: true,
        capabilities: ModelCapabilities::text_code(),
        health: HealthState::Healthy,
        config_ref: SecretlessConfigRef {
            source: "testkit".to_string(),
            reference: "fake-provider-no-secret".to_string(),
        },
        secret_value_exposed: false,
    }
}

pub fn fake_text_model(id: impl Into<String>, enabled: bool) -> ModelProfile {
    let mut role_scores = BTreeMap::new();
    role_scores.insert(
        ModelRole::Code,
        RoleScore {
            score: 0.82,
            evidence_refs: vec!["fake-provider-deterministic-profile".to_string()],
        },
    );
    role_scores.insert(
        ModelRole::Planning,
        RoleScore {
            score: 0.74,
            evidence_refs: vec!["fake-provider-deterministic-profile".to_string()],
        },
    );
    ModelProfile {
        schema: MODEL_PROFILE_SCHEMA.to_string(),
        id: id.into(),
        provider_id: "fake-local".to_string(),
        display_name: "Fake Text Model".to_string(),
        enabled,
        capabilities: ModelCapabilities::text_code(),
        limits: ModelLimits {
            max_input_tokens: Some(32_000),
            max_output_tokens: Some(8_000),
            requests_per_minute: Some(60),
            tokens_per_minute: None,
            max_concurrency: Some(4),
        },
        pricing: None,
        health: HealthState::Healthy,
        role_scores,
        user_tags: vec!["deterministic".to_string(), "fake".to_string()],
        config_ref: SecretlessConfigRef {
            source: "testkit".to_string(),
            reference: "fake-model-no-secret".to_string(),
        },
    }
}

pub fn fake_image_model(id: impl Into<String>, enabled: bool) -> ModelProfile {
    let mut model = fake_text_model(id, enabled);
    model.display_name = "Fake Image Model".to_string();
    model.capabilities = ModelCapabilities::image();
    model.role_scores.clear();
    model.role_scores.insert(
        ModelRole::Image,
        RoleScore {
            score: 0.9,
            evidence_refs: vec!["fake-provider-deterministic-profile".to_string()],
        },
    );
    model
}

pub fn fake_vision_model(id: impl Into<String>, enabled: bool) -> ModelProfile {
    let mut model = fake_text_model(id, enabled);
    model.display_name = "Fake Vision Model".to_string();
    model.capabilities = ModelCapabilities {
        text: true,
        vision_input: true,
        structured_output: true,
        ..ModelCapabilities::default()
    };
    model.role_scores.clear();
    model.role_scores.insert(
        ModelRole::Vision,
        RoleScore {
            score: 0.88,
            evidence_refs: vec!["fake-provider-deterministic-profile".to_string()],
        },
    );
    model
}

fn eligibility_reason(model: &ModelProfile, request: &RoutingRequest) -> Option<&'static str> {
    if !model.enabled {
        return Some("model_disabled");
    }
    if matches!(
        model.health,
        HealthState::Unavailable | HealthState::OpenCircuit
    ) {
        return Some("provider_health_open");
    }
    if !request.budget_allowed {
        return Some("budget_denied");
    }
    if !model.capabilities.satisfies(&request.required) {
        return Some("capability_mismatch");
    }
    None
}

fn capability_requirements_for_role(role: &ModelRole) -> CapabilityRequirements {
    match role {
        ModelRole::Code | ModelRole::Verification => CapabilityRequirements::code(),
        ModelRole::Planning | ModelRole::Arbiter => CapabilityRequirements {
            text: true,
            structured_output: true,
            ..CapabilityRequirements::default()
        },
        ModelRole::Vision => CapabilityRequirements {
            vision_input: true,
            ..CapabilityRequirements::default()
        },
        ModelRole::Image => CapabilityRequirements::image_output(),
        ModelRole::General => CapabilityRequirements::text(),
    }
}

fn role_plan_status(assignments: &[RoleRouteAssignment]) -> RoutingStatus {
    if assignments.is_empty() {
        return RoutingStatus::BlockedMissingCapability;
    }
    if assignments
        .iter()
        .all(|assignment| assignment.decision.status.has_resolved_model())
    {
        return RoutingStatus::Resolved;
    }
    if assignments
        .iter()
        .any(|assignment| assignment.decision.status == RoutingStatus::BlockedPolicy)
    {
        RoutingStatus::BlockedPolicy
    } else if assignments
        .iter()
        .any(|assignment| assignment.decision.status == RoutingStatus::BlockedProviderHealth)
    {
        RoutingStatus::BlockedProviderHealth
    } else if assignments
        .iter()
        .any(|assignment| assignment.decision.status == RoutingStatus::BlockedDisabled)
    {
        RoutingStatus::BlockedDisabled
    } else {
        RoutingStatus::BlockedMissingCapability
    }
}

fn score_model(model: &ModelProfile, role: &ModelRole) -> f64 {
    let role_quality = model
        .role_scores
        .get(role)
        .or_else(|| model.role_scores.get(&ModelRole::General))
        .map(|score| score.score)
        .unwrap_or(0.5);
    let reliability = match model.health {
        HealthState::Healthy => 1.0,
        HealthState::Degraded => 0.55,
        HealthState::Unknown => 0.4,
        HealthState::Unavailable | HealthState::OpenCircuit => 0.0,
    };
    let structured = if model.capabilities.structured_output {
        1.0
    } else {
        0.0
    };
    let cost_efficiency = pricing_efficiency_score(model, role);
    let concurrency = concurrency_score(model);
    role_quality * 0.46
        + reliability * 0.20
        + structured * 0.08
        + cost_efficiency * 0.16
        + concurrency * 0.10
}

fn pricing_efficiency_score(model: &ModelProfile, role: &ModelRole) -> f64 {
    let Some(pricing) = model.pricing.as_ref() else {
        return 0.55;
    };
    let mut normalized_cost = pricing.input_per_million_usd.unwrap_or(0.0)
        + pricing.output_per_million_usd.unwrap_or(0.0);
    if matches!(role, ModelRole::Image) {
        normalized_cost += pricing.image_output_usd.unwrap_or(0.0) * 20.0;
    }
    if normalized_cost <= 0.0 {
        return 1.0;
    }
    (1.0 / (1.0 + (normalized_cost / 20.0))).clamp(0.0, 1.0)
}

fn concurrency_score(model: &ModelProfile) -> f64 {
    model
        .limits
        .max_concurrency
        .map(|limit| f64::from(limit.min(16)) / 16.0)
        .unwrap_or(0.25)
}

fn stable_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensks_contracts::{
        LatencyBucket, MODEL_CATALOG_ENTRY_SCHEMA, PROVIDER_PROBE_RECEIPT_SCHEMA, PricingInfo,
        ProviderConcurrencyPolicy, ProviderEndpoint, ProviderHealthSnapshot, ProviderKind,
        ProviderProbeHttpCategory, SecretRef, SecretStoreKind,
    };
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn sample_connection(revision: u64) -> ProviderConnection {
        ProviderConnection {
            schema: PROVIDER_CONNECTION_SCHEMA.to_string(),
            id: "provider-1".to_string(),
            kind: ProviderKind::OpenRouter,
            display_name: "OpenRouter".to_string(),
            enabled: true,
            endpoint: ProviderEndpoint {
                base_url: "https://openrouter.ai/api/v1".to_string(),
                allow_insecure_http: false,
            },
            auth: SecretRef {
                schema: SECRET_REF_SCHEMA.to_string(),
                store: SecretStoreKind::MacosKeychain,
                service: "ai.opensks.provider.openrouter".to_string(),
                account: "provider-1".to_string(),
                version: revision,
            },
            organization_ref: None,
            project_ref: None,
            health: ProviderHealthSnapshot::unknown(),
            concurrency: ProviderConcurrencyPolicy {
                max_concurrent_requests: 4,
                requests_per_minute: Some(60),
                tokens_per_minute: None,
            },
            created_at_ms: 10,
            updated_at_ms: 20,
            revision,
        }
    }

    fn sample_catalog_entry(enabled: bool) -> ModelCatalogEntry {
        let mut role_scores = BTreeMap::new();
        role_scores.insert(
            ModelRole::Code,
            RoleScore {
                score: 0.88,
                evidence_refs: vec!["mock-catalog".to_string()],
            },
        );
        ModelCatalogEntry {
            schema: MODEL_CATALOG_ENTRY_SCHEMA.to_string(),
            id: "provider-1/code-model".to_string(),
            provider_id: "provider-1".to_string(),
            remote_model_id: "code-model".to_string(),
            display_name: "Code Model".to_string(),
            enabled,
            capabilities: ModelCapabilities::text_code(),
            limits: ModelLimits {
                max_input_tokens: Some(128_000),
                max_output_tokens: Some(16_000),
                requests_per_minute: Some(60),
                tokens_per_minute: None,
                max_concurrency: Some(4),
            },
            pricing: None,
            health: HealthState::Healthy,
            role_scores,
            catalog_revision: "catalog-rev-1".to_string(),
        }
    }

    fn sample_probe_receipt() -> ProviderProbeReceipt {
        ProviderProbeReceipt {
            schema: PROVIDER_PROBE_RECEIPT_SCHEMA.to_string(),
            provider_id: "provider-1".to_string(),
            endpoint_host_redacted: "openrouter.ai".to_string(),
            http_category: ProviderProbeHttpCategory::Success,
            latency_bucket: LatencyBucket::Under1S,
            auth_accepted: true,
            model_list_available: true,
            catalog_count: Some(1),
            occurred_at_ms: 30,
            reason_code: "probe_ok".to_string(),
            diagnostic_ref: None,
        }
    }

    fn priced_role_model(
        id: &str,
        role: ModelRole,
        role_score: f64,
        input_cost: f64,
        output_cost: f64,
        max_concurrency: u32,
    ) -> ModelProfile {
        let mut model = fake_text_model(id, true);
        model.role_scores.clear();
        model.role_scores.insert(
            role,
            RoleScore {
                score: role_score,
                evidence_refs: vec!["test-role-profile".to_string()],
            },
        );
        model.role_scores.insert(
            ModelRole::General,
            RoleScore {
                score: 0.50,
                evidence_refs: vec!["test-general-profile".to_string()],
            },
        );
        model.pricing = Some(PricingInfo {
            input_per_million_usd: Some(input_cost),
            output_per_million_usd: Some(output_cost),
            image_output_usd: None,
        });
        model.limits.max_concurrency = Some(max_concurrency);
        model
    }

    fn assigned_model(plan: &RoleRoutingPlan, role: ModelRole) -> &str {
        plan.assignments
            .iter()
            .find(|assignment| assignment.role == role)
            .and_then(|assignment| assignment.decision.selected_model_id.as_deref())
            .expect("role assignment selected a model")
    }

    #[test]
    fn disabled_model_is_never_routed() {
        let registry = ModelRegistry::new(
            vec![fake_text_model("disabled-code", false)],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );
        let decision = registry.route(&RoutingRequest::for_code("route-disabled"));
        assert_eq!(decision.status, RoutingStatus::BlockedDisabled);
        assert!(decision.selected_model_id.is_none());
        assert_eq!(decision.rejected_models[0].reason_code, "model_disabled");
        let receipt = decision.route_receipt.expect("route receipt");
        assert_eq!(receipt.reason_code, "blocked_no_enabled_compatible_model");
        assert!(receipt.provider_id.is_none());
        assert!(receipt.requested_capabilities.code);
    }

    #[test]
    fn image_task_blocks_text_only_model() {
        let registry = ModelRegistry::new(
            vec![fake_text_model("text-only", true)],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );
        let decision = registry.route(&RoutingRequest::for_image("route-image"));
        assert_eq!(decision.status, RoutingStatus::BlockedMissingCapability);
        assert_eq!(
            decision.rejected_models[0].reason_code,
            "capability_mismatch"
        );
    }

    #[test]
    fn vision_task_uses_vision_capability_without_requiring_image_output() {
        let registry = ModelRegistry::new(
            vec![
                fake_image_model("image-output", true),
                fake_vision_model("vision-only", true),
            ],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );
        let decision = registry.route(&RoutingRequest::for_vision("route-vision"));
        assert_eq!(decision.status, RoutingStatus::Resolved);
        assert_eq!(decision.selected_model_id.as_deref(), Some("vision-only"));
        let receipt = decision.route_receipt.expect("route receipt");
        assert!(receipt.requested_capabilities.vision_input);
        assert!(!receipt.requested_capabilities.image_output);
        assert_eq!(receipt.provider_id.as_deref(), Some("fake-local"));
    }

    #[test]
    fn role_routing_allocates_distinct_specialists_from_registry() {
        let registry = ModelRegistry::new(
            vec![
                priced_role_model("planner", ModelRole::Planning, 0.96, 4.0, 8.0, 4),
                priced_role_model("implementer", ModelRole::Code, 0.95, 5.0, 10.0, 4),
                priced_role_model("verifier", ModelRole::Verification, 0.94, 3.0, 6.0, 4),
                priced_role_model("arbiter", ModelRole::Arbiter, 0.93, 2.0, 4.0, 4),
            ],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );

        let plan = registry.route_roles(&RoleRoutingRequest::hyperparallel_default(
            "route-hyperparallel",
        ));

        assert_eq!(plan.status, RoutingStatus::Resolved);
        assert_eq!(plan.reason_code, "role_allocation_resolved_distinct_models");
        assert_eq!(plan.assignments.len(), 4);
        assert!(
            plan.evidence_refs
                .contains(&"provider:role-routing".to_string())
        );
        assert!(
            plan.evidence_refs
                .contains(&"provider:health-cost-concurrency-scoring".to_string())
        );
        assert_eq!(assigned_model(&plan, ModelRole::Planning), "planner");
        assert_eq!(assigned_model(&plan, ModelRole::Code), "implementer");
        assert_eq!(assigned_model(&plan, ModelRole::Verification), "verifier");
        assert_eq!(assigned_model(&plan, ModelRole::Arbiter), "arbiter");
        assert!(plan.assignments.iter().all(|assignment| {
            assignment.decision.status == RoutingStatus::Resolved
                && assignment.decision.model_snapshot_hash == plan.model_snapshot_hash
        }));
        assert!(plan.assignments.iter().any(|assignment| {
            assignment.decision.reason_code == "distinct_role_highest_weighted_score"
        }));
        let verifier_receipt = plan
            .assignments
            .iter()
            .find(|assignment| assignment.role == ModelRole::Verification)
            .and_then(|assignment| assignment.decision.route_receipt.as_ref())
            .expect("verifier route receipt");
        assert_eq!(verifier_receipt.registry_revision, plan.model_snapshot_hash);
        assert!(verifier_receipt.requested_capabilities.code);
        assert!(
            !plan
                .assignments
                .iter()
                .any(|assignment| assignment.reused_model)
        );
    }

    #[test]
    fn role_routing_reuses_single_model_when_no_distinct_candidate_exists() {
        let mut model = fake_text_model("omni", true);
        model.role_scores.insert(
            ModelRole::Verification,
            RoleScore {
                score: 0.80,
                evidence_refs: vec!["test-role-profile".to_string()],
            },
        );
        model.role_scores.insert(
            ModelRole::Arbiter,
            RoleScore {
                score: 0.78,
                evidence_refs: vec!["test-role-profile".to_string()],
            },
        );
        let registry = ModelRegistry::new(vec![model], PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() });

        let plan = registry.route_roles(&RoleRoutingRequest::hyperparallel_default(
            "route-single-model",
        ));

        assert_eq!(plan.status, RoutingStatus::Resolved);
        assert_eq!(
            plan.reason_code,
            "role_allocation_resolved_with_model_reuse"
        );
        assert!(
            plan.evidence_refs
                .contains(&"provider:single-model-role-reuse".to_string())
        );
        assert!(plan.assignments[0].decision.selected_model_id.is_some());
        assert!(plan.assignments.iter().skip(1).all(|assignment| {
            assignment.reused_model
                && assignment.decision.selected_model_id.as_deref() == Some("omni")
        }));
    }

    #[test]
    fn role_routing_blocks_only_roles_without_required_capability() {
        let registry = ModelRegistry::new(
            vec![fake_text_model("text-only", true)],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );
        let request =
            RoleRoutingRequest::with_roles("route-mixed", vec![ModelRole::Code, ModelRole::Image]);

        let plan = registry.route_roles(&request);

        assert_eq!(plan.status, RoutingStatus::BlockedMissingCapability);
        assert_eq!(plan.reason_code, "role_allocation_blocked");
        assert_eq!(plan.assignments.len(), 2);
        assert_eq!(plan.assignments[0].decision.status, RoutingStatus::Resolved);
        assert_eq!(
            plan.assignments[1].decision.status,
            RoutingStatus::BlockedMissingCapability
        );
        assert_eq!(
            plan.assignments[1].decision.rejected_models[0].reason_code,
            "capability_mismatch"
        );
    }

    #[test]
    fn close_role_scores_consider_cost_and_model_concurrency() {
        let expensive = priced_role_model("expensive", ModelRole::Code, 0.90, 100.0, 100.0, 1);
        let cheaper_parallel =
            priced_role_model("cheaper-parallel", ModelRole::Code, 0.89, 1.0, 1.0, 8);
        let registry = ModelRegistry::new(
            vec![expensive, cheaper_parallel],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );

        let decision = registry.route(&RoutingRequest::for_code("route-cost"));

        assert_eq!(decision.status, RoutingStatus::Resolved);
        assert_eq!(
            decision.selected_model_id.as_deref(),
            Some("cheaper-parallel")
        );
        assert_eq!(decision.reason_code, "highest_weighted_role_score");
    }

    #[test]
    fn single_enabled_compatible_model_fallback_routes() {
        let registry = ModelRegistry::new(
            vec![
                fake_text_model("disabled-code", false),
                fake_text_model("only-code", true),
            ],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );
        let decision = registry.route(&RoutingRequest::for_code("route-code"));
        assert_eq!(decision.status, RoutingStatus::Resolved);
        assert_eq!(decision.selected_model_id.as_deref(), Some("only-code"));
        assert_eq!(decision.reason_code, "single_enabled_compatible_model");
        let receipt = decision.route_receipt.expect("route receipt");
        assert_eq!(receipt.provider_id.as_deref(), Some("fake-local"));
        assert_eq!(receipt.model_id.as_deref(), Some("only-code"));
        assert_eq!(receipt.reason_code, "single_enabled_compatible_model");
        assert_eq!(receipt.registry_revision, decision.model_snapshot_hash);
        assert!(receipt.requested_capabilities.code);
    }

    #[test]
    fn repository_routing_allows_unprobed_codex_lb_seed_models() {
        let repo = ProviderRepository::open_in_memory().expect("repo");
        let mut connection = sample_connection(1);
        connection.kind = ProviderKind::CodexLb;
        connection.display_name = "codex-lb".to_string();
        connection.endpoint.base_url = "https://codex.hyper-lab.xyz/backend-api/codex".to_string();
        connection.auth = SecretRef {
            schema: SECRET_REF_SCHEMA.to_string(),
            store: SecretStoreKind::ExternalBroker,
            service: "env".to_string(),
            account: "CODEX_LB_API_KEY".to_string(),
            version: 1,
        };
        connection.health = ProviderHealthSnapshot::unknown();
        repo.upsert_connection(&connection, None, 1)
            .expect("upsert connection");
        let mut model = sample_catalog_entry(true);
        model.id = "provider-1/gpt-5.5".to_string();
        model.provider_id = "provider-1".to_string();
        model.remote_model_id = "gpt-5.5".to_string();
        model.display_name = "GPT-5.5".to_string();
        model.health = HealthState::Unknown;
        repo.sync_models("provider-1", &[model], 2)
            .expect("sync models");

        let decision = resolve_routing_decision_from_repository(
            &repo,
            "route-codex-lb-unprobed",
            &ConversationTurnSettings {
                model: opensks_contracts::ModelSelection {
                    mode: opensks_contracts::ModelSelectionMode::Auto,
                    model_id: None,
                    fallback_model_ids: Vec::new(),
                },
                reasoning_effort: opensks_contracts::ReasoningEffort::Standard,
                execution_mode: opensks_contracts::ExecutionMode::Worktree,
                pipeline_id: "auto".to_string(),
                graph_revision: None,
                max_parallelism: 16,
                verifier_count: 1,
                tool_policy_id: "project-default".to_string(),
                approval_policy_id: "safe-interactive".to_string(),
                token_budget: None,
                cost_budget_usd: None,
                timeout_ms: None,
                image_model_id: None,
            },
        )
        .expect("route");

        assert_eq!(decision.status, RoutingStatus::Resolved);
        assert_eq!(
            decision.selected_model_id.as_deref(),
            Some("provider-1/gpt-5.5")
        );
    }

    #[test]
    fn unhealthy_provider_is_blocked() {
        let mut model = fake_text_model("bad-health", true);
        model.health = HealthState::OpenCircuit;
        let registry = ModelRegistry::new(vec![model], PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() });
        let decision = registry.route(&RoutingRequest::for_code("route-health"));
        assert_eq!(decision.status, RoutingStatus::BlockedProviderHealth);
        assert_eq!(
            decision.rejected_models[0].reason_code,
            "provider_health_open"
        );
    }

    #[test]
    fn unknown_explicit_model_does_not_infer_provider_from_prefix() {
        let registry = ModelRegistry::new(Vec::new(), PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() });
        let request = RoutingRequest {
            explicit_model_id: Some("openrouter/nonexistent-model".to_string()),
            ..RoutingRequest::for_code("route-unknown-explicit")
        };
        let decision = registry.route(&request);
        assert_eq!(decision.status, RoutingStatus::BlockedMissingCapability);
        assert_eq!(decision.reason_code, "explicit_model_not_registered");
        assert_eq!(
            decision.selected_model_id, None,
            "unregistered explicit models are not dispatchable"
        );
        let receipt = decision.route_receipt.expect("blocked route receipt");
        assert!(
            receipt.provider_id.is_none(),
            "provider ids must come from registered adapter descriptors"
        );
        assert_eq!(
            receipt.model_id.as_deref(),
            Some("openrouter/nonexistent-model")
        );
    }

    #[test]
    fn turn_settings_routing_decision_records_request_without_fake_registry_resolution() {
        let mut settings = ConversationTurnSettings {
            model: opensks_contracts::ModelSelection {
                mode: opensks_contracts::ModelSelectionMode::Auto,
                model_id: None,
                fallback_model_ids: Vec::new(),
            },
            reasoning_effort: opensks_contracts::ReasoningEffort::Standard,
            execution_mode: opensks_contracts::ExecutionMode::Worktree,
            pipeline_id: "auto".to_string(),
            graph_revision: None,
            max_parallelism: 4,
            verifier_count: 1,
            tool_policy_id: "project-default".to_string(),
            approval_policy_id: "safe-interactive".to_string(),
            token_budget: None,
            cost_budget_usd: None,
            timeout_ms: None,
            image_model_id: None,
        };
        let blocked = routing_decision_from_turn_settings("route-turn-settings", &settings);
        assert_eq!(blocked.status, RoutingStatus::BlockedMissingCapability);
        assert_eq!(blocked.reason_code, "thread_settings_model_not_selected");
        assert!(blocked.selected_model_id.is_none());
        let blocked_receipt = blocked.route_receipt.expect("blocked route receipt");
        assert_eq!(
            blocked_receipt.reason_code,
            "thread_settings_model_not_selected"
        );
        assert!(blocked_receipt.provider_id.is_none());
        assert!(blocked_receipt.model_id.is_none());

        settings.model.model_id = Some("openrouter/code-model".to_string());
        let requested = routing_decision_from_turn_settings("route-turn-settings", &settings);
        assert_eq!(requested.status, RoutingStatus::Requested);
        assert_eq!(
            requested.selected_model_id.as_deref(),
            Some("openrouter/code-model")
        );
        assert_eq!(
            requested.reason_code,
            "explicit_thread_settings_model_requested"
        );
        assert!(
            requested.eligible_model_ids.is_empty(),
            "turn settings are not a provider registry snapshot"
        );
        let routed_receipt = requested.route_receipt.expect("requested route receipt");
        assert!(
            routed_receipt.provider_id.is_none(),
            "provider ids must come from registry descriptors, not model id prefixes"
        );
        assert_eq!(
            routed_receipt.model_id.as_deref(),
            Some("openrouter/code-model")
        );
        assert_eq!(
            routed_receipt.registry_revision,
            requested.model_snapshot_hash
        );
    }

    #[test]
    fn provider_repository_persists_secret_refs_without_plaintext() {
        let repo = ProviderRepository::open_in_memory().expect("provider repo");
        assert_eq!(repo.user_version().expect("user version"), 1);
        let connection = sample_connection(1);
        let receipt = repo
            .upsert_connection(&connection, None, 100)
            .expect("save provider");
        assert_eq!(receipt.mutation, ProviderMutationKind::Created);
        assert!(!receipt.secret_value_exposed);
        assert_eq!(receipt.secret_ref.as_ref().unwrap().version, 1);

        let stored = repo
            .stored_connection_json("provider-1")
            .expect("stored connection json");
        assert!(stored.contains("\"store\":\"macos_keychain\""));
        assert!(!stored.contains("sk-"));
        assert!(!stored.contains("api_key"));
        let listed = repo.list_connections().expect("list providers");
        assert_eq!(listed, vec![connection.clone()]);
        assert_eq!(
            repo.get_connection("provider-1").expect("get provider"),
            connection
        );
    }

    #[test]
    fn provider_repository_enforces_revision_cas_and_delete_cleanup() {
        let repo = ProviderRepository::open_in_memory().expect("provider repo");
        let mut connection = sample_connection(1);
        repo.upsert_connection(&connection, None, 100)
            .expect("create provider");
        connection.display_name = "OpenRouter Updated".to_string();
        connection.revision = 2;
        let conflict = repo
            .upsert_connection(&connection, Some(99), 110)
            .expect_err("revision conflict");
        assert!(matches!(
            conflict,
            ProviderError::RevisionConflict {
                provider_id,
                expected: 99,
                actual: 1,
            } if provider_id == "provider-1"
        ));

        repo.upsert_connection(&connection, Some(1), 120)
            .expect("update provider");
        let disabled = repo
            .set_provider_enabled("provider-1", false, 2, 130)
            .expect("disable provider");
        assert_eq!(disabled.mutation, ProviderMutationKind::Disabled);
        assert_eq!(disabled.revision, 3);

        let delete_receipt = repo
            .delete_connection("provider-1", 3, 140)
            .expect("delete provider");
        assert_eq!(delete_receipt.mutation, ProviderMutationKind::Deleted);
        assert!(matches!(
            repo.get_connection("provider-1"),
            Err(ProviderError::NotFound(_))
        ));
    }

    #[test]
    fn provider_repository_syncs_models_and_probe_receipts() {
        let repo = ProviderRepository::open_in_memory().expect("provider repo");
        repo.upsert_connection(&sample_connection(1), None, 100)
            .expect("create provider");
        let sync = repo
            .sync_models("provider-1", &[sample_catalog_entry(true)], 110)
            .expect("sync models");
        assert_eq!(sync.mutation, ProviderMutationKind::ModelsSynced);
        let models = repo.list_models("provider-1").expect("list models");
        assert_eq!(models.len(), 1);
        assert!(models[0].enabled);

        let disabled = repo
            .set_model_enabled("provider-1/code-model", false, 120)
            .expect("disable model");
        assert!(!disabled.enabled);
        repo.sync_models("provider-1", &[sample_catalog_entry(true)], 125)
            .expect("resync models");
        assert!(
            !repo
                .get_model("provider-1/code-model")
                .expect("get model")
                .enabled
        );

        let health = health_snapshot_from_probe_receipt(&sample_probe_receipt());
        let updated = repo
            .set_provider_health("provider-1", health, 126)
            .expect("set health");
        assert_eq!(updated.health.state, HealthState::Healthy);
        assert_eq!(updated.revision, 2);

        let receipt = sample_probe_receipt();
        repo.record_probe_receipt(&receipt).expect("record probe");
        let latest = repo
            .latest_probe_receipt("provider-1")
            .expect("latest probe")
            .expect("probe exists");
        assert_eq!(latest.reason_code, "probe_ok");
        assert!(
            !serde_json::to_string(&latest)
                .unwrap()
                .contains("raw_response")
        );
    }

    #[test]
    fn repository_backed_routing_requires_enabled_healthy_registry_models() {
        let repo = ProviderRepository::open_in_memory().expect("provider repo");
        let mut connection = sample_connection(1);
        connection.health = ProviderHealthSnapshot {
            state: HealthState::Healthy,
            circuit_open: false,
            checked_at_ms: Some(100),
            reason_code: "probe_ok".to_string(),
            diagnostic_ref: None,
        };
        repo.upsert_connection(&connection, None, 100)
            .expect("create provider");
        repo.sync_models("provider-1", &[sample_catalog_entry(true)], 110)
            .expect("sync models");
        let mut settings = ConversationTurnSettings {
            model: opensks_contracts::ModelSelection {
                mode: opensks_contracts::ModelSelectionMode::Pinned,
                model_id: Some("provider-1/code-model".to_string()),
                fallback_model_ids: Vec::new(),
            },
            reasoning_effort: opensks_contracts::ReasoningEffort::Standard,
            execution_mode: opensks_contracts::ExecutionMode::Worktree,
            pipeline_id: "auto".to_string(),
            graph_revision: None,
            max_parallelism: 4,
            verifier_count: 1,
            tool_policy_id: "project-default".to_string(),
            approval_policy_id: "safe-interactive".to_string(),
            token_budget: None,
            cost_budget_usd: None,
            timeout_ms: None,
            image_model_id: None,
        };

        let resolved = resolve_routing_decision_from_repository(&repo, "route-registry", &settings)
            .expect("route from registry");
        assert_eq!(resolved.status, RoutingStatus::Resolved);
        assert_eq!(
            resolved.selected_model_id.as_deref(),
            Some("provider-1/code-model")
        );
        assert_eq!(
            resolved
                .route_receipt
                .as_ref()
                .and_then(|receipt| receipt.provider_id.as_deref()),
            Some("provider-1")
        );

        repo.set_model_enabled("provider-1/code-model", false, 120)
            .expect("disable model");
        let blocked = resolve_routing_decision_from_repository(&repo, "route-disabled", &settings)
            .expect("route disabled");
        assert_eq!(blocked.status, RoutingStatus::BlockedDisabled);
        assert!(blocked.selected_model_id.is_none());

        settings.model.model_id = Some("provider-1/unknown".to_string());
        let missing = resolve_routing_decision_from_repository(&repo, "route-missing", &settings)
            .expect("route missing");
        assert_eq!(missing.status, RoutingStatus::BlockedMissingCapability);
        assert_eq!(missing.reason_code, "explicit_model_not_registered");
    }

    #[test]
    fn provider_probe_fetches_openai_compatible_models_without_secret_output() {
        let body = serde_json::json!({
            "object": "list",
            "data": [
                {
                    "id": "openai/gpt-4o-mini",
                    "name": "GPT-4o mini",
                    "context_length": 128000,
                    "architecture": {
                        "input_modalities": ["text", "image"],
                        "output_modalities": ["text"]
                    },
                    "supported_parameters": ["tools", "response_format"]
                }
            ]
        })
        .to_string();
        let endpoint = spawn_models_server(200, &body, Some("fixture-credential"));
        let mut connection = sample_connection(1);
        connection.endpoint.base_url = endpoint;
        connection.endpoint.allow_insecure_http = true;

        let outcome = probe_openai_compatible_provider(
            &connection,
            "fixture-credential",
            500,
            Duration::from_secs(3),
        )
        .expect("provider probe");

        assert_eq!(
            outcome.receipt.http_category,
            ProviderProbeHttpCategory::Success
        );
        assert!(outcome.receipt.auth_accepted);
        assert_eq!(outcome.receipt.catalog_count, Some(1));
        assert_eq!(outcome.models[0].id, "provider-1/openai/gpt-4o-mini");
        assert_eq!(outcome.models[0].display_name, "GPT-4o mini");
        assert!(outcome.models[0].capabilities.code);
        assert!(outcome.models[0].capabilities.vision_input);
        assert!(outcome.models[0].capabilities.tool_use);
        assert!(outcome.models[0].capabilities.long_context);
        let serialized = serde_json::to_string(&outcome.receipt).expect("receipt json");
        assert!(!serialized.contains("fixture-credential"));
        assert_eq!(
            health_snapshot_from_probe_receipt(&outcome.receipt).state,
            HealthState::Healthy
        );
    }

    #[test]
    fn provider_probe_classifies_auth_rejection_without_response_body_leak() {
        let endpoint = spawn_models_server(
            401,
            r#"{"error":{"message":"fixture-credential should stay private"}}"#,
            Some("fixture-credential"),
        );
        let mut connection = sample_connection(1);
        connection.endpoint.base_url = endpoint;
        connection.endpoint.allow_insecure_http = true;

        let outcome = probe_openai_compatible_provider(
            &connection,
            "fixture-credential",
            600,
            Duration::from_secs(3),
        )
        .expect("provider probe");

        assert_eq!(
            outcome.receipt.http_category,
            ProviderProbeHttpCategory::AuthRejected
        );
        assert!(!outcome.receipt.auth_accepted);
        assert!(outcome.models.is_empty());
        let serialized = serde_json::to_string(&outcome.receipt).expect("receipt json");
        assert!(!serialized.contains("fixture-credential"));
        assert_eq!(
            health_snapshot_from_probe_receipt(&outcome.receipt).state,
            HealthState::Unavailable
        );
    }

    fn spawn_models_server(status: u16, body: &str, expected_bearer: Option<&str>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("local addr");
        let body = body.to_string();
        let expected_bearer = expected_bearer.map(str::to_string);
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = [0_u8; 4096];
            let bytes = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(request.starts_with("GET /v1/models "));
            if let Some(secret) = expected_bearer {
                let lower_request = request.to_ascii_lowercase();
                assert!(
                    lower_request
                        .contains(&format!("authorization: bearer {secret}").to_ascii_lowercase())
                );
            }
            let reason = if status == 200 { "OK" } else { "Unauthorized" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });
        format!("http://{address}/v1")
    }
}
