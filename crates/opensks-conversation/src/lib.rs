//! Durable project/conversation/message persistence (PR-024).
//!
//! A WAL-mode SQLite repository for projects, conversations, and messages with
//! cursor pagination and an FTS index over secret-redacted content. There is NO
//! engine dispatch here — that lands in PR-027. Raw message text is redacted
//! before it is stored in the searchable `content_redacted` column / FTS index;
//! the original may be held encrypted out of band (content_ciphertext column).

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use opensks_contracts::{
    CONVERSATION_DIGEST_SCHEMA, CONVERSATION_MESSAGE_SCHEMA, CONVERSATION_SUMMARY_SCHEMA,
    CONVERSATION_TURN_ACCEPTED_SCHEMA, ConversationDeleteCounts, ConversationDigest,
    ConversationFilter, ConversationMessage, ConversationStatus, ConversationSummary,
    ConversationThreadSettings, ConversationTurnAccepted, ConversationTurnSettings,
    ConversationTurnStartRequest, EventKind, ExecutionEventEnvelope, MessageRole, MessageState,
    RunProjectionState, TIMELINE_ITEM_SCHEMA, TimelineItem, TimelineItemKind, TitleSource,
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};

const MIGRATION_VERSION: i32 = 6;
pub const CONVERSATION_DB_RELATIVE_PATH: &str = ".opensks/runtime/conversations.sqlite3";
const CONVERSATION_TIMELINE_SEQUENCE_STRIDE: i64 = 1_000_000;
const ASSISTANT_EVENT_SNIPPET_CHARS: usize = 700;
const GIT_RECEIPT_ANCHOR_PREFIX: &str = "[OpenSKS git receipt anchor]";

#[derive(Debug, thiserror::Error)]
pub enum ConversationError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("conversation not found: {0}")]
    NotFound(String),
    #[error("invalid stored enum value: {0}")]
    InvalidEnum(String),
    #[error("external event anchor already exists: {0}")]
    DuplicateAnchor(String),
    #[error("missing run projection for accepted replay: {0}")]
    ProjectionMissing(String),
    #[error("missing settings digest for accepted turn: {0}")]
    AcceptedSettingsDigestMissing(String),
    #[error("invalid run projection state: {0}")]
    InvalidRunProjectionState(String),
    #[error("stale turn supervisor lease for run: {0}")]
    StaleLease(String),
    #[error(
        "stale thread settings revision for conversation {conversation_id}: client={client_updated_at_ms}, canonical={canonical_updated_at_ms}"
    )]
    StaleThreadSettingsRevision {
        conversation_id: String,
        client_updated_at_ms: u64,
        canonical_updated_at_ms: u64,
    },
}

type Result<T> = std::result::Result<T, ConversationError>;

/// A run linked to a conversation via the `conversation_runs` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationRunRef {
    pub turn_id: String,
    pub run_id: String,
    pub message_id: String,
    pub relation: String,
    /// Real run state from `run_projections`, or `None` when no projection has
    /// been recorded. Callers MUST surface `None` as `unknown` — never as a
    /// fabricated `completed` (recovery directive §6.7).
    pub run_state: Option<String>,
}

/// The persisted identifiers for a previously executed turn, keyed by
/// (conversation_id, idempotency_key). Used to make `turn-start` idempotent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnIdempotencyRecord {
    pub turn_id: String,
    pub user_message_id: String,
    pub assistant_message_id: String,
    pub run_id: String,
}

/// Durable v2 turn/run snapshot recorded at accept time. This is the migration
/// bridge from the v1 CLI shim to the asynchronous TurnSupervisor path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRunSnapshot<'a> {
    pub turn_id: &'a str,
    pub run_id: &'a str,
    pub project_id: &'a str,
    pub conversation_id: &'a str,
    pub client_turn_id: &'a str,
    pub request_id: &'a str,
    pub idempotency_key: &'a str,
    pub state: &'a str,
    pub effective_settings_json: &'a str,
    pub settings_digest: &'a str,
    pub model_routing_decision_json: Option<&'a str>,
    pub now_ms: u64,
}

/// Opaque encrypted raw content for a message. Callers own encryption and key
/// provenance; the conversation store only persists ciphertext separately from
/// the searchable redacted copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRawContentCiphertext {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
}

/// A queued turn claimed by a TurnSupervisor lease. This is the durable handoff
/// boundary between immediate accept and later adapter/tool execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnSupervisorLease {
    pub turn_id: String,
    pub run_id: String,
    pub project_id: String,
    pub conversation_id: String,
    pub assistant_message_id: String,
    pub effective_settings_json: String,
    pub model_routing_decision_json: Option<String>,
    pub lease_owner: String,
    pub lease_expires_at_ms: u64,
    pub fencing_token: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedTurnSupervisorLease {
    pub state: String,
    pub last_event_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamCursorRecord {
    pub stream_id: String,
    pub run_id: Option<String>,
    pub last_sequence: u64,
    pub terminal_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStreamMetadata {
    pub stream_id: String,
    pub run_id: String,
    pub project_id: String,
    pub conversation_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineProjectionReport {
    pub run_id: String,
    pub projected_count: usize,
    pub duplicate_count: usize,
    pub skipped_count: usize,
    pub last_sequence: u64,
    pub terminal_kind: Option<String>,
}

#[derive(Debug, Clone)]
struct TimelineRunAnchor {
    project_id: String,
    conversation_id: String,
    turn_id: String,
    message_id: String,
    message_sequence: i64,
    message_updated_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineProjectionMode {
    Incremental,
    Rebuild,
}

/// Redact secret-looking tokens from text before it is stored in the searchable
/// copy or an FTS index. Whitespace within a line is normalized; line breaks are
/// preserved. Catches common provider/key prefixes, long high-entropy tokens,
/// `KEY=VALUE` / `KEY: VALUE` secret values, and whole credential lines (private
/// key blocks, `Authorization: Bearer …`) — recovery directive §19.5.
pub fn redact_secrets(input: &str) -> String {
    input
        .lines()
        .map(|line| {
            if line_has_credential_marker(line) {
                return "[REDACTED]".to_string();
            }
            line.split_whitespace()
                .map(|tok| if looks_secret(tok) { "[REDACTED]" } else { tok })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whole-line markers that should redact the entire line regardless of token
/// shape: PEM private-key blocks and bearer auth headers.
fn line_has_credential_marker(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if lower.contains("-----begin") && lower.contains("key") {
        return true;
    }
    if lower.contains("private key") {
        return true;
    }
    if lower.contains("authorization") && lower.contains("bearer ") {
        return true;
    }
    false
}

fn looks_secret(token: &str) -> bool {
    if looks_secret_core(token) {
        return true;
    }
    // `KEY=VALUE` / `KEY:VALUE` (e.g. a `.env` assignment or `password: …`):
    // judge the value part after the last separator.
    for sep in ['=', ':'] {
        if let Some((_, value)) = token.rsplit_once(sep) {
            if !value.is_empty() && looks_secret_core(value) {
                return true;
            }
        }
    }
    // URL userinfo: `scheme://user:pass@host` — the embedded credential.
    if token.contains("://") && token.contains('@') && token.contains(':') {
        if let Some(rest) = token.split("://").nth(1) {
            if let Some((userinfo, _)) = rest.split_once('@') {
                if userinfo.contains(':') {
                    return true;
                }
            }
        }
    }
    false
}

fn looks_secret_core(token: &str) -> bool {
    let core = token.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    if core.len() < 12 {
        return false;
    }
    // Known provider/key prefixes (lowercased compare). `sk-` also covers
    // OpenRouter `sk-or-…` and OpenAI `sk-…`.
    const PREFIXES: [&str; 13] = [
        "sk-",
        "sk_",
        "ghp_",
        "gho_",
        "ghs_",
        "github_pat_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "aws_",
        "akia",
        "asia",
        "aiza",
    ];
    let lower = core.to_ascii_lowercase();
    if PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return true;
    }
    // Long, mixed alphanumeric high-entropy tokens.
    if core.len() >= 28
        && core
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && core.chars().any(|c| c.is_ascii_digit())
        && core.chars().any(|c| c.is_ascii_alphabetic())
    {
        return true;
    }
    false
}

fn enum_to_str<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn enum_from_str<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T> {
    serde_json::from_value(serde_json::Value::String(raw.to_string()))
        .map_err(|_| ConversationError::InvalidEnum(raw.to_string()))
}

fn run_projection_state_from_str(raw: &str) -> Result<RunProjectionState> {
    match raw {
        "queued" => Ok(RunProjectionState::Queued),
        "running" => Ok(RunProjectionState::Running),
        "paused" => Ok(RunProjectionState::Paused),
        "completed" => Ok(RunProjectionState::Completed),
        "failed" => Ok(RunProjectionState::Failed),
        "cancelled" => Ok(RunProjectionState::Cancelled),
        _ => Err(ConversationError::InvalidRunProjectionState(
            raw.to_string(),
        )),
    }
}

fn trim_timeline_items(items: Vec<TimelineItem>, limit: usize) -> Vec<TimelineItem> {
    if items.len() <= limit {
        return items;
    }

    let mut selected_ids = HashSet::new();
    let mut selected = Vec::with_capacity(limit);
    for preserved_kind in [
        TimelineItemKind::UserMessage,
        TimelineItemKind::AssistantMessage,
    ] {
        if selected.len() >= limit {
            break;
        }
        if let Some(item) = items
            .iter()
            .rev()
            .find(|item| item.kind == preserved_kind && !selected_ids.contains(&item.id))
        {
            selected_ids.insert(item.id.clone());
            selected.push(item.clone());
        }
    }

    for item in items.iter().rev() {
        if selected.len() >= limit {
            break;
        }
        if selected_ids.insert(item.id.clone()) {
            selected.push(item.clone());
        }
    }

    selected.sort_by(|lhs, rhs| {
        lhs.created_at_ms
            .cmp(&rhs.created_at_ms)
            .then(lhs.sequence.cmp(&rhs.sequence))
            .then(lhs.id.cmp(&rhs.id))
    });
    selected
}

fn conversation_status_for_projected_run_state(raw: &str) -> &'static str {
    match raw {
        "completed" => "completed",
        "failed" | "cancelled" => "failed",
        "queued" | "running" | "paused" => "running",
        _ => "running",
    }
}

fn sha256_v1(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    format!("sha256:v1:{hex}")
}

pub struct ConversationRepository {
    conn: Connection,
}

impl ConversationRepository {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let repo = Self { conn };
        repo.migrate()?;
        Ok(repo)
    }

    pub fn open_workspace(workspace: &Path) -> Result<Self> {
        Self::open(&workspace.join(CONVERSATION_DB_RELATIVE_PATH))
    }

    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let repo = Self { conn };
        repo.migrate()?;
        Ok(repo)
    }

    /// Idempotent migration. Safe to run on an empty DB or one that already holds
    /// other (e.g. engine) tables.
    pub fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY,
                workspace_key TEXT NOT NULL UNIQUE,
                display_name TEXT NOT NULL,
                last_conversation_id TEXT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS conversations (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                title TEXT NOT NULL,
                title_source TEXT NOT NULL CHECK(title_source IN ('generated','user','agent','imported')),
                status TEXT NOT NULL,
                pinned INTEGER NOT NULL DEFAULT 0,
                archived INTEGER NOT NULL DEFAULT 0,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                last_message_at_ms INTEGER NULL,
                version INTEGER NOT NULL DEFAULT 1
            );
            CREATE INDEX IF NOT EXISTS idx_conversations_project_updated
                ON conversations(project_id, archived, pinned DESC, updated_at_ms DESC);
            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                turn_id TEXT NOT NULL,
                role TEXT NOT NULL CHECK(role IN ('system','user','assistant','tool','event')),
                state TEXT NOT NULL CHECK(state IN ('draft','queued','streaming','complete','failed','cancelled')),
                content_redacted TEXT NOT NULL,
                content_ciphertext BLOB NULL,
                content_nonce BLOB NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                sequence INTEGER NOT NULL,
                UNIQUE(conversation_id, sequence)
            );
            CREATE INDEX IF NOT EXISTS idx_messages_conversation_sequence
                ON messages(conversation_id, sequence);
            CREATE TABLE IF NOT EXISTS conversation_runs (
                conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                message_id TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                relation TEXT NOT NULL CHECK(relation IN ('primary','child','retry','verification','design','image')),
                created_at_ms INTEGER NOT NULL,
                PRIMARY KEY(conversation_id, run_id)
            );
            CREATE TABLE IF NOT EXISTS turn_idempotency (
                idempotency_key TEXT NOT NULL,
                conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                turn_id TEXT NOT NULL,
                user_message_id TEXT NOT NULL,
                assistant_message_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                UNIQUE(conversation_id, idempotency_key)
            );
            CREATE TABLE IF NOT EXISTS message_attachments (
                message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                artifact_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                display_name TEXT NOT NULL,
                local_ref TEXT NOT NULL,
                PRIMARY KEY(message_id, artifact_id)
            );
            CREATE TABLE IF NOT EXISTS conversation_summaries (
                conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
                summary_redacted TEXT NOT NULL,
                source_message_sequence INTEGER NOT NULL,
                generated_at_ms INTEGER NOT NULL
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                conversation_id UNINDEXED,
                message_id UNINDEXED,
                content_redacted,
                tokenize='unicode61'
            );
            -- Recovery release §20: durable thread settings, typed timeline items,
            -- run projection (the real source of run state), and stream cursors.
            CREATE TABLE IF NOT EXISTS conversation_settings (
                conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
                settings_json TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS timeline_items (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                turn_id TEXT NULL,
                run_id TEXT NULL,
                sequence INTEGER NOT NULL,
                kind TEXT NOT NULL,
                state TEXT NOT NULL,
                payload_redacted_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                UNIQUE(conversation_id, sequence)
            );
            CREATE INDEX IF NOT EXISTS idx_timeline_conversation_sequence
                ON timeline_items(conversation_id, sequence);
            CREATE TABLE IF NOT EXISTS run_projections (
                run_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                conversation_id TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                state TEXT NOT NULL,
                pipeline_id TEXT NULL,
                graph_revision TEXT NULL,
                last_sequence INTEGER NOT NULL,
                projection_json TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_run_projections_conversation
                ON run_projections(conversation_id);
            CREATE TABLE IF NOT EXISTS stream_cursors (
                stream_id TEXT PRIMARY KEY,
                run_id TEXT NULL,
                last_sequence INTEGER NOT NULL,
                terminal_kind TEXT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS turns (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                client_turn_id TEXT NOT NULL,
                request_id TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                effective_settings_json TEXT NOT NULL,
                settings_digest TEXT NOT NULL,
                model_routing_decision_json TEXT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                UNIQUE(conversation_id, idempotency_key)
            );
            CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY,
                turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
                last_event_sequence INTEGER NOT NULL DEFAULT -1,
                lease_owner TEXT NULL,
                lease_expires_at_ms INTEGER NULL,
                fencing_token INTEGER NOT NULL DEFAULT 0,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            ",
        )?;
        self.ensure_turns_model_routing_column()?;
        self.ensure_runs_fencing_token_column()?;
        self.backfill_run_projections_from_legacy_lifecycle_state()?;
        self.drop_legacy_lifecycle_state_columns()?;
        self.repair_conversation_statuses()?;
        self.conn
            .pragma_update(None, "user_version", MIGRATION_VERSION)?;
        Ok(())
    }

    fn backfill_run_projections_from_legacy_lifecycle_state(&self) -> Result<()> {
        if !self.table_has_column("runs", "state")? {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO run_projections(
                 run_id, project_id, conversation_id, turn_id, state,
                 pipeline_id, graph_revision, last_sequence, projection_json, updated_at_ms)
             SELECT
                 r.id,
                 t.project_id,
                 t.conversation_id,
                 t.id,
                 r.state,
                 NULL,
                 NULL,
                 r.last_event_sequence,
                 '{}',
                 r.updated_at_ms
             FROM runs r
             JOIN turns t ON t.id = r.turn_id
             JOIN conversation_runs cr ON cr.turn_id = t.id AND cr.run_id = r.id
             WHERE r.state IN ('queued', 'running', 'paused', 'completed', 'failed', 'cancelled')
               AND NOT EXISTS (
                   SELECT 1 FROM run_projections rp WHERE rp.run_id = r.id
               )",
            [],
        )?;
        Ok(())
    }

    fn drop_legacy_lifecycle_state_columns(&self) -> Result<()> {
        if self.table_has_column("turns", "state")? {
            self.conn
                .execute("ALTER TABLE turns DROP COLUMN state", [])?;
        }
        if self.table_has_column("runs", "state")? {
            self.conn
                .execute("ALTER TABLE runs DROP COLUMN state", [])?;
        }
        Ok(())
    }

    fn table_has_column(&self, table: &str, column: &str) -> Result<bool> {
        let pragma = match table {
            "turns" => "PRAGMA table_info(turns)",
            "runs" => "PRAGMA table_info(runs)",
            _ => return Ok(false),
        };
        let mut stmt = self.conn.prepare(pragma)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn repair_conversation_statuses(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE conversations
             SET status = COALESCE((
                 SELECT CASE rp.state
                     WHEN 'completed' THEN 'completed'
                     WHEN 'failed' THEN 'failed'
                     WHEN 'cancelled' THEN 'failed'
                     WHEN 'paused' THEN 'running'
                     WHEN 'queued' THEN 'running'
                     WHEN 'running' THEN 'running'
                     ELSE 'idle'
                 END
                 FROM conversation_runs cr
                 JOIN run_projections rp ON rp.run_id = cr.run_id
                 WHERE cr.conversation_id = conversations.id
                 ORDER BY cr.created_at_ms DESC, cr.run_id DESC
                 LIMIT 1
             ), 'idle')
             WHERE archived = 0
               AND status = 'running'
               AND NOT EXISTS (
                   SELECT 1
                   FROM conversation_runs cr
                   JOIN run_projections rp ON rp.run_id = cr.run_id
                   WHERE cr.conversation_id = conversations.id
                     AND rp.state IN ('queued', 'running', 'paused')
               )",
            [],
        )?;
        Ok(())
    }

    fn ensure_turns_model_routing_column(&self) -> Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA table_info(turns)")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == "model_routing_decision_json" {
                return Ok(());
            }
        }
        self.conn.execute(
            "ALTER TABLE turns ADD COLUMN model_routing_decision_json TEXT NULL",
            [],
        )?;
        Ok(())
    }

    fn ensure_runs_fencing_token_column(&self) -> Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA table_info(runs)")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == "fencing_token" {
                return Ok(());
            }
        }
        self.conn.execute(
            "ALTER TABLE runs ADD COLUMN fencing_token INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
        Ok(())
    }

    fn new_id(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?)
    }

    /// Create (or return the existing) project for a workspace key.
    pub fn create_project(
        &self,
        workspace_key: &str,
        display_name: &str,
        now_ms: u64,
    ) -> Result<String> {
        if let Some(existing) = self.project_id_for_workspace(workspace_key)? {
            return Ok(existing);
        }
        let id = self.new_id()?;
        self.conn.execute(
            "INSERT INTO projects(id, workspace_key, display_name, last_conversation_id, created_at_ms, updated_at_ms)
             VALUES(?1, ?2, ?3, NULL, ?4, ?4)",
            params![id, workspace_key, display_name, now_ms as i64],
        )?;
        Ok(id)
    }

    fn project_id_for_workspace(&self, workspace_key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM projects WHERE workspace_key = ?1")?;
        let mut rows = stmt.query(params![workspace_key])?;
        Ok(rows.next()?.map(|r| r.get::<_, String>(0)).transpose()?)
    }

    pub fn create_conversation(
        &self,
        project_id: &str,
        title: &str,
        now_ms: u64,
    ) -> Result<String> {
        let id = self.new_id()?;
        self.conn.execute(
            "INSERT INTO conversations(id, project_id, title, title_source, status, pinned, archived, created_at_ms, updated_at_ms, last_message_at_ms, version)
             VALUES(?1, ?2, ?3, 'generated', 'idle', 0, 0, ?4, ?4, NULL, 1)",
            params![id, project_id, title, now_ms as i64],
        )?;
        self.conn.execute(
            "UPDATE projects SET last_conversation_id = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![id, now_ms as i64, project_id],
        )?;
        Ok(id)
    }

    pub fn get_conversation(&self, id: &str) -> Result<Option<ConversationSummary>> {
        let mut stmt = self.conn.prepare(SELECT_CONVERSATION_BASE)?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(conversation_from_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn list_conversations(
        &self,
        project_id: &str,
        filter: ConversationFilter,
        limit: usize,
    ) -> Result<Vec<ConversationSummary>> {
        let where_extra = match filter {
            ConversationFilter::All => "c.archived = 0",
            ConversationFilter::Running => "c.archived = 0 AND c.status = 'running'",
            ConversationFilter::Pinned => "c.archived = 0 AND c.pinned = 1",
            ConversationFilter::Archived => "c.archived = 1",
        };
        let sql = format!(
            "{SELECT_CONVERSATION_LIST_PREFIX} WHERE c.project_id = ?1 AND {where_extra}
             ORDER BY c.pinned DESC, c.updated_at_ms DESC LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![project_id, limit as i64])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(conversation_from_row(row)?);
        }
        Ok(out)
    }

    pub fn rename_conversation(&self, id: &str, title: &str, now_ms: u64) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE conversations SET title = ?1, title_source = 'user', updated_at_ms = ?2 WHERE id = ?3",
            params![title, now_ms as i64, id],
        )?;
        if n == 0 {
            return Err(ConversationError::NotFound(id.to_string()));
        }
        Ok(())
    }

    pub fn set_pinned(&self, id: &str, pinned: bool, now_ms: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE conversations SET pinned = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![pinned as i64, now_ms as i64, id],
        )?;
        Ok(())
    }

    pub fn set_archived(&self, id: &str, archived: bool, now_ms: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE conversations SET archived = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![archived as i64, now_ms as i64, id],
        )?;
        Ok(())
    }

    pub fn set_status(&self, id: &str, status: ConversationStatus, now_ms: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE conversations SET status = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![enum_to_str(&status), now_ms as i64, id],
        )?;
        Ok(())
    }

    pub fn delete_conversation(&self, id: &str) -> Result<ConversationDeleteCounts> {
        let messages: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE conversation_id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        let runs: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM conversation_runs WHERE conversation_id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "DELETE FROM messages_fts WHERE conversation_id = ?1",
            params![id],
        )?;
        let removed = self
            .conn
            .execute("DELETE FROM conversations WHERE id = ?1", params![id])?;
        if removed == 0 {
            return Err(ConversationError::NotFound(id.to_string()));
        }
        Ok(ConversationDeleteCounts {
            messages: messages as u64,
            runs: runs as u64,
        })
    }

    pub fn fork_conversation(
        &self,
        source_id: &str,
        after_sequence: Option<i64>,
        now_ms: u64,
    ) -> Result<String> {
        let (project_id, title): (String, String) = self.conn.query_row(
            "SELECT project_id, title FROM conversations WHERE id = ?1",
            params![source_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let new_id = self.create_conversation(&project_id, &format!("{title} (fork)"), now_ms)?;
        let cutoff = after_sequence.unwrap_or(i64::MAX);

        let mut stmt = self.conn.prepare(
            "SELECT turn_id, role, state, content_redacted, sequence, created_at_ms
             FROM messages WHERE conversation_id = ?1 AND sequence <= ?2 ORDER BY sequence ASC",
        )?;
        let copied: Vec<(String, String, String, String, i64, i64)> = stmt
            .query_map(params![source_id, cutoff], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;

        for (turn_id, role, state, content, sequence, created) in copied {
            let mid = self.new_id()?;
            self.conn.execute(
                "INSERT INTO messages(id, project_id, conversation_id, turn_id, role, state, content_redacted, created_at_ms, updated_at_ms, sequence)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9)",
                params![mid, project_id, new_id, turn_id, role, state, content, created, sequence],
            )?;
            self.conn.execute(
                "INSERT INTO messages_fts(conversation_id, message_id, content_redacted) VALUES(?1, ?2, ?3)",
                params![new_id, mid, content],
            )?;
        }
        Ok(new_id)
    }

    /// Append a message. `content_raw` is redacted before storage; only the
    /// redacted copy is indexed for search.
    #[allow(clippy::too_many_arguments)]
    pub fn append_message(
        &self,
        project_id: &str,
        conversation_id: &str,
        turn_id: &str,
        role: MessageRole,
        state: MessageState,
        content_raw: &str,
        now_ms: u64,
    ) -> Result<String> {
        self.append_message_with_raw_ciphertext(
            project_id,
            conversation_id,
            turn_id,
            role,
            state,
            content_raw,
            None,
            now_ms,
        )
    }

    /// Append a message with an optional encrypted raw-content payload. The
    /// searchable/read-model copy remains redacted; ciphertext is returned only
    /// through explicit ciphertext APIs.
    #[allow(clippy::too_many_arguments)]
    pub fn append_message_with_raw_ciphertext(
        &self,
        project_id: &str,
        conversation_id: &str,
        turn_id: &str,
        role: MessageRole,
        state: MessageState,
        content_raw: &str,
        raw_content: Option<&MessageRawContentCiphertext>,
        now_ms: u64,
    ) -> Result<String> {
        let content_redacted = redact_secrets(content_raw);
        let id = self.new_id()?;
        let sequence: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM messages WHERE conversation_id = ?1",
            params![conversation_id],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO messages(id, project_id, conversation_id, turn_id, role, state, content_redacted, content_ciphertext, content_nonce, created_at_ms, updated_at_ms, sequence)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11)",
            params![
                id,
                project_id,
                conversation_id,
                turn_id,
                enum_to_str(&role),
                enum_to_str(&state),
                content_redacted,
                raw_content.map(|content| content.ciphertext.as_slice()),
                raw_content.map(|content| content.nonce.as_slice()),
                now_ms as i64,
                sequence
            ],
        )?;
        self.conn.execute(
            "INSERT INTO messages_fts(conversation_id, message_id, content_redacted) VALUES(?1, ?2, ?3)",
            params![conversation_id, id, content_redacted],
        )?;
        self.conn.execute(
            "UPDATE conversations SET last_message_at_ms = ?1, updated_at_ms = ?1 WHERE id = ?2",
            params![now_ms as i64, conversation_id],
        )?;
        Ok(id)
    }

    /// Generate a fresh random id using the same scheme as message/conversation
    /// ids. Useful for callers that need a `turn_id` before any rows exist.
    pub fn new_turn_id(&self) -> Result<String> {
        self.new_id()
    }

    /// Overwrite a message's stored content (redacted) and state by id. Used to
    /// finalize an assistant placeholder once a run produces its result.
    pub fn set_message_content(
        &self,
        message_id: &str,
        content_raw: &str,
        state: MessageState,
        now_ms: u64,
    ) -> Result<()> {
        self.set_message_content_with_raw_ciphertext(message_id, content_raw, state, None, now_ms)
    }

    /// Replace message content and optional encrypted raw-content payload.
    /// Passing `None` clears any prior ciphertext so stale raw payloads cannot
    /// survive a redacted content update.
    pub fn set_message_content_with_raw_ciphertext(
        &self,
        message_id: &str,
        content_raw: &str,
        state: MessageState,
        raw_content: Option<&MessageRawContentCiphertext>,
        now_ms: u64,
    ) -> Result<()> {
        let content_redacted = redact_secrets(content_raw);
        let n = self.conn.execute(
            "UPDATE messages SET content_redacted = ?1, state = ?2, content_ciphertext = ?3, content_nonce = ?4, updated_at_ms = ?5 WHERE id = ?6",
            params![
                content_redacted,
                enum_to_str(&state),
                raw_content.map(|content| content.ciphertext.as_slice()),
                raw_content.map(|content| content.nonce.as_slice()),
                now_ms as i64,
                message_id
            ],
        )?;
        if n == 0 {
            return Err(ConversationError::NotFound(message_id.to_string()));
        }
        self.conn.execute(
            "UPDATE messages_fts SET content_redacted = ?1 WHERE message_id = ?2",
            params![content_redacted, message_id],
        )?;
        Ok(())
    }

    /// Fetch encrypted raw-content payload for a message, when present. This is
    /// deliberately separate from message pagination/search/digest APIs.
    pub fn message_raw_content_ciphertext(
        &self,
        message_id: &str,
    ) -> Result<Option<MessageRawContentCiphertext>> {
        let mut stmt = self
            .conn
            .prepare("SELECT content_ciphertext, content_nonce FROM messages WHERE id = ?1")?;
        let mut rows = stmt.query(params![message_id])?;
        match rows.next()? {
            Some(row) => {
                let ciphertext: Option<Vec<u8>> = row.get(0)?;
                let nonce: Option<Vec<u8>> = row.get(1)?;
                Ok(ciphertext.map(|ciphertext| MessageRawContentCiphertext {
                    ciphertext,
                    nonce: nonce.unwrap_or_default(),
                }))
            }
            None => Ok(None),
        }
    }

    /// Fetch encrypted raw-content payload for the first user message in a turn.
    pub fn turn_user_message_raw_content_ciphertext(
        &self,
        turn_id: &str,
    ) -> Result<Option<MessageRawContentCiphertext>> {
        let mut stmt = self.conn.prepare(
            "SELECT content_ciphertext, content_nonce FROM messages
             WHERE turn_id = ?1 AND role = 'user'
             ORDER BY sequence ASC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![turn_id])?;
        match rows.next()? {
            Some(row) => {
                let ciphertext: Option<Vec<u8>> = row.get(0)?;
                let nonce: Option<Vec<u8>> = row.get(1)?;
                Ok(ciphertext.map(|ciphertext| MessageRawContentCiphertext {
                    ciphertext,
                    nonce: nonce.unwrap_or_default(),
                }))
            }
            None => Ok(None),
        }
    }

    /// Link a run to a conversation/message/turn in the `conversation_runs`
    /// table. `relation` must be one of the CHECK-constrained relation kinds.
    pub fn link_run(
        &self,
        conversation_id: &str,
        message_id: &str,
        turn_id: &str,
        run_id: &str,
        relation: &str,
        now_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO conversation_runs(conversation_id, message_id, turn_id, run_id, relation, created_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![conversation_id, message_id, turn_id, run_id, relation, now_ms as i64],
        )?;
        Ok(())
    }

    /// List the runs linked to a conversation, oldest first. The run state is
    /// joined from `run_projections`; a run with no projection yields
    /// `run_state = None` (surfaced as `unknown`), never a fabricated state.
    pub fn runs_for_conversation(&self, conversation_id: &str) -> Result<Vec<ConversationRunRef>> {
        let mut stmt = self.conn.prepare(
            "SELECT cr.turn_id, cr.run_id, cr.message_id, cr.relation, rp.state
             FROM conversation_runs cr
             LEFT JOIN run_projections rp ON rp.run_id = cr.run_id
             WHERE cr.conversation_id = ?1
             ORDER BY cr.created_at_ms ASC, cr.run_id ASC",
        )?;
        let rows = stmt.query_map(params![conversation_id], |row| {
            Ok(ConversationRunRef {
                turn_id: row.get(0)?,
                run_id: row.get(1)?,
                message_id: row.get(2)?,
                relation: row.get(3)?,
                run_state: row.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Replay the current conversation read models as typed timeline items. This
    /// bridges existing durable messages/run refs into the PR-068 timeline
    /// contract before every event source has a first-class reducer.
    pub fn timeline_items_for_conversation(
        &self,
        conversation_id: &str,
        limit: usize,
    ) -> Result<Vec<TimelineItem>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let messages = self.message_page(conversation_id, None, limit)?;
        let runs_by_message: HashMap<String, ConversationRunRef> = self
            .runs_for_conversation(conversation_id)?
            .into_iter()
            .map(|run| (run.message_id.clone(), run))
            .collect();
        let persisted = self.persisted_timeline_items_for_conversation(conversation_id, limit)?;
        let mut items = Vec::with_capacity(messages.len() + persisted.len());
        for message in messages {
            if message.role == MessageRole::Event
                && message
                    .content_redacted
                    .starts_with(GIT_RECEIPT_ANCHOR_PREFIX)
            {
                continue;
            }
            let run = runs_by_message.get(&message.id);
            let kind = match message.role {
                MessageRole::User => TimelineItemKind::UserMessage,
                MessageRole::Assistant => TimelineItemKind::AssistantMessage,
                MessageRole::Tool => TimelineItemKind::ToolCall,
                MessageRole::Event | MessageRole::System => TimelineItemKind::Warning,
            };
            let message_state = enum_to_str(&message.state);
            let message_role = enum_to_str(&message.role);
            let state = if let Some(run) = run {
                run.run_state
                    .clone()
                    .unwrap_or_else(|| message_state.clone())
            } else {
                message_state.clone()
            };
            items.push(TimelineItem {
                schema: TIMELINE_ITEM_SCHEMA.to_string(),
                id: format!("timeline-{}", message.id),
                project_id: message.project_id.clone(),
                conversation_id: message.conversation_id.clone(),
                turn_id: (!message.turn_id.is_empty()).then(|| message.turn_id.clone()),
                run_id: run.map(|run| run.run_id.clone()),
                sequence: message.sequence,
                kind,
                state,
                payload: serde_json::json!({
                    "message_id": message.id,
                    "role": message_role,
                    "message_state": message_state,
                    "content_redacted": message.content_redacted,
                    "run_relation": run.map(|run| run.relation.as_str()),
                }),
                created_at_ms: message.created_at_ms,
                updated_at_ms: message.updated_at_ms,
            });
        }
        items.extend(persisted);
        items.sort_by(|lhs, rhs| {
            lhs.created_at_ms
                .cmp(&rhs.created_at_ms)
                .then(lhs.sequence.cmp(&rhs.sequence))
                .then(lhs.id.cmp(&rhs.id))
        });
        Ok(trim_timeline_items(items, limit))
    }

    pub fn append_timeline_item(
        &self,
        project_id: &str,
        conversation_id: &str,
        kind: TimelineItemKind,
        state: &str,
        payload: serde_json::Value,
        now_ms: u64,
    ) -> Result<TimelineItem> {
        if self.get_conversation(conversation_id)?.is_none() {
            return Err(ConversationError::NotFound(conversation_id.to_string()));
        }
        let sequence: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1
             FROM (
               SELECT sequence FROM messages WHERE conversation_id = ?1
               UNION ALL
               SELECT sequence FROM timeline_items WHERE conversation_id = ?1
             )",
            params![conversation_id],
            |row| row.get(0),
        )?;
        let id = format!("timeline-{}", self.new_id()?);
        let payload_json = serde_json::to_string(&payload)?;
        self.conn.execute(
            "INSERT INTO timeline_items(
                id, project_id, conversation_id, turn_id, run_id, sequence,
                kind, state, payload_redacted_json, created_at_ms, updated_at_ms
             ) VALUES(?1, ?2, ?3, NULL, NULL, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                id,
                project_id,
                conversation_id,
                sequence,
                enum_to_str(&kind),
                state,
                payload_json,
                now_ms as i64,
            ],
        )?;
        self.conn.execute(
            "UPDATE conversations SET updated_at_ms = ?1 WHERE id = ?2",
            params![now_ms as i64, conversation_id],
        )?;
        Ok(TimelineItem {
            schema: TIMELINE_ITEM_SCHEMA.to_string(),
            id,
            project_id: project_id.to_string(),
            conversation_id: conversation_id.to_string(),
            turn_id: None,
            run_id: None,
            sequence,
            kind,
            state: state.to_string(),
            payload,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        })
    }

    /// Materialize durable execution events into the conversation timeline read
    /// model for a run. Replaying the same event batch is idempotent: timeline
    /// rows use the execution event id as their stable identity, and duplicate
    /// rows are counted rather than appended again.
    pub fn project_execution_events_into_timeline(
        &self,
        run_id: &str,
        events: &[ExecutionEventEnvelope],
        now_ms: u64,
    ) -> Result<TimelineProjectionReport> {
        self.project_execution_events_into_timeline_with_mode(
            run_id,
            events,
            now_ms,
            TimelineProjectionMode::Incremental,
        )
    }

    /// Rebuild event-derived conversation read models for a run from a full
    /// replay. Unlike incremental projection, this starts from the accepted
    /// turn's initial queued state instead of trusting stale projection rows.
    pub fn rebuild_execution_events_into_timeline(
        &self,
        run_id: &str,
        events: &[ExecutionEventEnvelope],
        now_ms: u64,
    ) -> Result<TimelineProjectionReport> {
        self.project_execution_events_into_timeline_with_mode(
            run_id,
            events,
            now_ms,
            TimelineProjectionMode::Rebuild,
        )
    }

    fn project_execution_events_into_timeline_with_mode(
        &self,
        run_id: &str,
        events: &[ExecutionEventEnvelope],
        now_ms: u64,
        mode: TimelineProjectionMode,
    ) -> Result<TimelineProjectionReport> {
        let Some(anchor) = self.timeline_anchor_for_run(run_id)? else {
            return Err(ConversationError::NotFound(run_id.to_string()));
        };
        let mut ordered_events: Vec<&ExecutionEventEnvelope> = events
            .iter()
            .filter(|event| event.run_id == run_id)
            .collect();
        ordered_events.sort_by(|left, right| {
            left.sequence
                .cmp(&right.sequence)
                .then(left.id.cmp(&right.id))
        });

        let mut projected_count = 0usize;
        let mut duplicate_count = 0usize;
        let mut skipped_count = events.len().saturating_sub(ordered_events.len());
        let mut last_sequence = 0u64;
        let mut terminal_kind = None;

        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            let mut projected_run_state = match mode {
                TimelineProjectionMode::Incremental => {
                    let stored_run_state = self
                        .run_projection_state(run_id)?
                        .ok_or_else(|| ConversationError::ProjectionMissing(run_id.to_string()))?;
                    run_projection_state_from_str(&stored_run_state)?
                }
                TimelineProjectionMode::Rebuild => RunProjectionState::Queued,
            };
            if mode == TimelineProjectionMode::Rebuild {
                self.conn.execute(
                    "DELETE FROM timeline_items
                     WHERE run_id = ?1
                       AND id LIKE 'timeline-event-%'",
                    params![run_id],
                )?;
            }
            for event in ordered_events {
                last_sequence = last_sequence.max(event.sequence);
                if let Some(kind) = terminal_kind_for_execution_event(event) {
                    terminal_kind = Some(kind.to_string());
                }
                if event.sequence == 0 {
                    skipped_count += 1;
                    continue;
                }
                if let Some(next_state) = run_projection_state_for_execution_event(event) {
                    projected_run_state =
                        merge_run_projection_state(projected_run_state, next_state);
                }
                let item = execution_event_timeline_item(&anchor, event);
                let payload_json = serde_json::to_string(&item.payload)?;
                let inserted = self.conn.execute(
                    "INSERT OR IGNORE INTO timeline_items(
                        id, project_id, conversation_id, turn_id, run_id, sequence,
                        kind, state, payload_redacted_json, created_at_ms, updated_at_ms
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![
                        item.id,
                        item.project_id,
                        item.conversation_id,
                        item.turn_id,
                        item.run_id,
                        item.sequence,
                        enum_to_str(&item.kind),
                        item.state,
                        payload_json,
                        item.created_at_ms as i64,
                        item.updated_at_ms as i64,
                    ],
                )?;
                if inserted == 0 {
                    duplicate_count += 1;
                } else {
                    projected_count += 1;
                }
            }

            if last_sequence > 0 || mode == TimelineProjectionMode::Rebuild {
                match mode {
                    TimelineProjectionMode::Incremental => {
                        self.conn.execute(
                            "UPDATE stream_cursors
                             SET last_sequence = MAX(last_sequence, ?1),
                                 terminal_kind = COALESCE(terminal_kind, ?2),
                                 updated_at_ms = ?3
                             WHERE run_id = ?4",
                            params![
                                last_sequence as i64,
                                terminal_kind.as_deref(),
                                now_ms as i64,
                                run_id
                            ],
                        )?;
                        self.conn.execute(
                            "UPDATE run_projections
                             SET state = ?1,
                                 last_sequence = MAX(last_sequence, ?2),
                                 updated_at_ms = ?3
                             WHERE run_id = ?4",
                            params![
                                run_projection_state_to_str(projected_run_state),
                                last_sequence as i64,
                                now_ms as i64,
                                run_id
                            ],
                        )?;
                    }
                    TimelineProjectionMode::Rebuild => {
                        self.conn.execute(
                            "UPDATE stream_cursors
                             SET last_sequence = ?1,
                                 terminal_kind = ?2,
                                 updated_at_ms = ?3
                             WHERE run_id = ?4",
                            params![
                                last_sequence as i64,
                                terminal_kind.as_deref(),
                                now_ms as i64,
                                run_id
                            ],
                        )?;
                        self.conn.execute(
                            "UPDATE run_projections
                             SET state = ?1,
                                 last_sequence = ?2,
                                 updated_at_ms = ?3
                             WHERE run_id = ?4",
                            params![
                                run_projection_state_to_str(projected_run_state),
                                last_sequence as i64,
                                now_ms as i64,
                                run_id
                            ],
                        )?;
                    }
                }
                self.refresh_conversation_status_from_run_projections(
                    &anchor.conversation_id,
                    now_ms,
                )?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => self.conn.execute_batch("COMMIT")?,
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        }

        Ok(TimelineProjectionReport {
            run_id: run_id.to_string(),
            projected_count,
            duplicate_count,
            skipped_count,
            last_sequence,
            terminal_kind,
        })
    }

    fn refresh_conversation_status_from_run_projections(
        &self,
        conversation_id: &str,
        now_ms: u64,
    ) -> Result<()> {
        let active_runs: i64 = self.conn.query_row(
            "SELECT COUNT(*)
             FROM conversation_runs cr
             JOIN run_projections rp ON rp.run_id = cr.run_id
             WHERE cr.conversation_id = ?1
               AND rp.state IN ('queued', 'running', 'paused')",
            params![conversation_id],
            |row| row.get(0),
        )?;
        let status = if active_runs > 0 {
            "running".to_string()
        } else {
            self.conn
                .query_row(
                    "SELECT rp.state
                     FROM conversation_runs cr
                     JOIN run_projections rp ON rp.run_id = cr.run_id
                     WHERE cr.conversation_id = ?1
                     ORDER BY cr.created_at_ms DESC, cr.run_id DESC
                     LIMIT 1",
                    params![conversation_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|state| conversation_status_for_projected_run_state(&state).to_string())
                .unwrap_or_else(|| "idle".to_string())
        };
        self.conn.execute(
            "UPDATE conversations
             SET status = ?1,
                 updated_at_ms = ?2
             WHERE id = ?3
               AND archived = 0",
            params![status, now_ms as i64, conversation_id],
        )?;
        Ok(())
    }

    pub fn stream_cursor_for_run(&self, run_id: &str) -> Result<Option<StreamCursorRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT stream_id, run_id, last_sequence, terminal_kind
             FROM stream_cursors
             WHERE run_id = ?1",
        )?;
        let mut rows = stmt.query(params![run_id])?;
        Ok(rows
            .next()?
            .map(|row| {
                Ok::<_, rusqlite::Error>(StreamCursorRecord {
                    stream_id: row.get(0)?,
                    run_id: row.get(1)?,
                    last_sequence: row.get::<_, i64>(2)? as u64,
                    terminal_kind: row.get(3)?,
                })
            })
            .transpose()?)
    }

    pub fn stream_metadata_for_run(&self, run_id: &str) -> Result<Option<RunStreamMetadata>> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(sc.stream_id, 'stream-' || cr.turn_id),
                    cr.run_id,
                    COALESCE(rp.project_id, m.project_id),
                    cr.conversation_id,
                    cr.turn_id
             FROM conversation_runs cr
             JOIN messages m ON m.id = cr.message_id
             LEFT JOIN run_projections rp ON rp.run_id = cr.run_id
             LEFT JOIN stream_cursors sc ON sc.run_id = cr.run_id
             WHERE cr.run_id = ?1
             ORDER BY cr.created_at_ms ASC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![run_id])?;
        Ok(rows
            .next()?
            .map(|row| {
                Ok::<_, rusqlite::Error>(RunStreamMetadata {
                    stream_id: row.get(0)?,
                    run_id: row.get(1)?,
                    project_id: row.get(2)?,
                    conversation_id: row.get(3)?,
                    turn_id: row.get(4)?,
                })
            })
            .transpose()?)
    }

    fn timeline_anchor_for_run(&self, run_id: &str) -> Result<Option<TimelineRunAnchor>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.project_id, cr.conversation_id, cr.turn_id, m.id,
                    m.sequence, m.updated_at_ms
             FROM conversation_runs cr
             JOIN messages m ON m.id = cr.message_id
             WHERE cr.run_id = ?1
             ORDER BY cr.created_at_ms ASC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![run_id])?;
        Ok(rows
            .next()?
            .map(|row| {
                Ok::<_, rusqlite::Error>(TimelineRunAnchor {
                    project_id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    turn_id: row.get(2)?,
                    message_id: row.get(3)?,
                    message_sequence: row.get(4)?,
                    message_updated_at_ms: row.get::<_, i64>(5)? as u64,
                })
            })
            .transpose()?)
    }

    fn persisted_timeline_items_for_conversation(
        &self,
        conversation_id: &str,
        limit: usize,
    ) -> Result<Vec<TimelineItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, conversation_id, turn_id, run_id, sequence,
                    kind, state, payload_redacted_json, created_at_ms, updated_at_ms
             FROM timeline_items
             WHERE conversation_id = ?1
             ORDER BY sequence DESC, id DESC
             LIMIT ?2",
        )?;
        let mut rows = stmt.query(params![conversation_id, limit as i64])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(timeline_item_from_row(row)?);
        }
        Ok(out)
    }

    /// Record (or update) the real lifecycle state of a run. This is the single
    /// source of truth the runs list and idempotent replay read from; nothing
    /// may report a run's state without a projection row behind it.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_run_projection(
        &self,
        run_id: &str,
        project_id: &str,
        conversation_id: &str,
        turn_id: &str,
        state: &str,
        now_ms: u64,
    ) -> Result<()> {
        self.upsert_run_projection_with_last_sequence(
            run_id,
            project_id,
            conversation_id,
            turn_id,
            state,
            0,
            now_ms,
        )
    }

    /// Record (or update) run lifecycle state with the last durable execution
    /// event sequence known to the caller. The event journal itself is owned by
    /// `opensks-event-store`; this table is a read model for conversation lists.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_run_projection_with_last_sequence(
        &self,
        run_id: &str,
        project_id: &str,
        conversation_id: &str,
        turn_id: &str,
        state: &str,
        last_event_sequence: u64,
        now_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO run_projections(
                 run_id, project_id, conversation_id, turn_id, state,
                 pipeline_id, graph_revision, last_sequence, projection_json, updated_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, NULL, NULL, ?6, '{}', ?7)
             ON CONFLICT(run_id) DO UPDATE SET
                 state = excluded.state,
                 last_sequence = excluded.last_sequence,
                 updated_at_ms = excluded.updated_at_ms",
            params![
                run_id,
                project_id,
                conversation_id,
                turn_id,
                state,
                last_event_sequence as i64,
                now_ms as i64
            ],
        )?;
        Ok(())
    }

    /// Ensure a non-chat execution event has a durable conversation anchor so it
    /// can be replayed through the same event-journal timeline projector as
    /// runtime runs. Used for Git Studio commit/push receipts, where the git
    /// effect happens outside a chat turn but still belongs in the active
    /// conversation's audit timeline.
    pub fn ensure_external_execution_event_anchor(
        &self,
        project_id: &str,
        conversation_id: &str,
        turn_id: &str,
        run_id: &str,
        anchor_message: &str,
        now_ms: u64,
    ) -> Result<String> {
        if let Some(existing) = self.existing_message_for_run(conversation_id, run_id)? {
            return Ok(existing);
        }
        let message_id = self.new_id()?;
        let stream_id = format!("stream-{turn_id}");
        let effective_settings = self.effective_turn_settings_for_accept(conversation_id)?;
        let effective_settings_json = serde_json::to_string(&effective_settings)?;
        let settings_digest = sha256_v1(&effective_settings_json);
        let content_redacted = redact_secrets(anchor_message);
        let sequence: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM messages WHERE conversation_id = ?1",
            params![conversation_id],
            |r| r.get(0),
        )?;

        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            if let Some(existing) = self.existing_message_for_run(conversation_id, run_id)? {
                return Err(ConversationError::DuplicateAnchor(existing));
            }
            self.conn.execute(
                "INSERT INTO messages(id, project_id, conversation_id, turn_id, role, state, content_redacted, created_at_ms, updated_at_ms, sequence)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9)",
                params![
                    message_id,
                    project_id,
                    conversation_id,
                    turn_id,
                    enum_to_str(&MessageRole::Event),
                    enum_to_str(&MessageState::Complete),
                    content_redacted,
                    now_ms as i64,
                    sequence,
                ],
            )?;
            self.conn.execute(
                "INSERT INTO messages_fts(conversation_id, message_id, content_redacted) VALUES(?1, ?2, ?3)",
                params![conversation_id, message_id, content_redacted],
            )?;
            self.conn.execute(
                "INSERT INTO turns(
                     id, project_id, conversation_id, client_turn_id, request_id,
                     idempotency_key, effective_settings_json, settings_digest,
                     model_routing_decision_json, created_at_ms, updated_at_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?9)",
                params![
                    turn_id,
                    project_id,
                    conversation_id,
                    turn_id,
                    run_id,
                    run_id,
                    effective_settings_json,
                    settings_digest,
                    now_ms as i64,
                ],
            )?;
            self.conn.execute(
                "INSERT INTO runs(
                     id, turn_id, last_event_sequence, lease_owner,
                     lease_expires_at_ms, fencing_token, created_at_ms, updated_at_ms)
                 VALUES(?1, ?2, 0, NULL, NULL, 0, ?3, ?3)",
                params![run_id, turn_id, now_ms as i64],
            )?;
            self.conn.execute(
                "INSERT INTO conversation_runs(conversation_id, message_id, turn_id, run_id, relation, created_at_ms)
                 VALUES(?1, ?2, ?3, ?4, 'primary', ?5)",
                params![conversation_id, message_id, turn_id, run_id, now_ms as i64],
            )?;
            self.conn.execute(
                "INSERT INTO run_projections(
                     run_id, project_id, conversation_id, turn_id, state,
                     pipeline_id, graph_revision, last_sequence, projection_json, updated_at_ms)
                 VALUES(?1, ?2, ?3, ?4, 'completed', ?5, ?6, 0, '{}', ?7)",
                params![
                    run_id,
                    project_id,
                    conversation_id,
                    turn_id,
                    effective_settings.pipeline_id,
                    effective_settings.graph_revision,
                    now_ms as i64,
                ],
            )?;
            self.conn.execute(
                "INSERT INTO stream_cursors(stream_id, run_id, last_sequence, terminal_kind, updated_at_ms)
                 VALUES(?1, ?2, 0, 'completed', ?3)",
                params![stream_id, run_id, now_ms as i64],
            )?;
            self.conn.execute(
                "UPDATE conversations
                 SET last_message_at_ms = ?1,
                     updated_at_ms = ?1
                 WHERE id = ?2",
                params![now_ms as i64, conversation_id],
            )?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(message_id)
            }
            Err(ConversationError::DuplicateAnchor(existing)) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Ok(existing)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    fn existing_message_for_run(
        &self,
        conversation_id: &str,
        run_id: &str,
    ) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT message_id FROM conversation_runs
             WHERE conversation_id = ?1 AND run_id = ?2
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![conversation_id, run_id])?;
        Ok(rows
            .next()?
            .map(|row| row.get::<_, String>(0))
            .transpose()?)
    }

    /// Real state of a single run, or `None` when no projection exists.
    pub fn run_projection_state(&self, run_id: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT state FROM run_projections WHERE run_id = ?1")?;
        let mut rows = stmt.query(params![run_id])?;
        Ok(rows.next()?.map(|r| r.get::<_, String>(0)).transpose()?)
    }

    /// Durable per-thread settings JSON, or `None` when never set (the caller
    /// applies its own default). Holds no secrets — only ids/refs.
    pub fn get_thread_settings(&self, conversation_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT settings_json FROM conversation_settings WHERE conversation_id = ?1",
        )?;
        let mut rows = stmt.query(params![conversation_id])?;
        Ok(rows.next()?.map(|r| r.get::<_, String>(0)).transpose()?)
    }

    /// Upsert durable per-thread settings (so model/mode/pipeline selections
    /// survive relaunch — directive §5.7 / PR-048).
    pub fn set_thread_settings(
        &self,
        conversation_id: &str,
        settings_json: &str,
        now_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO conversation_settings(conversation_id, settings_json, updated_at_ms)
             VALUES(?1, ?2, ?3)
             ON CONFLICT(conversation_id) DO UPDATE SET
                 settings_json = excluded.settings_json,
                 updated_at_ms = excluded.updated_at_ms",
            params![conversation_id, settings_json, now_ms as i64],
        )?;
        Ok(())
    }

    /// Record the identifiers produced for a turn under an idempotency key so a
    /// repeated `turn-start` with the same key can be replayed without a new run.
    pub fn record_turn_idempotency(
        &self,
        idempotency_key: &str,
        conversation_id: &str,
        record: &TurnIdempotencyRecord,
        now_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO turn_idempotency(idempotency_key, conversation_id, turn_id, user_message_id, assistant_message_id, run_id, created_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                idempotency_key,
                conversation_id,
                record.turn_id,
                record.user_message_id,
                record.assistant_message_id,
                record.run_id,
                now_ms as i64
            ],
        )?;
        Ok(())
    }

    /// Record the durable v2 turn/run accept snapshot. The old CLI shim calls
    /// this before/around execution so crash recovery can see the accepted
    /// identity even while the synchronous compatibility response still exists.
    pub fn record_turn_run_snapshot(&self, snapshot: TurnRunSnapshot<'_>) -> Result<()> {
        self.conn.execute(
                "INSERT INTO turns(
                     id, project_id, conversation_id, client_turn_id, request_id,
                 idempotency_key, effective_settings_json, settings_digest,
                 model_routing_decision_json, created_at_ms, updated_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
             ON CONFLICT(conversation_id, idempotency_key) DO UPDATE SET
                 model_routing_decision_json = COALESCE(turns.model_routing_decision_json, excluded.model_routing_decision_json),
                 updated_at_ms = excluded.updated_at_ms",
            params![
                snapshot.turn_id,
                snapshot.project_id,
                snapshot.conversation_id,
                snapshot.client_turn_id,
                snapshot.request_id,
                snapshot.idempotency_key,
                snapshot.effective_settings_json,
                snapshot.settings_digest,
                snapshot.model_routing_decision_json,
                snapshot.now_ms as i64,
            ],
        )?;
        self.conn.execute(
            "INSERT INTO runs(
                 id, turn_id, last_event_sequence, lease_owner,
                 lease_expires_at_ms, fencing_token, created_at_ms, updated_at_ms)
             VALUES(?1, ?2, -1, NULL, NULL, 0, ?3, ?3)
             ON CONFLICT(id) DO UPDATE SET
                 updated_at_ms = excluded.updated_at_ms",
            params![snapshot.run_id, snapshot.turn_id, snapshot.now_ms as i64,],
        )?;
        Ok(())
    }

    pub fn set_turn_run_state(
        &self,
        turn_id: &str,
        run_id: &str,
        state: &str,
        now_ms: u64,
    ) -> Result<()> {
        self.set_turn_run_state_with_last_sequence(turn_id, run_id, state, None, now_ms)
    }

    pub fn set_turn_run_state_with_last_sequence(
        &self,
        turn_id: &str,
        run_id: &str,
        state: &str,
        last_event_sequence: Option<u64>,
        now_ms: u64,
    ) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            self.conn.execute(
                "UPDATE turns SET updated_at_ms = ?1 WHERE id = ?2",
                params![now_ms as i64, turn_id],
            )?;
            match last_event_sequence {
                Some(last_event_sequence) => {
                    self.conn.execute(
                        "UPDATE runs
                         SET last_event_sequence = MAX(last_event_sequence, ?1),
                             updated_at_ms = ?2
                         WHERE id = ?3",
                        params![last_event_sequence as i64, now_ms as i64, run_id],
                    )?;
                    self.conn.execute(
                        "UPDATE run_projections
                         SET state = ?1,
                             last_sequence = MAX(last_sequence, ?2),
                             updated_at_ms = ?3
                         WHERE run_id = ?4",
                        params![state, last_event_sequence as i64, now_ms as i64, run_id],
                    )?;
                }
                None => {
                    self.conn.execute(
                        "UPDATE runs SET updated_at_ms = ?1 WHERE id = ?2",
                        params![now_ms as i64, run_id],
                    )?;
                    self.conn.execute(
                        "UPDATE run_projections
                         SET state = ?1,
                             updated_at_ms = ?2
                         WHERE run_id = ?3",
                        params![state, now_ms as i64, run_id],
                    )?;
                }
            }
            let conversation_id = self
                .conn
                .query_row(
                    "SELECT conversation_id FROM conversation_runs WHERE run_id = ?1",
                    params![run_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(conversation_id) = conversation_id {
                self.refresh_conversation_status_from_run_projections(&conversation_id, now_ms)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Finish a supervisor-owned lease and publish the terminal run state into
    /// the conversation read models. This clears the lease so expired-lease
    /// recovery never requeues an already terminal turn.
    pub fn finish_turn_supervisor_lease(
        &self,
        lease: &TurnSupervisorLease,
        state: &str,
        last_event_sequence: u64,
        terminal_kind: &str,
        now_ms: u64,
    ) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            let updated = self.conn.execute(
                "UPDATE runs
                 SET last_event_sequence = MAX(last_event_sequence, ?1),
                     lease_owner = NULL,
                     lease_expires_at_ms = NULL,
                     updated_at_ms = ?2
                 WHERE id = ?3
                   AND lease_owner = ?4
                   AND fencing_token = ?5",
                params![
                    last_event_sequence as i64,
                    now_ms as i64,
                    lease.run_id,
                    lease.lease_owner,
                    lease.fencing_token as i64
                ],
            )?;
            if updated != 1 {
                return Err(ConversationError::StaleLease(lease.run_id.clone()));
            }
            self.conn.execute(
                "UPDATE turns SET updated_at_ms = ?1 WHERE id = ?2",
                params![now_ms as i64, lease.turn_id],
            )?;
            self.conn.execute(
                "UPDATE run_projections
                 SET state = ?1,
	                     last_sequence = ?2,
	                     updated_at_ms = ?3
	                 WHERE run_id = ?4",
                params![
                    state,
                    last_event_sequence as i64,
                    now_ms as i64,
                    lease.run_id
                ],
            )?;
            self.conn.execute(
                "UPDATE stream_cursors
                 SET last_sequence = ?1,
                     terminal_kind = ?2,
                     updated_at_ms = ?3
                 WHERE run_id = ?4",
                params![
                    last_event_sequence as i64,
                    terminal_kind,
                    now_ms as i64,
                    lease.run_id
                ],
            )?;
            let conversation_id = self
                .conn
                .query_row(
                    "SELECT conversation_id FROM conversation_runs WHERE run_id = ?1",
                    params![lease.run_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(conversation_id) = conversation_id {
                self.refresh_conversation_status_from_run_projections(&conversation_id, now_ms)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Finish a supervisor-owned lease after the event journal reducer has
    /// already updated `run_projections` and `stream_cursors`. This keeps the
    /// projection as lifecycle authority while `runs` stores only lease,
    /// fencing, and checkpoint metadata.
    pub fn finish_turn_supervisor_lease_after_projection(
        &self,
        lease: &TurnSupervisorLease,
        now_ms: u64,
    ) -> Result<FinishedTurnSupervisorLease> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<FinishedTurnSupervisorLease> {
            let (state, last_event_sequence): (String, i64) = self
                .conn
                .query_row(
                    "SELECT state, last_sequence
                     FROM run_projections
                     WHERE run_id = ?1",
                    params![lease.run_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| ConversationError::ProjectionMissing(lease.run_id.clone()))?;
            run_projection_state_from_str(&state)?;
            let updated = self.conn.execute(
                "UPDATE runs
                 SET last_event_sequence = MAX(last_event_sequence, ?1),
                     lease_owner = NULL,
                     lease_expires_at_ms = NULL,
                     updated_at_ms = ?2
                 WHERE id = ?3
                   AND lease_owner = ?4
                   AND fencing_token = ?5",
                params![
                    last_event_sequence,
                    now_ms as i64,
                    lease.run_id,
                    lease.lease_owner,
                    lease.fencing_token as i64
                ],
            )?;
            if updated != 1 {
                return Err(ConversationError::StaleLease(lease.run_id.clone()));
            }
            self.conn.execute(
                "UPDATE turns SET updated_at_ms = ?1 WHERE id = ?2",
                params![now_ms as i64, lease.turn_id],
            )?;
            let conversation_id = self
                .conn
                .query_row(
                    "SELECT conversation_id FROM conversation_runs WHERE run_id = ?1",
                    params![lease.run_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(conversation_id) = conversation_id {
                self.refresh_conversation_status_from_run_projections(&conversation_id, now_ms)?;
            }
            Ok(FinishedTurnSupervisorLease {
                state,
                last_event_sequence: last_event_sequence as u64,
            })
        })();
        match result {
            Ok(finished) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(finished)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    pub fn run_last_event_sequence(&self, run_id: &str) -> Result<Option<u64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT last_sequence FROM run_projections WHERE run_id = ?1")?;
        let mut rows = stmt.query(params![run_id])?;
        Ok(rows
            .next()?
            .map(|r| r.get::<_, i64>(0).map(|v| v as u64))
            .transpose()?)
    }

    /// Effective settings JSON snapshotted for a turn at accept time.
    pub fn turn_effective_settings_json(&self, turn_id: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT effective_settings_json FROM turns WHERE id = ?1")?;
        let mut rows = stmt.query(params![turn_id])?;
        Ok(rows.next()?.map(|r| r.get::<_, String>(0)).transpose()?)
    }

    /// Digest of the settings snapshot accepted for this turn.
    pub fn turn_settings_digest(&self, turn_id: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT settings_digest FROM turns WHERE id = ?1")?;
        let mut rows = stmt.query(params![turn_id])?;
        Ok(rows.next()?.map(|r| r.get::<_, String>(0)).transpose()?)
    }

    /// Model routing decision JSON snapshotted for a turn, if routing has run.
    pub fn turn_model_routing_decision_json(&self, turn_id: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT model_routing_decision_json FROM turns WHERE id = ?1")?;
        let mut rows = stmt.query(params![turn_id])?;
        Ok(rows
            .next()?
            .map(|r| r.get::<_, Option<String>>(0))
            .transpose()?
            .flatten())
    }

    /// Redacted user prompt associated with a turn. The commercial path will
    /// replace this with encrypted raw content retrieval; for the current
    /// supervisor foundation this is the only durable prompt copy available.
    pub fn turn_user_message_text(&self, turn_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT content_redacted FROM messages
             WHERE turn_id = ?1 AND role = 'user'
             ORDER BY sequence ASC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![turn_id])?;
        Ok(rows.next()?.map(|r| r.get::<_, String>(0)).transpose()?)
    }

    /// Attach model-routing provenance to an accepted turn before the caller
    /// exposes it as ready for execution.
    pub fn set_turn_model_routing_decision(
        &self,
        turn_id: &str,
        decision_json: &str,
        now_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE turns
             SET model_routing_decision_json = ?1,
                 updated_at_ms = ?2
             WHERE id = ?3",
            params![decision_json, now_ms as i64, turn_id],
        )?;
        Ok(())
    }

    /// Atomically claim the oldest queued turn for a supervisor. This does not
    /// execute adapter/tool work; it is the durable lease boundary that prevents
    /// two supervisors from running the same accepted turn.
    pub fn claim_next_queued_turn(
        &self,
        supervisor_id: &str,
        lease_ttl_ms: u64,
        now_ms: u64,
    ) -> Result<Option<TurnSupervisorLease>> {
        let lease_ttl_ms = lease_ttl_ms.max(1);
        let lease_expires_at_ms = now_ms.saturating_add(lease_ttl_ms);
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<Option<TurnSupervisorLease>> {
            let claimed = {
                let mut stmt = self.conn.prepare(
                    "SELECT
                         t.id,
                         r.id,
                         t.project_id,
                         t.conversation_id,
                         cr.message_id,
                         t.effective_settings_json,
                         t.model_routing_decision_json,
                         r.fencing_token
                     FROM turns t
                     JOIN runs r ON r.turn_id = t.id
                     JOIN conversation_runs cr
                       ON cr.turn_id = t.id
                      AND cr.run_id = r.id
                      AND cr.relation = 'primary'
                     JOIN run_projections rp
                       ON rp.run_id = r.id
                     WHERE rp.state = 'queued'
                       AND (r.lease_expires_at_ms IS NULL OR r.lease_expires_at_ms <= ?1)
                     ORDER BY t.created_at_ms ASC, t.id ASC
                     LIMIT 1",
                )?;
                let mut rows = stmt.query(params![now_ms as i64])?;
                rows.next()?
                    .map(|row| {
                        Ok::<TurnSupervisorLease, rusqlite::Error>(TurnSupervisorLease {
                            turn_id: row.get(0)?,
                            run_id: row.get(1)?,
                            project_id: row.get(2)?,
                            conversation_id: row.get(3)?,
                            assistant_message_id: row.get(4)?,
                            effective_settings_json: row.get(5)?,
                            model_routing_decision_json: row.get(6)?,
                            lease_owner: supervisor_id.to_string(),
                            lease_expires_at_ms,
                            fencing_token: row.get::<_, i64>(7)? as u64 + 1,
                        })
                    })
                    .transpose()?
            };
            let Some(claimed) = claimed else {
                return Ok(None);
            };
            self.conn.execute(
                "UPDATE turns SET updated_at_ms = ?1 WHERE id = ?2",
                params![now_ms as i64, claimed.turn_id],
            )?;
            self.conn.execute(
                "UPDATE runs
	                     SET lease_owner = ?1,
	                         lease_expires_at_ms = ?2,
	                         fencing_token = ?3,
	                         updated_at_ms = ?4
	                     WHERE id = ?5",
                params![
                    supervisor_id,
                    lease_expires_at_ms as i64,
                    claimed.fencing_token as i64,
                    now_ms as i64,
                    claimed.run_id
                ],
            )?;
            self.conn.execute(
                "UPDATE run_projections
                 SET state = 'running',
                     updated_at_ms = ?1
                 WHERE run_id = ?2",
                params![now_ms as i64, claimed.run_id],
            )?;
            Ok(Some(claimed))
        })();
        match result {
            Ok(value) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Atomically claim one specific queued run for a foreground supervisor.
    /// This keeps an interactive Chat send attached to the run the user just
    /// started instead of being delayed behind older durable queued turns.
    pub fn claim_queued_turn_by_run_id(
        &self,
        supervisor_id: &str,
        run_id: &str,
        lease_ttl_ms: u64,
        now_ms: u64,
    ) -> Result<Option<TurnSupervisorLease>> {
        let lease_ttl_ms = lease_ttl_ms.max(1);
        let lease_expires_at_ms = now_ms.saturating_add(lease_ttl_ms);
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<Option<TurnSupervisorLease>> {
            let claimed = {
                let mut stmt = self.conn.prepare(
                    "SELECT
                         t.id,
                         r.id,
                         t.project_id,
                         t.conversation_id,
                         cr.message_id,
                         t.effective_settings_json,
                         t.model_routing_decision_json,
                         r.fencing_token
                     FROM turns t
                     JOIN runs r ON r.turn_id = t.id
                     JOIN conversation_runs cr
                       ON cr.turn_id = t.id
                      AND cr.run_id = r.id
                      AND cr.relation = 'primary'
                     JOIN run_projections rp
                       ON rp.run_id = r.id
                     WHERE r.id = ?1
                       AND rp.state = 'queued'
                       AND (r.lease_expires_at_ms IS NULL OR r.lease_expires_at_ms <= ?2)
                     LIMIT 1",
                )?;
                let mut rows = stmt.query(params![run_id, now_ms as i64])?;
                rows.next()?
                    .map(|row| {
                        Ok::<TurnSupervisorLease, rusqlite::Error>(TurnSupervisorLease {
                            turn_id: row.get(0)?,
                            run_id: row.get(1)?,
                            project_id: row.get(2)?,
                            conversation_id: row.get(3)?,
                            assistant_message_id: row.get(4)?,
                            effective_settings_json: row.get(5)?,
                            model_routing_decision_json: row.get(6)?,
                            lease_owner: supervisor_id.to_string(),
                            lease_expires_at_ms,
                            fencing_token: row.get::<_, i64>(7)? as u64 + 1,
                        })
                    })
                    .transpose()?
            };
            let Some(claimed) = claimed else {
                return Ok(None);
            };
            self.conn.execute(
                "UPDATE turns SET updated_at_ms = ?1 WHERE id = ?2",
                params![now_ms as i64, claimed.turn_id],
            )?;
            self.conn.execute(
                "UPDATE runs
                     SET lease_owner = ?1,
                         lease_expires_at_ms = ?2,
                         fencing_token = ?3,
                         updated_at_ms = ?4
                     WHERE id = ?5",
                params![
                    supervisor_id,
                    lease_expires_at_ms as i64,
                    claimed.fencing_token as i64,
                    now_ms as i64,
                    claimed.run_id
                ],
            )?;
            self.conn.execute(
                "UPDATE run_projections
                 SET state = 'running',
                     updated_at_ms = ?1
                 WHERE run_id = ?2",
                params![now_ms as i64, claimed.run_id],
            )?;
            Ok(Some(claimed))
        })();
        match result {
            Ok(value) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Extend a running turn lease held by `supervisor_id`.
    pub fn heartbeat_turn_supervisor_lease(
        &self,
        run_id: &str,
        supervisor_id: &str,
        fencing_token: u64,
        lease_ttl_ms: u64,
        now_ms: u64,
    ) -> Result<bool> {
        let lease_expires_at_ms = now_ms.saturating_add(lease_ttl_ms.max(1));
        let updated = self.conn.execute(
            "UPDATE runs
             SET lease_expires_at_ms = ?1,
                 updated_at_ms = ?2
             WHERE id = ?3
               AND lease_owner = ?4
               AND fencing_token = ?5
               AND EXISTS (
                   SELECT 1
                   FROM run_projections rp
                   WHERE rp.run_id = runs.id
                     AND rp.state = 'running'
               )",
            params![
                lease_expires_at_ms as i64,
                now_ms as i64,
                run_id,
                supervisor_id,
                fencing_token as i64
            ],
        )?;
        Ok(updated > 0)
    }

    /// Requeue running turns whose supervisor lease has expired. The caller
    /// should invoke this before attempting to claim new work.
    pub fn recover_expired_turn_supervisor_leases(&self, now_ms: u64) -> Result<usize> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<usize> {
            let run_ids: Vec<String> = {
                let mut stmt = self.conn.prepare(
                    "SELECT r.id
                     FROM runs r
                     JOIN run_projections rp ON rp.run_id = r.id
                     WHERE rp.state = 'running'
                       AND r.lease_expires_at_ms IS NOT NULL
                       AND r.lease_expires_at_ms <= ?1",
                )?;
                let rows = stmt.query_map(params![now_ms as i64], |row| row.get(0))?;
                rows.collect::<std::result::Result<Vec<String>, _>>()?
            };
            for run_id in &run_ids {
                self.conn.execute(
                    "UPDATE runs
                     SET lease_owner = NULL,
                         lease_expires_at_ms = NULL,
                         updated_at_ms = ?1
                     WHERE id = ?2",
                    params![now_ms as i64, run_id],
                )?;
                self.conn.execute(
                    "UPDATE run_projections
                     SET state = 'queued',
                         updated_at_ms = ?1
                     WHERE run_id = ?2",
                    params![now_ms as i64, run_id],
                )?;
            }
            Ok(run_ids.len())
        })();
        match result {
            Ok(value) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Pipeline id recorded in the conversation run projection read model.
    pub fn run_projection_pipeline_id(&self, run_id: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT pipeline_id FROM run_projections WHERE run_id = ?1")?;
        let mut rows = stmt.query(params![run_id])?;
        Ok(rows
            .next()?
            .map(|r| r.get::<_, Option<String>>(0))
            .transpose()?
            .flatten())
    }

    /// Accept a v2 conversation turn in one durable transaction. This is the
    /// daemon bridge for PR-063: it persists the user message, assistant
    /// placeholder, turn/run snapshot, primary run link, idempotency key, queued
    /// projection, and stream cursor before returning an accepted handle. It
    /// intentionally does not execute the adapter; the future TurnSupervisor owns
    /// runtime work after this commit point.
    pub fn accept_conversation_turn(
        &self,
        request: &ConversationTurnStartRequest,
        now_ms: u64,
    ) -> Result<ConversationTurnAccepted> {
        self.accept_conversation_turn_with_raw_ciphertext(request, None, now_ms)
    }

    /// Accept a v2 conversation turn while optionally attaching encrypted raw
    /// content to the user message. Read models still store/index only the
    /// redacted copy; ciphertext is visible only through explicit raw-content
    /// retrieval APIs.
    pub fn accept_conversation_turn_with_raw_ciphertext(
        &self,
        request: &ConversationTurnStartRequest,
        raw_content: Option<&MessageRawContentCiphertext>,
        now_ms: u64,
    ) -> Result<ConversationTurnAccepted> {
        if let Some(existing) =
            self.lookup_turn_idempotency(&request.idempotency_key, &request.conversation_id)?
        {
            let settings_digest =
                self.turn_settings_digest(&existing.turn_id)?
                    .ok_or_else(|| {
                        ConversationError::AcceptedSettingsDigestMissing(existing.turn_id.clone())
                    })?;
            let state = self
                .run_projection_state(&existing.run_id)?
                .ok_or_else(|| ConversationError::ProjectionMissing(existing.run_id.clone()))
                .and_then(|state| run_projection_state_from_str(&state))?;
            return Ok(ConversationTurnAccepted {
                schema: CONVERSATION_TURN_ACCEPTED_SCHEMA.to_string(),
                request_id: request.request_id.clone(),
                turn_id: existing.turn_id.clone(),
                run_id: existing.run_id.clone(),
                user_message_id: existing.user_message_id,
                assistant_message_id: existing.assistant_message_id,
                stream_id: format!("stream-{}", existing.turn_id),
                settings_digest,
                state,
            });
        }

        let turn_id = self.new_id()?;
        let run_id = format!("turn-{turn_id}");
        let stream_id = format!("stream-{turn_id}");
        let user_message_id = self.new_id()?;
        let assistant_message_id = self.new_id()?;
        let user_content_redacted = redact_secrets(&request.message.text);
        let assistant_content_redacted = "...".to_string();

        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            let effective_settings = self.effective_turn_settings_for_turn_start(request)?;
            let effective_settings_json = serde_json::to_string(&effective_settings)?;
            let settings_digest = sha256_v1(&effective_settings_json);
            let user_sequence: i64 = self.conn.query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM messages WHERE conversation_id = ?1",
                params![request.conversation_id],
                |r| r.get(0),
            )?;
            let assistant_sequence = user_sequence + 1;
            self.conn.execute(
                "INSERT INTO messages(id, project_id, conversation_id, turn_id, role, state, content_redacted, content_ciphertext, content_nonce, created_at_ms, updated_at_ms, sequence)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11)",
                params![
                    user_message_id,
                    request.project_id,
                    request.conversation_id,
                    turn_id,
                    enum_to_str(&MessageRole::User),
                    enum_to_str(&MessageState::Complete),
                    user_content_redacted,
                    raw_content.map(|content| content.ciphertext.as_slice()),
                    raw_content.map(|content| content.nonce.as_slice()),
                    now_ms as i64,
                    user_sequence,
                ],
            )?;
            self.conn.execute(
                "INSERT INTO messages_fts(conversation_id, message_id, content_redacted) VALUES(?1, ?2, ?3)",
                params![request.conversation_id, user_message_id, user_content_redacted],
            )?;
            self.conn.execute(
                "INSERT INTO messages(id, project_id, conversation_id, turn_id, role, state, content_redacted, created_at_ms, updated_at_ms, sequence)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9)",
                params![
                    assistant_message_id,
                    request.project_id,
                    request.conversation_id,
                    turn_id,
                    enum_to_str(&MessageRole::Assistant),
                    enum_to_str(&MessageState::Streaming),
                    assistant_content_redacted,
                    now_ms as i64,
                    assistant_sequence,
                ],
            )?;
            self.conn.execute(
                "INSERT INTO messages_fts(conversation_id, message_id, content_redacted) VALUES(?1, ?2, ?3)",
                params![
                    request.conversation_id,
                    assistant_message_id,
                    assistant_content_redacted
                ],
            )?;
            self.conn.execute(
                "INSERT INTO turns(
                     id, project_id, conversation_id, client_turn_id, request_id,
                     idempotency_key, effective_settings_json, settings_digest,
                     model_routing_decision_json, created_at_ms, updated_at_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?9)",
                params![
                    turn_id,
                    request.project_id,
                    request.conversation_id,
                    request.client_turn_id,
                    request.request_id,
                    request.idempotency_key,
                    effective_settings_json,
                    settings_digest,
                    now_ms as i64,
                ],
            )?;
            self.conn.execute(
                "INSERT INTO runs(
                     id, turn_id, last_event_sequence, lease_owner,
                     lease_expires_at_ms, fencing_token, created_at_ms, updated_at_ms)
                 VALUES(?1, ?2, 0, NULL, NULL, 0, ?3, ?3)",
                params![run_id, turn_id, now_ms as i64],
            )?;
            self.conn.execute(
                "INSERT INTO conversation_runs(conversation_id, message_id, turn_id, run_id, relation, created_at_ms)
                 VALUES(?1, ?2, ?3, ?4, 'primary', ?5)",
                params![
                    request.conversation_id,
                    assistant_message_id,
                    turn_id,
                    run_id,
                    now_ms as i64,
                ],
            )?;
            self.conn.execute(
                "INSERT INTO turn_idempotency(idempotency_key, conversation_id, turn_id, user_message_id, assistant_message_id, run_id, created_at_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    request.idempotency_key,
                    request.conversation_id,
                    turn_id,
                    user_message_id,
                    assistant_message_id,
                    run_id,
                    now_ms as i64,
                ],
            )?;
            self.conn.execute(
                "INSERT INTO run_projections(
                     run_id, project_id, conversation_id, turn_id, state,
                     pipeline_id, graph_revision, last_sequence, projection_json, updated_at_ms)
                 VALUES(?1, ?2, ?3, ?4, 'queued', ?5, ?6, 0, '{}', ?7)",
                params![
                    run_id,
                    request.project_id,
                    request.conversation_id,
                    turn_id,
                    effective_settings.pipeline_id,
                    effective_settings.graph_revision,
                    now_ms as i64,
                ],
            )?;
            self.conn.execute(
                "INSERT INTO stream_cursors(stream_id, run_id, last_sequence, terminal_kind, updated_at_ms)
                 VALUES(?1, ?2, 0, NULL, ?3)",
                params![stream_id, run_id, now_ms as i64],
            )?;
            self.conn.execute(
                "UPDATE conversations
                 SET status = 'running',
                     last_message_at_ms = ?1,
                     updated_at_ms = ?1
                 WHERE id = ?2",
                params![now_ms as i64, request.conversation_id],
            )?;
            Ok(())
        })();
        match result {
            Ok(()) => self.conn.execute_batch("COMMIT")?,
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        }

        let settings_digest = self
            .turn_settings_digest(&turn_id)?
            .ok_or_else(|| ConversationError::AcceptedSettingsDigestMissing(turn_id.clone()))?;

        Ok(ConversationTurnAccepted {
            schema: CONVERSATION_TURN_ACCEPTED_SCHEMA.to_string(),
            request_id: request.request_id.clone(),
            turn_id,
            run_id,
            user_message_id,
            assistant_message_id,
            stream_id,
            settings_digest,
            state: RunProjectionState::Queued,
        })
    }

    fn thread_settings_for_accept(
        &self,
        conversation_id: &str,
    ) -> Result<ConversationThreadSettings> {
        let thread_settings = match self.get_thread_settings(conversation_id)? {
            Some(raw) => serde_json::from_str::<ConversationThreadSettings>(&raw)?,
            None => ConversationThreadSettings::default_for(conversation_id, 0),
        };
        Ok(thread_settings)
    }

    fn effective_turn_settings_for_accept(
        &self,
        conversation_id: &str,
    ) -> Result<ConversationTurnSettings> {
        let thread_settings = self.thread_settings_for_accept(conversation_id)?;
        Ok(turn_settings_from_thread(&thread_settings))
    }

    fn effective_turn_settings_for_turn_start(
        &self,
        request: &ConversationTurnStartRequest,
    ) -> Result<ConversationTurnSettings> {
        let thread_settings = self.thread_settings_for_accept(&request.conversation_id)?;
        if let Some(client_updated_at_ms) = request.thread_settings_updated_at_ms {
            if client_updated_at_ms != thread_settings.updated_at_ms {
                return Err(ConversationError::StaleThreadSettingsRevision {
                    conversation_id: request.conversation_id.clone(),
                    client_updated_at_ms,
                    canonical_updated_at_ms: thread_settings.updated_at_ms,
                });
            }
        }
        Ok(turn_settings_from_thread(&thread_settings))
    }

    /// Look up the identifiers previously recorded for an idempotency key within
    /// a conversation, if any.
    pub fn lookup_turn_idempotency(
        &self,
        idempotency_key: &str,
        conversation_id: &str,
    ) -> Result<Option<TurnIdempotencyRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT turn_id, user_message_id, assistant_message_id, run_id FROM turn_idempotency
             WHERE conversation_id = ?1 AND idempotency_key = ?2",
        )?;
        let mut rows = stmt.query(params![conversation_id, idempotency_key])?;
        match rows.next()? {
            Some(row) => Ok(Some(TurnIdempotencyRecord {
                turn_id: row.get(0)?,
                user_message_id: row.get(1)?,
                assistant_message_id: row.get(2)?,
                run_id: row.get(3)?,
            })),
            None => Ok(None),
        }
    }

    /// Cursor pagination: returns up to `limit` messages with sequence strictly
    /// below `before_sequence` (or the latest when `None`), ascending for display.
    pub fn message_page(
        &self,
        conversation_id: &str,
        before_sequence: Option<i64>,
        limit: usize,
    ) -> Result<Vec<ConversationMessage>> {
        let before = before_sequence.unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, conversation_id, turn_id, role, state, content_redacted, sequence, created_at_ms, updated_at_ms
             FROM messages WHERE conversation_id = ?1 AND sequence < ?2 ORDER BY sequence DESC LIMIT ?3",
        )?;
        let mut rows = stmt.query(params![conversation_id, before, limit as i64])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(message_from_row(row)?);
        }
        out.reverse();
        Ok(out)
    }

    /// FTS search over redacted content within a conversation. Returns message ids.
    pub fn search_messages(&self, conversation_id: &str, query: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT message_id FROM messages_fts WHERE messages_fts MATCH ?1 AND conversation_id = ?2",
        )?;
        let ids: Vec<String> = stmt
            .query_map(params![query, conversation_id], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(ids)
    }

    pub fn upsert_summary(
        &self,
        conversation_id: &str,
        summary_raw: &str,
        source_message_sequence: i64,
        now_ms: u64,
    ) -> Result<()> {
        let summary_redacted = redact_secrets(summary_raw);
        self.conn.execute(
            "INSERT INTO conversation_summaries(conversation_id, summary_redacted, source_message_sequence, generated_at_ms)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(conversation_id) DO UPDATE SET
                summary_redacted = excluded.summary_redacted,
                source_message_sequence = excluded.source_message_sequence,
                generated_at_ms = excluded.generated_at_ms",
            params![conversation_id, summary_redacted, source_message_sequence, now_ms as i64],
        )?;
        Ok(())
    }

    pub fn get_summary(&self, conversation_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT summary_redacted FROM conversation_summaries WHERE conversation_id = ?1",
        )?;
        let mut rows = stmt.query(params![conversation_id])?;
        Ok(rows.next()?.map(|r| r.get::<_, String>(0)).transpose()?)
    }

    pub fn get_digest(&self, conversation_id: &str) -> Result<Option<ConversationDigest>> {
        let mut stmt = self.conn.prepare(
            "SELECT summary_redacted, source_message_sequence, generated_at_ms
             FROM conversation_summaries WHERE conversation_id = ?1",
        )?;
        let mut rows = stmt.query(params![conversation_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(ConversationDigest {
            schema: CONVERSATION_DIGEST_SCHEMA.to_string(),
            conversation_id: conversation_id.to_string(),
            summary_redacted: row.get(0)?,
            source_message_sequence: row.get(1)?,
            generated_at_ms: row.get::<_, i64>(2)? as u64,
        }))
    }

    /// Seed many messages in a single transaction (test/fixture helper).
    pub fn seed_messages(
        &mut self,
        project_id: &str,
        conversation_id: &str,
        turn_id: &str,
        count: usize,
        now_ms: u64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO messages(id, project_id, conversation_id, turn_id, role, state, content_redacted, created_at_ms, updated_at_ms, sequence)
                 VALUES(?1, ?2, ?3, ?4, 'user', 'complete', ?5, ?6, ?6, ?7)",
            )?;
            let mut fts = tx.prepare(
                "INSERT INTO messages_fts(conversation_id, message_id, content_redacted) VALUES(?1, ?2, ?3)",
            )?;
            for i in 0..count {
                let id = format!("seed-{conversation_id}-{i}");
                let content = format!("seed message {i}");
                let sequence = (i + 1) as i64;
                insert.execute(params![
                    id,
                    project_id,
                    conversation_id,
                    turn_id,
                    content,
                    now_ms as i64,
                    sequence
                ])?;
                fts.execute(params![conversation_id, id, content])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

const SELECT_CONVERSATION_BASE: &str = "SELECT c.id, c.project_id, c.title, c.title_source, c.status, c.pinned, c.archived, c.created_at_ms, c.updated_at_ms, c.last_message_at_ms, (SELECT COUNT(*) FROM messages m WHERE m.conversation_id = c.id) FROM conversations c WHERE c.id = ?1";

const SELECT_CONVERSATION_LIST_PREFIX: &str = "SELECT c.id, c.project_id, c.title, c.title_source, c.status, c.pinned, c.archived, c.created_at_ms, c.updated_at_ms, c.last_message_at_ms, (SELECT COUNT(*) FROM messages m WHERE m.conversation_id = c.id) FROM conversations c";

fn conversation_from_row(row: &Row<'_>) -> Result<ConversationSummary> {
    let title_source: String = row.get(3)?;
    let status: String = row.get(4)?;
    let pinned: i64 = row.get(5)?;
    let archived: i64 = row.get(6)?;
    let last: Option<i64> = row.get(9)?;
    let count: i64 = row.get(10)?;
    Ok(ConversationSummary {
        schema: CONVERSATION_SUMMARY_SCHEMA.to_string(),
        id: row.get(0)?,
        project_id: row.get(1)?,
        title: row.get(2)?,
        title_source: enum_from_str::<TitleSource>(&title_source)?,
        status: enum_from_str::<ConversationStatus>(&status)?,
        pinned: pinned != 0,
        archived: archived != 0,
        message_count: count as u64,
        created_at_ms: row.get::<_, i64>(7)? as u64,
        updated_at_ms: row.get::<_, i64>(8)? as u64,
        last_message_at_ms: last.map(|v| v as u64),
    })
}

fn message_from_row(row: &Row<'_>) -> Result<ConversationMessage> {
    let role: String = row.get(4)?;
    let state: String = row.get(5)?;
    Ok(ConversationMessage {
        schema: CONVERSATION_MESSAGE_SCHEMA.to_string(),
        id: row.get(0)?,
        project_id: row.get(1)?,
        conversation_id: row.get(2)?,
        turn_id: row.get(3)?,
        role: enum_from_str::<MessageRole>(&role)?,
        state: enum_from_str::<MessageState>(&state)?,
        content_redacted: row.get(6)?,
        sequence: row.get(7)?,
        created_at_ms: row.get::<_, i64>(8)? as u64,
        updated_at_ms: row.get::<_, i64>(9)? as u64,
    })
}

fn timeline_item_from_row(row: &Row<'_>) -> Result<TimelineItem> {
    let kind: String = row.get(6)?;
    let payload: String = row.get(8)?;
    Ok(TimelineItem {
        schema: TIMELINE_ITEM_SCHEMA.to_string(),
        id: row.get(0)?,
        project_id: row.get(1)?,
        conversation_id: row.get(2)?,
        turn_id: row.get(3)?,
        run_id: row.get(4)?,
        sequence: row.get(5)?,
        kind: enum_from_str::<TimelineItemKind>(&kind)?,
        state: row.get(7)?,
        payload: serde_json::from_str(&payload)?,
        created_at_ms: row.get::<_, i64>(9)? as u64,
        updated_at_ms: row.get::<_, i64>(10)? as u64,
    })
}

fn execution_event_timeline_item(
    anchor: &TimelineRunAnchor,
    event: &ExecutionEventEnvelope,
) -> TimelineItem {
    let sequence_offset = i64::try_from(event.sequence)
        .unwrap_or(CONVERSATION_TIMELINE_SEQUENCE_STRIDE - 1)
        .min(CONVERSATION_TIMELINE_SEQUENCE_STRIDE - 1);
    let occurred_at_ms =
        execution_event_occurred_at_ms(&event.occurred_at).unwrap_or(anchor.message_updated_at_ms);
    let event_kind = event.kind.as_str().to_string();
    let content_redacted = execution_event_timeline_text(event);
    let kind = timeline_kind_for_execution_event(event);
    let state = timeline_state_for_execution_event(event);
    let payload_redacted = redact_json_value(event.payload.clone());
    let mut payload = serde_json::json!({
        "source_schema": event.schema,
        "event_id": event.id,
        "event_kind": event_kind,
        "event_sequence": event.sequence,
        "actor": event.actor,
        "causation_id": event.causation_id,
        "correlation_id": event.correlation_id,
        "content_redacted": content_redacted,
        "payload_redacted": payload_redacted,
        "sensitivity": event.sensitivity.as_str(),
        "evidence_refs": event.evidence_refs,
        "projection": "event_journal_replay"
    });
    merge_git_receipt_payload(event, &mut payload);
    merge_image_artifact_payload(event, &mut payload);
    merge_assistant_event_payload(anchor, event, &mut payload);
    merge_execution_detail_payload(event, &mut payload);
    TimelineItem {
        schema: TIMELINE_ITEM_SCHEMA.to_string(),
        id: format!("timeline-event-{}", event.id),
        project_id: anchor.project_id.clone(),
        conversation_id: anchor.conversation_id.clone(),
        turn_id: Some(anchor.turn_id.clone()),
        run_id: Some(event.run_id.clone()),
        sequence: anchor
            .message_sequence
            .saturating_mul(CONVERSATION_TIMELINE_SEQUENCE_STRIDE)
            .saturating_add(sequence_offset),
        kind,
        state,
        payload,
        created_at_ms: occurred_at_ms,
        updated_at_ms: occurred_at_ms,
    }
}

fn timeline_kind_for_execution_event(event: &ExecutionEventEnvelope) -> TimelineItemKind {
    if let Some(agent_kind) = event
        .payload
        .get("agent_event_kind")
        .and_then(serde_json::Value::as_str)
    {
        return match agent_kind {
            "plan_updated" => TimelineItemKind::Plan,
            "tool_call_started" | "tool_call_output" | "tool_call_completed" => {
                TimelineItemKind::ToolCall
            }
            "file_patch_proposed" | "file_patch_applied" => TimelineItemKind::Patch,
            "verification_started" | "verification_completed" => TimelineItemKind::Verification,
            "approval_requested" | "approval_resolved" => TimelineItemKind::Approval,
            "worker_spawned" | "worker_progress" | "worker_completed" => TimelineItemKind::Worker,
            "image_artifact_created" => TimelineItemKind::ImageArtifact,
            "warning" => TimelineItemKind::Warning,
            "error" => TimelineItemKind::Error,
            "assistant_text_delta" | "assistant_text_completed" => {
                TimelineItemKind::AssistantMessage
            }
            _ => TimelineItemKind::Warning,
        };
    }

    match event.kind {
        EventKind::ApprovalRequested | EventKind::ApprovalApproved | EventKind::ApprovalDenied => {
            TimelineItemKind::Approval
        }
        EventKind::VerificationPassed => TimelineItemKind::Verification,
        EventKind::VerificationFailed => TimelineItemKind::Error,
        EventKind::GitCommitReceipt => TimelineItemKind::CommitReceipt,
        EventKind::GitPushReceipt | EventKind::GitPushFailed => TimelineItemKind::PushReceipt,
        EventKind::ImageArtifactCreated => TimelineItemKind::ImageArtifact,
        EventKind::WorkItemQueued
        | EventKind::WorkItemLeased
        | EventKind::WorkItemRunning
        | EventKind::WorkItemCompleted
        | EventKind::LeaseHeartbeat
        | EventKind::LeaseExpired
        | EventKind::RunStarted
        | EventKind::RunPaused
        | EventKind::RunResumed
        | EventKind::RunCancelled
        | EventKind::RunCompleted
        | EventKind::SteeringRequested
        | EventKind::SnapshotWritten => TimelineItemKind::Worker,
        EventKind::Unknown => TimelineItemKind::Warning,
    }
}

fn timeline_state_for_execution_event(event: &ExecutionEventEnvelope) -> String {
    if let Some(agent_kind) = payload_string_deep(event, "agent_event_kind") {
        match agent_kind {
            "assistant_text_delta" => return "streaming".to_string(),
            "assistant_text_completed" => return "completed".to_string(),
            _ => {}
        }
    }
    match event.kind {
        EventKind::GitCommitReceipt => "committed".to_string(),
        EventKind::GitPushReceipt => "pushed".to_string(),
        EventKind::GitPushFailed => "failed".to_string(),
        EventKind::ImageArtifactCreated => "created".to_string(),
        _ => event.kind.as_str().to_string(),
    }
}

fn terminal_kind_for_execution_event(event: &ExecutionEventEnvelope) -> Option<&'static str> {
    if is_advisory_role_worker_failure_event(event) {
        return None;
    }
    match event.kind {
        EventKind::RunCancelled => Some("cancelled"),
        EventKind::RunCompleted => Some("completed"),
        EventKind::VerificationFailed => Some("failed"),
        _ => None,
    }
}

fn run_projection_state_for_execution_event(
    event: &ExecutionEventEnvelope,
) -> Option<RunProjectionState> {
    if is_advisory_role_worker_failure_event(event) {
        return None;
    }
    match event.kind {
        EventKind::RunStarted | EventKind::RunResumed => Some(RunProjectionState::Running),
        EventKind::RunPaused => Some(RunProjectionState::Paused),
        EventKind::RunCancelled => Some(RunProjectionState::Cancelled),
        EventKind::RunCompleted => Some(RunProjectionState::Completed),
        EventKind::VerificationFailed => Some(RunProjectionState::Failed),
        _ => None,
    }
}

fn is_advisory_role_worker_failure_event(event: &ExecutionEventEnvelope) -> bool {
    if event.kind != EventKind::VerificationFailed {
        return false;
    }
    let worker_id = payload_string_deep(event, "worker_id").unwrap_or_default();
    let work_item_id = payload_string_deep(event, "work_item_id").unwrap_or_default();
    let code = payload_string_deep(event, "code").unwrap_or_default();
    let reason_code = payload_string_deep(event, "reason_code").unwrap_or_default();
    worker_id.starts_with("role-subcontract-")
        || work_item_id.starts_with("turn-role-")
        || code.starts_with("role_worker_")
        || reason_code == "provider_role_call_failed"
}

fn merge_run_projection_state(
    current: RunProjectionState,
    incoming: RunProjectionState,
) -> RunProjectionState {
    if current.is_terminal() {
        return current;
    }
    if incoming.is_terminal() {
        return incoming;
    }
    match incoming {
        RunProjectionState::Queued => current,
        _ => incoming,
    }
}

fn run_projection_state_to_str(state: RunProjectionState) -> &'static str {
    match state {
        RunProjectionState::Queued => "queued",
        RunProjectionState::Running => "running",
        RunProjectionState::Paused => "paused",
        RunProjectionState::Completed => "completed",
        RunProjectionState::Failed => "failed",
        RunProjectionState::Cancelled => "cancelled",
    }
}

fn execution_event_timeline_text(event: &ExecutionEventEnvelope) -> String {
    if is_assistant_text_event(event) {
        if let Some((_target_key, value)) = assistant_event_text_value(event) {
            return redacted_timeline_snippet(value, ASSISTANT_EVENT_SNIPPET_CHARS);
        }
    }
    event
        .payload
        .get("content_redacted")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            event
                .payload
                .get("payload")
                .and_then(|payload| payload.get("content_redacted"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            event
                .payload
                .get("payload")
                .and_then(|payload| payload.get("message"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            event
                .payload
                .get("message")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            event
                .payload
                .get("agent_event_kind")
                .and_then(serde_json::Value::as_str)
        })
        .map(redact_secrets)
        .unwrap_or_else(|| event.kind.as_str().replace('_', " "))
}

fn is_assistant_text_event(event: &ExecutionEventEnvelope) -> bool {
    matches!(
        payload_string_deep(event, "agent_event_kind"),
        Some("assistant_text_delta" | "assistant_text_completed")
    )
}

fn assistant_event_text_value(event: &ExecutionEventEnvelope) -> Option<(&'static str, &str)> {
    for (source_key, target_key) in [
        ("assistant_delta", "assistant_delta"),
        ("text_delta", "assistant_delta"),
        ("delta", "assistant_delta"),
        ("content_delta", "assistant_delta"),
        ("assistant_text", "assistant_text"),
        ("text", "assistant_text"),
        ("content", "assistant_text"),
    ] {
        if let Some(value) = payload_string_deep(event, source_key) {
            return Some((target_key, value));
        }
    }
    None
}

fn redacted_timeline_snippet(input: &str, max_chars: usize) -> String {
    let redacted = redact_secrets(input);
    let mut snippet: String = redacted.chars().take(max_chars).collect();
    if redacted.chars().count() > max_chars {
        snippet.push_str("...");
    }
    snippet
}

fn merge_git_receipt_payload(event: &ExecutionEventEnvelope, payload: &mut serde_json::Value) {
    match event.kind {
        EventKind::GitCommitReceipt => {
            copy_payload_string(event, payload, "commit");
            copy_payload_array(event, payload, "paths");
            copy_payload_string(event, payload, "message");
            copy_payload_bool(event, payload, "committed");
            copy_payload_string(event, payload, "staged_diff_hash");
            copy_payload_string(event, payload, "staged_diff_ref");
            copy_payload_string(event, payload, "reviewed_staged_diff_hash");
            copy_payload_string(event, payload, "reviewed_staged_diff_ref");
            copy_payload_string(event, payload, "integration_final_diff_hash");
            copy_payload_string(event, payload, "integration_final_diff_ref");
            copy_payload_string(event, payload, "integration_run_id");
            copy_payload_string(event, payload, "integration_candidate_id");
            payload["projection"] = serde_json::Value::String("git_receipt_event".to_string());
        }
        EventKind::GitPushReceipt | EventKind::GitPushFailed => {
            copy_payload_string(event, payload, "remote");
            copy_payload_string(event, payload, "ref");
            copy_payload_string(event, payload, "remote_oid");
            copy_payload_string(event, payload, "local_oid");
            copy_payload_bool(event, payload, "already_done");
            copy_payload_bool(event, payload, "pushed");
            copy_payload_string(event, payload, "intent_id");
            copy_payload_string(event, payload, "effect_digest");
            copy_payload_string(event, payload, "idempotency_key");
            copy_payload_string(event, payload, "remote_url_redacted");
            copy_payload_string(event, payload, "remote_expected_oid");
            copy_payload_bool(event, payload, "protected");
            copy_payload_string(event, payload, "approval_id");
            copy_payload_bool(event, payload, "approval_matched");
            copy_payload_string(event, payload, "reason_code");
            copy_payload_string(event, payload, "diagnostic_id");
            payload["projection"] = serde_json::Value::String("git_receipt_event".to_string());
        }
        _ => {}
    }
}

fn merge_image_artifact_payload(event: &ExecutionEventEnvelope, payload: &mut serde_json::Value) {
    if event.kind != EventKind::ImageArtifactCreated
        && event
            .payload
            .get("agent_event_kind")
            .and_then(serde_json::Value::as_str)
            != Some("image_artifact_created")
    {
        return;
    }
    copy_payload_string_deep(event, payload, "asset_id");
    copy_payload_string_deep(event, payload, "provider_id");
    copy_payload_string_deep(event, payload, "model_id");
    copy_payload_string_deep(event, payload, "path");
    copy_payload_string_deep(event, payload, "content_hash");
    copy_payload_string_deep(event, payload, "provenance_hash");
    copy_payload_string_deep(event, payload, "operation");
    copy_payload_number_deep(event, payload, "width");
    copy_payload_number_deep(event, payload, "height");
    payload["projection"] = serde_json::Value::String("image_artifact_event".to_string());
}

fn merge_assistant_event_payload(
    anchor: &TimelineRunAnchor,
    event: &ExecutionEventEnvelope,
    payload: &mut serde_json::Value,
) {
    if !is_assistant_text_event(event) {
        return;
    }
    copy_payload_string_deep(event, payload, "assistant_message_id");
    if payload
        .get("assistant_message_id")
        .and_then(serde_json::Value::as_str)
        .is_none()
    {
        payload["assistant_message_id"] = serde_json::Value::String(anchor.message_id.clone());
    }
    if let Some((target_key, value)) = assistant_event_text_value(event) {
        payload[target_key] = serde_json::Value::String(redacted_timeline_snippet(
            value,
            ASSISTANT_EVENT_SNIPPET_CHARS,
        ));
    }
    copy_payload_string_deep(event, payload, "response_hash");
    copy_payload_string_deep(event, payload, "completion_reason");
    copy_payload_string_deep_as(event, payload, "finish_reason", "completion_reason");
    payload["projection"] = serde_json::Value::String("assistant_execution_event".to_string());
}

fn merge_execution_detail_payload(event: &ExecutionEventEnvelope, payload: &mut serde_json::Value) {
    for key in [
        "agent_event_kind",
        "tool",
        "command_redacted",
        "worker_id",
        "work_item_id",
        "lease_id",
        "lease_holder",
        "fencing_token",
        "fencing_holder",
        "batch_id",
        "code",
        "reason_code",
        "receipt_ref",
        "patch_ref",
        "verification_ref",
        "repair_ref",
        "final_diff_ref",
        "context_pack_ref",
        "worker_context_pack_ref",
        "provider_id",
        "model_id",
        "response_hash",
    ] {
        copy_payload_string_deep(event, payload, key);
    }
    copy_payload_string_deep_as(event, payload, "role", "role_label");
    for key in [
        "applied_files",
        "target_paths",
        "touched_paths",
        "test_targets",
    ] {
        copy_payload_array_deep(event, payload, key);
    }
    for key in [
        "approval_required",
        "main_workspace_modified",
        "verifier_passed",
        "worker_ok",
        "timed_out",
        "model_call",
        "parallel_batch",
    ] {
        copy_payload_bool_deep(event, payload, key);
    }
    for key in [
        "patch_count",
        "apply_result_count",
        "exit_code",
        "duration_ms",
        "response_bytes",
        "parallel_batch_size",
        "parallel_lane_index",
    ] {
        copy_payload_number_deep(event, payload, key);
    }
}

fn copy_payload_string(event: &ExecutionEventEnvelope, payload: &mut serde_json::Value, key: &str) {
    if let Some(value) = event.payload.get(key).and_then(serde_json::Value::as_str) {
        payload[key] = serde_json::Value::String(redact_secrets(value));
    }
}

fn copy_payload_string_deep(
    event: &ExecutionEventEnvelope,
    payload: &mut serde_json::Value,
    key: &str,
) {
    if let Some(value) = payload_string_deep(event, key) {
        payload[key] = serde_json::Value::String(redact_secrets(value));
    }
}

fn copy_payload_string_deep_as(
    event: &ExecutionEventEnvelope,
    payload: &mut serde_json::Value,
    source_key: &str,
    target_key: &str,
) {
    if let Some(value) = payload_string_deep(event, source_key) {
        payload[target_key] = serde_json::Value::String(redact_secrets(value));
    }
}

fn payload_string_deep<'a>(event: &'a ExecutionEventEnvelope, key: &str) -> Option<&'a str> {
    event
        .payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            event
                .payload
                .get("payload")
                .and_then(|payload| payload.get(key))
                .and_then(serde_json::Value::as_str)
        })
}

fn copy_payload_number_deep(
    event: &ExecutionEventEnvelope,
    payload: &mut serde_json::Value,
    key: &str,
) {
    if let Some(value) = event
        .payload
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            event
                .payload
                .get("payload")
                .and_then(|payload| payload.get(key))
                .and_then(serde_json::Value::as_u64)
        })
    {
        payload[key] = serde_json::Value::Number(value.into());
    }
}

fn copy_payload_array_deep(
    event: &ExecutionEventEnvelope,
    payload: &mut serde_json::Value,
    key: &str,
) {
    if let Some(values) = event
        .payload
        .get(key)
        .and_then(serde_json::Value::as_array)
        .or_else(|| {
            event
                .payload
                .get("payload")
                .and_then(|payload| payload.get(key))
                .and_then(serde_json::Value::as_array)
        })
    {
        let redacted = values
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(|value| serde_json::Value::String(redact_secrets(value)))
            .collect();
        payload[key] = serde_json::Value::Array(redacted);
    }
}

fn copy_payload_bool_deep(
    event: &ExecutionEventEnvelope,
    payload: &mut serde_json::Value,
    key: &str,
) {
    if let Some(value) = event
        .payload
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .or_else(|| {
            event
                .payload
                .get("payload")
                .and_then(|payload| payload.get(key))
                .and_then(serde_json::Value::as_bool)
        })
    {
        payload[key] = serde_json::Value::Bool(value);
    }
}

fn copy_payload_array(event: &ExecutionEventEnvelope, payload: &mut serde_json::Value, key: &str) {
    if let Some(values) = event.payload.get(key).and_then(serde_json::Value::as_array) {
        let redacted = values
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(|value| serde_json::Value::String(redact_secrets(value)))
            .collect();
        payload[key] = serde_json::Value::Array(redacted);
    }
}

fn copy_payload_bool(event: &ExecutionEventEnvelope, payload: &mut serde_json::Value, key: &str) {
    if let Some(value) = event.payload.get(key).and_then(serde_json::Value::as_bool) {
        payload[key] = serde_json::Value::Bool(value);
    }
}

fn execution_event_occurred_at_ms(occurred_at: &str) -> Option<u64> {
    let (secs, nanos) = occurred_at.split_once('.')?;
    let secs = secs.parse::<u64>().ok()?;
    let nanos_digits: String = nanos.chars().take(9).collect();
    let nanos = nanos_digits.parse::<u64>().ok()?;
    Some(secs.saturating_mul(1_000).saturating_add(nanos / 1_000_000))
}

fn redact_json_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(value) => serde_json::Value::String(redact_secrets(&value)),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(redact_json_value).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, redact_json_value(value)))
                .collect(),
        ),
        other => other,
    }
}

fn turn_settings_from_thread(settings: &ConversationThreadSettings) -> ConversationTurnSettings {
    ConversationTurnSettings {
        model: settings.model_selection.clone(),
        reasoning_effort: settings.reasoning_effort,
        execution_mode: settings.execution_mode,
        pipeline_id: settings.pipeline_id.clone(),
        graph_revision: None,
        max_parallelism: settings.max_parallelism,
        verifier_count: settings.verifier_count,
        tool_policy_id: settings.tool_policy_id.clone(),
        approval_policy_id: settings.approval_policy_id.clone(),
        token_budget: settings.token_budget,
        cost_budget_usd: settings.cost_budget_usd,
        timeout_ms: settings.timeout_ms,
        image_model_id: settings.image_model_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "redaction-test-secret-fixture-0002";

    fn openai_key_assignment(value: &str) -> String {
        format!("{}={value}", "OPENAI_API_KEY")
    }

    fn project_and_conversation(repo: &ConversationRepository) -> (String, String) {
        let pid = repo.create_project("/ws/demo", "Demo", 1_000).unwrap();
        let cid = repo.create_conversation(&pid, "First", 1_000).unwrap();
        (pid, cid)
    }

    #[test]
    fn redaction_corpus_catches_secrets_and_spares_safe_text() {
        // Secret-bearing inputs that MUST be redacted (recovery directive §19.5 /
        // §23.6 security corpus). Includes the OpenRouter key shape.
        let secrets = [
            concat!("sk-", "or-v1-deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
            concat!("sk-", "proj-abcdefghijklmnop1234567890"),
            concat!("ghp", "_0123456789abcdef0123456789abcdef0123"),
            concat!("github", "_pat_11ABCDEFG0abcdefghijklmnopqrstuv"),
            concat!("AKIA", "IOSFODNN7EXAMPLE"),
            concat!("AIza", "SyA1234567890abcdefghijklmnop"),
            concat!("xoxb", "-1234567890-abcdefABCDEF1234"),
            concat!("API_KEY=sk-", "or-v1-deadbeefdeadbeefdeadbeef"),
            "password:s3cr3tValueWithEnoughEntropy123",
            "https://user:hunter2longpassword@example.com/x",
        ];
        for secret in secrets {
            assert!(
                redact_secrets(secret).contains("[REDACTED]"),
                "not redacted: {secret}"
            );
        }
        // Whole credential lines redact wholesale.
        assert_eq!(
            redact_secrets("-----BEGIN OPENSSH PRIVATE KEY-----"),
            "[REDACTED]"
        );
        assert!(redact_secrets("Authorization: Bearer sometoken").contains("[REDACTED]"));

        // Ordinary content must survive untouched (no over-redaction).
        for safe in [
            "hello world",
            "fix the parser bug",
            "src/lib.rs:42",
            "RunProjectionState::Paused",
            "the cat sat on the mat",
        ] {
            assert_eq!(redact_secrets(safe), safe, "wrongly redacted: {safe}");
        }
    }

    #[test]
    fn migrate_is_idempotent_and_works_on_existing_db() {
        let dir = std::env::temp_dir().join(format!("opensks-conv-{}", std::process::id()));
        let path = dir.join("conversations.sqlite3");
        let _ = std::fs::remove_file(&path);
        // First open migrates an empty DB.
        let repo = ConversationRepository::open(&path).unwrap();
        let pid = repo.create_project("/ws/x", "X", 10).unwrap();
        drop(repo);
        // Reopening an existing DB migrates again without error and preserves data.
        let repo2 = ConversationRepository::open(&path).unwrap();
        assert_eq!(repo2.project_id_for_workspace("/ws/x").unwrap(), Some(pid));
        repo2.migrate().unwrap(); // explicit second migrate is a no-op
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn migrate_drops_legacy_lifecycle_state_columns() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE projects (
                id TEXT PRIMARY KEY,
                workspace_key TEXT NOT NULL UNIQUE,
                display_name TEXT NOT NULL,
                last_conversation_id TEXT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE TABLE conversations (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                title TEXT NOT NULL,
                title_source TEXT NOT NULL,
                status TEXT NOT NULL,
                pinned INTEGER NOT NULL DEFAULT 0,
                archived INTEGER NOT NULL DEFAULT 0,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                last_message_at_ms INTEGER NULL,
                version INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE messages (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                conversation_id TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                role TEXT NOT NULL,
                state TEXT NOT NULL,
                content_redacted TEXT NOT NULL,
                content_ciphertext BLOB NULL,
                content_nonce BLOB NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                sequence INTEGER NOT NULL,
                UNIQUE(conversation_id, sequence)
            );
            CREATE TABLE conversation_runs (
                conversation_id TEXT NOT NULL,
                message_id TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                relation TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                PRIMARY KEY(conversation_id, run_id)
            );
            CREATE TABLE turns (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                conversation_id TEXT NOT NULL,
                client_turn_id TEXT NOT NULL,
                request_id TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                state TEXT NOT NULL,
                effective_settings_json TEXT NOT NULL,
                settings_digest TEXT NOT NULL,
                model_routing_decision_json TEXT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                UNIQUE(conversation_id, idempotency_key)
            );
            CREATE TABLE runs (
                id TEXT PRIMARY KEY,
                turn_id TEXT NOT NULL,
                state TEXT NOT NULL,
                last_event_sequence INTEGER NOT NULL DEFAULT -1,
                lease_owner TEXT NULL,
                lease_expires_at_ms INTEGER NULL,
                fencing_token INTEGER NOT NULL DEFAULT 0,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            INSERT INTO projects(id, workspace_key, display_name, created_at_ms, updated_at_ms)
            VALUES('project-legacy', '/ws/legacy', 'Legacy', 1000, 1000);
            INSERT INTO conversations(
                id, project_id, title, title_source, status, pinned, archived,
                created_at_ms, updated_at_ms, last_message_at_ms, version)
            VALUES(
                'conversation-legacy', 'project-legacy', 'Legacy', 'generated', 'running',
                0, 0, 1000, 1000, 1000, 1);
            INSERT INTO messages(
                id, project_id, conversation_id, turn_id, role, state, content_redacted,
                created_at_ms, updated_at_ms, sequence)
            VALUES(
                'message-legacy', 'project-legacy', 'conversation-legacy', 'turn-legacy',
                'assistant', 'streaming', 'legacy assistant', 1000, 1000, 1);
            INSERT INTO turns(
                id, project_id, conversation_id, client_turn_id, request_id, idempotency_key,
                state, effective_settings_json, settings_digest, created_at_ms, updated_at_ms)
            VALUES(
                'turn-legacy', 'project-legacy', 'conversation-legacy', 'client-legacy',
                'request-legacy', 'idem-legacy', 'completed', '{}', 'sha256:v1:legacy',
                1000, 1000);
            INSERT INTO runs(
                id, turn_id, state, last_event_sequence, created_at_ms, updated_at_ms)
            VALUES('run-legacy', 'turn-legacy', 'completed', 42, 1000, 1000);
            INSERT INTO conversation_runs(
                conversation_id, message_id, turn_id, run_id, relation, created_at_ms)
            VALUES(
                'conversation-legacy', 'message-legacy', 'turn-legacy', 'run-legacy',
                'primary', 1000);
            ",
        )
        .unwrap();
        let repo = ConversationRepository { conn };
        assert!(repo.table_has_column("turns", "state").unwrap());
        assert!(repo.table_has_column("runs", "state").unwrap());

        repo.migrate().unwrap();

        assert!(!repo.table_has_column("turns", "state").unwrap());
        assert!(!repo.table_has_column("runs", "state").unwrap());
        let projection: (String, i64) = repo
            .conn
            .query_row(
                "SELECT state, last_sequence FROM run_projections WHERE run_id = 'run-legacy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(projection, ("completed".to_string(), 42));
        assert_eq!(
            repo.get_conversation("conversation-legacy")
                .unwrap()
                .unwrap()
                .status,
            ConversationStatus::Completed
        );
        let user_version: i32 = repo
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(user_version, MIGRATION_VERSION);
    }

    #[test]
    fn linked_run_without_projection_reads_unknown_not_completed() {
        // §6.7: a run with no projection must surface as `unknown`, never as a
        // fabricated `completed`. Recording the real state then reflects it.
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let msg = repo
            .append_message(
                &pid,
                &cid,
                "t1",
                MessageRole::Assistant,
                MessageState::Streaming,
                "...",
                2_000,
            )
            .unwrap();
        repo.link_run(&cid, &msg, "t1", "run-1", "primary", 2_000)
            .unwrap();

        let runs = repo.runs_for_conversation(&cid).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].run_state, None,
            "no projection ⇒ unknown, not completed"
        );
        assert_eq!(repo.run_projection_state("run-1").unwrap(), None);

        repo.upsert_run_projection("run-1", &pid, &cid, "t1", "failed", 2_100)
            .unwrap();
        let runs = repo.runs_for_conversation(&cid).unwrap();
        assert_eq!(runs[0].run_state.as_deref(), Some("failed"));

        // Upsert is idempotent and updates state in place.
        repo.upsert_run_projection("run-1", &pid, &cid, "t1", "completed", 2_200)
            .unwrap();
        assert_eq!(
            repo.run_projection_state("run-1").unwrap().as_deref(),
            Some("completed")
        );
    }

    fn sample_turn_start_request(
        project_id: &str,
        conversation_id: &str,
        request_id: &str,
        idempotency_key: &str,
    ) -> ConversationTurnStartRequest {
        ConversationTurnStartRequest {
            schema: opensks_contracts::CONVERSATION_TURN_START_REQUEST_SCHEMA.to_string(),
            request_id: request_id.to_string(),
            project_id: project_id.to_string(),
            conversation_id: conversation_id.to_string(),
            client_turn_id: format!("client-{request_id}"),
            message: opensks_contracts::UserMessageInput {
                text: "accept this turn".to_string(),
                attachment_refs: vec![],
            },
            thread_settings_updated_at_ms: None,
            settings: Some(opensks_contracts::ConversationTurnSettings {
                model: opensks_contracts::ModelSelection {
                    mode: opensks_contracts::ModelSelectionMode::Auto,
                    model_id: None,
                    fallback_model_ids: vec![],
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
            }),
            context: opensks_contracts::TurnContextSelection::default(),
            idempotency_key: idempotency_key.to_string(),
        }
    }

    fn execution_event(
        run_id: &str,
        id: &str,
        sequence: u64,
        kind: EventKind,
        payload: serde_json::Value,
    ) -> ExecutionEventEnvelope {
        ExecutionEventEnvelope {
            schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
            id: id.to_string(),
            run_id: run_id.to_string(),
            sequence,
            occurred_at: format!("{}.000000000", 1_700_000_000 + sequence),
            actor: "test".to_string(),
            causation_id: None,
            correlation_id: None,
            kind,
            payload,
            sensitivity: opensks_contracts::Sensitivity::Public,
            evidence_refs: vec!["test:event-journal".to_string()],
        }
    }

    #[test]
    fn accept_conversation_turn_is_durable_and_idempotent() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let request = sample_turn_start_request(&pid, &cid, "req-accept-1", "idem-accept-1");

        let accepted = repo.accept_conversation_turn(&request, 2_000).unwrap();
        assert_eq!(accepted.schema, CONVERSATION_TURN_ACCEPTED_SCHEMA);
        assert_eq!(accepted.request_id, "req-accept-1");
        assert_eq!(accepted.state, RunProjectionState::Queued);
        assert!(accepted.run_id.starts_with("turn-"));
        assert_eq!(accepted.stream_id, format!("stream-{}", accepted.turn_id));

        let messages = repo.message_page(&cid, None, 10).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::User);
        assert_eq!(messages[0].state, MessageState::Complete);
        assert_eq!(messages[1].role, MessageRole::Assistant);
        assert_eq!(messages[1].state, MessageState::Streaming);

        let runs = repo.runs_for_conversation(&cid).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, accepted.run_id);
        assert_eq!(runs[0].run_state.as_deref(), Some("queued"));
        assert_eq!(
            repo.run_last_event_sequence(&accepted.run_id).unwrap(),
            Some(0)
        );

        let replay = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-accept-replay", "idem-accept-1"),
                3_000,
            )
            .unwrap();
        assert_eq!(replay.turn_id, accepted.turn_id);
        assert_eq!(replay.run_id, accepted.run_id);
        assert_eq!(replay.user_message_id, accepted.user_message_id);
        assert_eq!(replay.assistant_message_id, accepted.assistant_message_id);
        assert_eq!(replay.request_id, "req-accept-replay");
        assert_eq!(repo.message_page(&cid, None, 10).unwrap().len(), 2);
    }

    #[test]
    fn idempotent_accept_fails_closed_when_projection_row_is_missing() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let first = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-missing-projection-1", "idem-corrupt"),
                2_000,
            )
            .unwrap();
        repo.conn
            .execute(
                "DELETE FROM run_projections WHERE run_id = ?1",
                params![first.run_id],
            )
            .unwrap();

        let error = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-missing-projection-2", "idem-corrupt"),
                2_010,
            )
            .unwrap_err();

        match error {
            ConversationError::ProjectionMissing(run_id) => assert_eq!(run_id, first.run_id),
            other => panic!("expected missing projection error, got {other:?}"),
        }
    }

    #[test]
    fn idempotent_accept_fails_closed_when_projection_state_is_unknown() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let first = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-unknown-projection-1", "idem-unknown"),
                2_000,
            )
            .unwrap();
        repo.conn
            .execute(
                "UPDATE run_projections SET state = 'materializing' WHERE run_id = ?1",
                params![first.run_id],
            )
            .unwrap();

        let error = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-unknown-projection-2", "idem-unknown"),
                2_010,
            )
            .unwrap_err();

        match error {
            ConversationError::InvalidRunProjectionState(state) => {
                assert_eq!(state, "materializing")
            }
            other => panic!("expected invalid projection state error, got {other:?}"),
        }
    }

    #[test]
    fn fresh_schema_omits_legacy_turn_and_run_state_columns() {
        let repo = ConversationRepository::open_memory().unwrap();

        assert!(!repo.table_has_column("turns", "state").unwrap());
        assert!(!repo.table_has_column("runs", "state").unwrap());
    }

    #[test]
    fn queued_claim_uses_run_projection_not_legacy_turn_run_state() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-stale-claim", "idem-stale-claim"),
                2_000,
            )
            .unwrap();
        repo.conn
            .execute(
                "UPDATE run_projections
                 SET state = 'completed'
                 WHERE run_id = ?1",
                params![accepted.run_id],
            )
            .unwrap();

        let claimed = repo
            .claim_next_queued_turn("supervisor-stale", 100, 2_010)
            .unwrap();
        assert!(
            claimed.is_none(),
            "queued turns/runs metadata must not be claimable once the projection is terminal"
        );
        assert_eq!(
            repo.run_projection_state(&accepted.run_id)
                .unwrap()
                .as_deref(),
            Some("completed")
        );
        assert!(!repo.table_has_column("turns", "state").unwrap());
        assert!(!repo.table_has_column("runs", "state").unwrap());
    }

    #[test]
    fn run_last_event_sequence_reads_projection_checkpoint_not_legacy_runs() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-projection-seq", "idem-proj-seq"),
                2_000,
            )
            .unwrap();
        repo.conn
            .execute(
                "UPDATE runs
                 SET last_event_sequence = 99
                 WHERE id = ?1",
                params![accepted.run_id],
            )
            .unwrap();
        repo.conn
            .execute(
                "UPDATE run_projections
                 SET last_sequence = 7
                 WHERE run_id = ?1",
                params![accepted.run_id],
            )
            .unwrap();

        assert_eq!(
            repo.run_last_event_sequence(&accepted.run_id).unwrap(),
            Some(7),
            "last event sequence is the projection checkpoint, not the stale legacy runs column"
        );
    }

    #[test]
    fn set_turn_run_state_updates_projection_checkpoint_and_conversation_status() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-compat-state", "idem-compat-state"),
                2_000,
            )
            .unwrap();

        repo.set_turn_run_state_with_last_sequence(
            &accepted.turn_id,
            &accepted.run_id,
            "completed",
            Some(11),
            2_020,
        )
        .unwrap();

        assert_eq!(
            repo.run_projection_state(&accepted.run_id)
                .unwrap()
                .as_deref(),
            Some("completed")
        );
        assert_eq!(
            repo.run_last_event_sequence(&accepted.run_id).unwrap(),
            Some(11)
        );
        assert_eq!(
            repo.get_conversation(&cid).unwrap().unwrap().status,
            ConversationStatus::Completed,
            "state setter refreshes conversation summary from projections"
        );

        repo.set_turn_run_state(&accepted.turn_id, &accepted.run_id, "running", 2_030)
            .unwrap();
        assert_eq!(
            repo.run_projection_state(&accepted.run_id)
                .unwrap()
                .as_deref(),
            Some("running")
        );
        assert_eq!(
            repo.run_last_event_sequence(&accepted.run_id).unwrap(),
            Some(11),
            "state-only update does not reset the projection checkpoint"
        );
    }

    #[test]
    fn queued_claim_uses_projection_after_legacy_state_columns_removed() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(
                    &pid,
                    &cid,
                    "req-projection-claim",
                    "idem-projection-claim",
                ),
                2_000,
            )
            .unwrap();
        assert!(!repo.table_has_column("turns", "state").unwrap());
        assert!(!repo.table_has_column("runs", "state").unwrap());

        let claimed = repo
            .claim_next_queued_turn("supervisor-projection", 100, 2_010)
            .unwrap()
            .expect("projection-queued run should be claimable");
        assert_eq!(claimed.run_id, accepted.run_id);
        assert_eq!(
            repo.run_projection_state(&accepted.run_id)
                .unwrap()
                .as_deref(),
            Some("running")
        );
        let lease_owner: Option<String> = repo
            .conn
            .query_row(
                "SELECT lease_owner FROM runs WHERE id = ?1",
                params![accepted.run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(lease_owner.as_deref(), Some("supervisor-projection"));
    }

    #[test]
    fn expired_lease_recovery_uses_run_projection_not_legacy_running_state() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-stale-recover", "idem-stale-recover"),
                2_000,
            )
            .unwrap();
        let claimed = repo
            .claim_next_queued_turn("supervisor-stale", 10, 2_010)
            .unwrap()
            .expect("claim accepted run");
        assert_eq!(claimed.run_id, accepted.run_id);
        repo.conn
            .execute(
                "UPDATE run_projections
                 SET state = 'completed'
                 WHERE run_id = ?1",
                params![accepted.run_id],
            )
            .unwrap();

        let recovered = repo.recover_expired_turn_supervisor_leases(2_021).unwrap();
        assert_eq!(
            recovered, 0,
            "running lease metadata must not requeue a terminal projection"
        );
        assert_eq!(
            repo.run_projection_state(&accepted.run_id)
                .unwrap()
                .as_deref(),
            Some("completed")
        );
        assert!(!repo.table_has_column("turns", "state").unwrap());
        assert!(!repo.table_has_column("runs", "state").unwrap());
    }

    #[test]
    fn heartbeat_and_recovery_use_projection_when_run_state_columns_are_absent() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(
                    &pid,
                    &cid,
                    "req-projection-lease",
                    "idem-projection-lease",
                ),
                2_000,
            )
            .unwrap();
        let claimed = repo
            .claim_next_queued_turn("supervisor-projection", 10, 2_010)
            .unwrap()
            .expect("claim projection-queued run");
        assert_eq!(claimed.run_id, accepted.run_id);
        assert!(!repo.table_has_column("turns", "state").unwrap());
        assert!(!repo.table_has_column("runs", "state").unwrap());

        assert!(
            repo.heartbeat_turn_supervisor_lease(
                &accepted.run_id,
                "supervisor-projection",
                claimed.fencing_token,
                30,
                2_015,
            )
            .unwrap(),
            "heartbeat should use projected running state"
        );
        let lease_expires_at_ms: i64 = repo
            .conn
            .query_row(
                "SELECT lease_expires_at_ms FROM runs WHERE id = ?1",
                params![accepted.run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(lease_expires_at_ms, 2_045);

        let recovered = repo.recover_expired_turn_supervisor_leases(2_046).unwrap();
        assert_eq!(
            recovered, 1,
            "expired recovery should requeue projected-running work"
        );
        assert_eq!(
            repo.run_projection_state(&accepted.run_id)
                .unwrap()
                .as_deref(),
            Some("queued")
        );
        let lease_owner: Option<String> = repo
            .conn
            .query_row(
                "SELECT lease_owner FROM runs WHERE id = ?1",
                params![accepted.run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(lease_owner, None);
    }

    #[test]
    fn heartbeat_rejects_terminal_projection() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(
                    &pid,
                    &cid,
                    "req-stale-heartbeat",
                    "idem-stale-heartbeat",
                ),
                2_000,
            )
            .unwrap();
        let claimed = repo
            .claim_next_queued_turn("supervisor-stale", 100, 2_010)
            .unwrap()
            .expect("claim accepted run");
        repo.conn
            .execute(
                "UPDATE run_projections
                 SET state = 'completed'
                 WHERE run_id = ?1",
                params![accepted.run_id],
            )
            .unwrap();

        let updated = repo
            .heartbeat_turn_supervisor_lease(
                &accepted.run_id,
                "supervisor-stale",
                claimed.fencing_token,
                100,
                2_020,
            )
            .unwrap();
        assert!(
            !updated,
            "running lease metadata must not accept heartbeat after projection becomes terminal"
        );
    }

    #[test]
    fn accept_conversation_turn_snapshots_persisted_thread_settings() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let thread_settings = ConversationThreadSettings {
            schema: opensks_contracts::CONVERSATION_THREAD_SETTINGS_SCHEMA.to_string(),
            conversation_id: cid.clone(),
            model_selection: opensks_contracts::ModelSelection {
                mode: opensks_contracts::ModelSelectionMode::Pinned,
                model_id: Some("openrouter/production-code-model".to_string()),
                fallback_model_ids: vec!["openrouter/fallback-code-model".to_string()],
            },
            reasoning_effort: opensks_contracts::ReasoningEffort::Deep,
            execution_mode: opensks_contracts::ExecutionMode::ReadOnly,
            pipeline_id: "parallel-build".to_string(),
            max_parallelism: 7,
            verifier_count: 3,
            tool_policy_id: "strict-tools".to_string(),
            approval_policy_id: "review-everything".to_string(),
            token_budget: Some(200_000),
            cost_budget_usd: Some(4.25),
            timeout_ms: Some(900_000),
            image_model_id: Some("image-model-a".to_string()),
            updated_at_ms: 1_900,
        };
        repo.set_thread_settings(
            &cid,
            &serde_json::to_string(&thread_settings).unwrap(),
            1_900,
        )
        .unwrap();

        let mut request =
            sample_turn_start_request(&pid, &cid, "req-thread-settings", "idem-thread-settings");
        request.thread_settings_updated_at_ms = Some(1_900);
        let legacy_settings = request.settings.as_mut().expect("legacy settings echo");
        legacy_settings.pipeline_id = "client-sent-ignored".to_string();
        legacy_settings.max_parallelism = 99;

        let accepted = repo.accept_conversation_turn(&request, 2_000).unwrap();
        let effective_raw = repo
            .turn_effective_settings_json(&accepted.turn_id)
            .unwrap()
            .expect("turn settings snapshot");
        let effective: ConversationTurnSettings = serde_json::from_str(&effective_raw).unwrap();
        let stored_digest = repo
            .turn_settings_digest(&accepted.turn_id)
            .unwrap()
            .expect("stored settings digest");
        assert_eq!(accepted.settings_digest, stored_digest);

        assert_eq!(effective.pipeline_id, "parallel-build");
        assert_eq!(effective.max_parallelism, 7);
        assert_eq!(effective.verifier_count, 3);
        assert_eq!(effective.tool_policy_id, "strict-tools");
        assert_eq!(effective.approval_policy_id, "review-everything");
        assert_eq!(effective.token_budget, Some(200_000));
        assert_eq!(effective.cost_budget_usd, Some(4.25));
        assert_eq!(effective.timeout_ms, Some(900_000));
        assert_eq!(
            effective.reasoning_effort,
            opensks_contracts::ReasoningEffort::Deep
        );
        assert_eq!(
            effective.execution_mode,
            opensks_contracts::ExecutionMode::ReadOnly
        );
        assert_eq!(
            effective.model.model_id.as_deref(),
            Some("openrouter/production-code-model")
        );
        assert_eq!(
            repo.run_projection_pipeline_id(&accepted.run_id)
                .unwrap()
                .as_deref(),
            Some("parallel-build")
        );
        let replayed = repo.accept_conversation_turn(&request, 2_050).unwrap();
        assert_eq!(replayed.turn_id, accepted.turn_id);
        assert_eq!(replayed.run_id, accepted.run_id);
        assert_eq!(replayed.settings_digest, accepted.settings_digest);
    }

    #[test]
    fn accept_conversation_turn_rejects_stale_thread_settings_revision() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let thread_settings = ConversationThreadSettings {
            schema: opensks_contracts::CONVERSATION_THREAD_SETTINGS_SCHEMA.to_string(),
            conversation_id: cid.clone(),
            model_selection: opensks_contracts::ModelSelection {
                mode: opensks_contracts::ModelSelectionMode::Auto,
                model_id: None,
                fallback_model_ids: Vec::new(),
            },
            reasoning_effort: opensks_contracts::ReasoningEffort::Standard,
            execution_mode: opensks_contracts::ExecutionMode::Worktree,
            pipeline_id: "auto".to_string(),
            max_parallelism: 4,
            verifier_count: 1,
            tool_policy_id: "project-default".to_string(),
            approval_policy_id: "safe-interactive".to_string(),
            token_budget: None,
            cost_budget_usd: None,
            timeout_ms: None,
            image_model_id: None,
            updated_at_ms: 2_500,
        };
        repo.set_thread_settings(
            &cid,
            &serde_json::to_string(&thread_settings).unwrap(),
            2_500,
        )
        .unwrap();

        let mut request =
            sample_turn_start_request(&pid, &cid, "req-stale-settings", "idem-stale-settings");
        request.thread_settings_updated_at_ms = Some(2_499);

        let err = repo.accept_conversation_turn(&request, 2_600).unwrap_err();
        assert!(
            matches!(
                err,
                ConversationError::StaleThreadSettingsRevision {
                    client_updated_at_ms: 2_499,
                    canonical_updated_at_ms: 2_500,
                    ..
                }
            ),
            "unexpected error: {err:?}"
        );
        assert!(
            repo.lookup_turn_idempotency(&request.idempotency_key, &cid)
                .unwrap()
                .is_none(),
            "stale settings revision must fail before accepting/idempotency persistence"
        );
    }

    #[test]
    fn accept_conversation_turn_accepts_default_thread_settings_revision_zero() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let mut request =
            sample_turn_start_request(&pid, &cid, "req-default-settings", "idem-default-settings");
        request.thread_settings_updated_at_ms = Some(0);

        let accepted = repo.accept_conversation_turn(&request, 2_000).unwrap();
        let effective_raw = repo
            .turn_effective_settings_json(&accepted.turn_id)
            .unwrap()
            .expect("turn settings snapshot");
        let effective: ConversationTurnSettings = serde_json::from_str(&effective_raw).unwrap();

        assert_eq!(effective.pipeline_id, "auto");
        assert_eq!(effective.max_parallelism, 4);
    }

    #[test]
    fn turn_supervisor_claims_target_run_before_older_queued_turn() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let first = repo
            .accept_conversation_turn(
                &sample_turn_start_request(
                    &pid,
                    &cid,
                    "req-target-supervisor-1",
                    "idem-target-supervisor-1",
                ),
                2_000,
            )
            .unwrap();
        let second = repo
            .accept_conversation_turn(
                &sample_turn_start_request(
                    &pid,
                    &cid,
                    "req-target-supervisor-2",
                    "idem-target-supervisor-2",
                ),
                2_010,
            )
            .unwrap();

        let claimed = repo
            .claim_queued_turn_by_run_id("supervisor-target", &second.run_id, 100, 2_020)
            .unwrap()
            .expect("target queued run should be claimed");

        assert_eq!(claimed.run_id, second.run_id);
        assert_eq!(
            repo.run_projection_state(&first.run_id).unwrap().as_deref(),
            Some("queued")
        );
        assert_eq!(
            repo.run_projection_state(&second.run_id)
                .unwrap()
                .as_deref(),
            Some("running")
        );
        let next = repo
            .claim_next_queued_turn("supervisor-next", 100, 2_030)
            .unwrap()
            .expect("older turn remains claimable");
        assert_eq!(next.run_id, first.run_id);
    }

    #[test]
    fn turn_supervisor_target_claim_can_heartbeat_immediately() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(
                    &pid,
                    &cid,
                    "req-target-supervisor-heartbeat",
                    "idem-target-supervisor-heartbeat",
                ),
                2_000,
            )
            .unwrap();
        let claimed = repo
            .claim_queued_turn_by_run_id(
                "supervisor-target-heartbeat",
                &accepted.run_id,
                30_000,
                2_010,
            )
            .unwrap()
            .expect("target queued run should be claimed");

        assert_eq!(
            repo.run_projection_state(&accepted.run_id)
                .unwrap()
                .as_deref(),
            Some("running")
        );
        assert!(
            repo.heartbeat_turn_supervisor_lease(
                &claimed.run_id,
                &claimed.lease_owner,
                claimed.fencing_token,
                30_000,
                2_020,
            )
            .unwrap(),
            "fresh target lease must be heartbeatable before event projection"
        );
    }

    #[test]
    fn turn_supervisor_claims_queued_turn_once_and_recovers_expired_lease() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let first = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-supervisor-1", "idem-supervisor-1"),
                2_000,
            )
            .unwrap();

        let claimed = repo
            .claim_next_queued_turn("supervisor-a", 100, 2_010)
            .unwrap()
            .expect("queued turn should be claimed");
        assert_eq!(claimed.turn_id, first.turn_id);
        assert_eq!(claimed.run_id, first.run_id);
        assert_eq!(claimed.lease_owner, "supervisor-a");
        assert_eq!(claimed.lease_expires_at_ms, 2_110);
        assert_eq!(claimed.fencing_token, 1);
        assert_eq!(
            repo.run_projection_state(&first.run_id).unwrap().as_deref(),
            Some("running")
        );

        assert_eq!(
            repo.claim_next_queued_turn("supervisor-b", 100, 2_020)
                .unwrap(),
            None,
            "a running leased turn must not be claimed twice"
        );
        assert!(
            repo.heartbeat_turn_supervisor_lease(
                &first.run_id,
                "supervisor-a",
                claimed.fencing_token,
                200,
                2_050,
            )
            .unwrap()
        );
        assert!(
            !repo
                .heartbeat_turn_supervisor_lease(
                    &first.run_id,
                    "wrong-supervisor",
                    claimed.fencing_token,
                    200,
                    2_060,
                )
                .unwrap()
        );

        assert_eq!(
            repo.recover_expired_turn_supervisor_leases(2_100).unwrap(),
            0
        );
        assert_eq!(
            repo.recover_expired_turn_supervisor_leases(2_251).unwrap(),
            1
        );
        assert_eq!(
            repo.run_projection_state(&first.run_id).unwrap().as_deref(),
            Some("queued")
        );

        let reclaimed = repo
            .claim_next_queued_turn("supervisor-b", 100, 2_260)
            .unwrap()
            .expect("expired turn should be claimable again");
        assert_eq!(reclaimed.run_id, first.run_id);
        assert_eq!(reclaimed.lease_owner, "supervisor-b");
        assert_eq!(reclaimed.fencing_token, 2);
        assert!(
            !repo
                .heartbeat_turn_supervisor_lease(
                    &first.run_id,
                    "supervisor-b",
                    claimed.fencing_token,
                    200,
                    2_270,
                )
                .unwrap(),
            "old fencing token must not heartbeat after a newer claim"
        );
        assert!(
            repo.finish_turn_supervisor_lease(&claimed, "failed", 6, "failed", 2_280,)
                .is_err(),
            "old fencing token must not finalize after a newer claim"
        );
        assert_eq!(
            repo.turn_user_message_text(&first.turn_id)
                .unwrap()
                .as_deref(),
            Some("accept this turn")
        );

        repo.finish_turn_supervisor_lease(&reclaimed, "completed", 7, "completed", 2_300)
            .unwrap();
        assert_eq!(
            repo.run_projection_state(&first.run_id).unwrap().as_deref(),
            Some("completed")
        );
        assert_eq!(
            repo.get_conversation(&cid).unwrap().unwrap().status,
            ConversationStatus::Completed
        );
        assert_eq!(
            repo.run_last_event_sequence(&first.run_id).unwrap(),
            Some(7)
        );
        let lease_owner: Option<String> = repo
            .conn
            .query_row(
                "SELECT lease_owner FROM runs WHERE id = ?1",
                params![first.run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(lease_owner, None);
    }

    #[test]
    fn finish_turn_supervisor_lease_after_projection_preserves_event_reducer_state() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(
                    &pid,
                    &cid,
                    "req-projection-finalize",
                    "idem-projection-finalize",
                ),
                2_000,
            )
            .unwrap();
        let lease = repo
            .claim_next_queued_turn("supervisor-projection-finalize", 100, 2_010)
            .unwrap()
            .expect("claim queued run");
        let events = vec![
            execution_event(
                &accepted.run_id,
                "evt-finalize-start",
                1,
                EventKind::RunStarted,
                serde_json::json!({"message": "run started"}),
            ),
            execution_event(
                &accepted.run_id,
                "evt-finalize-completed",
                2,
                EventKind::RunCompleted,
                serde_json::json!({"message": "run completed"}),
            ),
        ];
        let report = repo
            .project_execution_events_into_timeline(&accepted.run_id, &events, 2_020)
            .unwrap();
        assert_eq!(report.last_sequence, 2);
        assert_eq!(report.terminal_kind.as_deref(), Some("completed"));
        repo.conn
            .execute(
                "UPDATE runs
                 SET last_event_sequence = 0
                 WHERE id = ?1",
                params![accepted.run_id],
            )
            .unwrap();

        let finished = repo
            .finish_turn_supervisor_lease_after_projection(&lease, 2_030)
            .unwrap();

        assert_eq!(finished.state, "completed");
        assert_eq!(finished.last_event_sequence, 2);
        let metadata: (i64, Option<String>, Option<i64>) = repo
            .conn
            .query_row(
                "SELECT last_event_sequence, lease_owner, lease_expires_at_ms
                 FROM runs
                 WHERE id = ?1",
                params![accepted.run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(metadata, (2, None, None));
        assert!(!repo.table_has_column("turns", "state").unwrap());
        assert!(!repo.table_has_column("runs", "state").unwrap());
        let cursor = repo
            .stream_cursor_for_run(&accepted.run_id)
            .unwrap()
            .expect("stream cursor");
        assert_eq!(cursor.last_sequence, 2);
        assert_eq!(cursor.terminal_kind.as_deref(), Some("completed"));
        assert_eq!(
            repo.get_conversation(&cid).unwrap().unwrap().status,
            ConversationStatus::Completed
        );
    }

    #[test]
    fn timeline_replays_messages_with_run_projection_state() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-timeline", "idem-timeline"),
                2_000,
            )
            .unwrap();
        let lease = repo
            .claim_next_queued_turn("supervisor-timeline", 100, 2_005)
            .unwrap()
            .expect("claim timeline run");
        repo.finish_turn_supervisor_lease(&lease, "completed", 7, "completed", 2_010)
            .unwrap();

        let items = repo.timeline_items_for_conversation(&cid, 10).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, TimelineItemKind::UserMessage);
        assert_eq!(items[0].run_id, None);
        assert_eq!(items[0].payload["role"], "user");
        assert_eq!(items[1].kind, TimelineItemKind::AssistantMessage);
        assert_eq!(items[1].run_id.as_deref(), Some(accepted.run_id.as_str()));
        assert_eq!(items[1].state, "completed");
        assert_eq!(items[1].payload["message_state"], "streaming");
        assert_eq!(items[1].sequence, 2);
    }

    #[test]
    fn timeline_limit_preserves_recent_messages_when_event_window_is_full() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(
                    &pid,
                    &cid,
                    "req-timeline-event-window",
                    "idem-timeline-event-window",
                ),
                2_000,
            )
            .unwrap();
        let events = (1..=6)
            .map(|sequence| {
                execution_event(
                    &accepted.run_id,
                    &format!("evt-window-{sequence}"),
                    sequence,
                    EventKind::WorkItemRunning,
                    serde_json::json!({
                        "message": format!("worker event {sequence}"),
                        "work_item_id": "wi-window",
                        "to": "Running"
                    }),
                )
            })
            .collect::<Vec<_>>();
        repo.project_execution_events_into_timeline(&accepted.run_id, &events, 2_020)
            .unwrap();

        let items = repo.timeline_items_for_conversation(&cid, 3).unwrap();
        assert!(
            items.len() <= 3,
            "timeline limit must be enforced: {items:#?}"
        );
        assert!(
            items
                .iter()
                .any(|item| item.kind == TimelineItemKind::UserMessage),
            "message items must not be dropped behind high-sequence worker events: {items:#?}"
        );
        assert!(
            items
                .iter()
                .any(|item| item.kind == TimelineItemKind::AssistantMessage),
            "assistant placeholder must remain visible with the latest event window: {items:#?}"
        );
        assert!(
            items
                .iter()
                .any(|item| item.kind == TimelineItemKind::Worker),
            "the latest event window should still be included: {items:#?}"
        );
    }

    #[test]
    fn timeline_orders_newer_messages_after_older_high_sequence_events() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let first = repo
            .accept_conversation_turn(
                &sample_turn_start_request(
                    &pid,
                    &cid,
                    "req-timeline-order-first",
                    "idem-timeline-order-first",
                ),
                1_699_999_999_000,
            )
            .unwrap();
        let first_events = (1..=6)
            .map(|sequence| {
                execution_event(
                    &first.run_id,
                    &format!("evt-order-first-{sequence}"),
                    sequence,
                    EventKind::WorkItemRunning,
                    serde_json::json!({
                        "message": format!("older worker event {sequence}"),
                        "work_item_id": "wi-order",
                        "to": "Running"
                    }),
                )
            })
            .collect::<Vec<_>>();
        repo.project_execution_events_into_timeline(&first.run_id, &first_events, 2_050)
            .unwrap();

        let second = repo
            .accept_conversation_turn(
                &sample_turn_start_request(
                    &pid,
                    &cid,
                    "req-timeline-order-second",
                    "idem-timeline-order-second",
                ),
                1_700_000_010_000,
            )
            .unwrap();

        let items = repo.timeline_items_for_conversation(&cid, 20).unwrap();
        let last = items
            .last()
            .expect("newer queued assistant message should be visible");
        assert_eq!(last.kind, TimelineItemKind::AssistantMessage);
        assert_eq!(last.turn_id.as_deref(), Some(second.turn_id.as_str()));
        assert_eq!(
            last.run_id.as_deref(),
            Some(second.run_id.as_str()),
            "older high-sequence worker events must not sort below a newer queued turn: {items:#?}"
        );
    }

    #[test]
    fn execution_event_replay_materializes_timeline_items_once_and_updates_cursor() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-timeline-events", "idem-tl-events"),
                2_000,
            )
            .unwrap();
        let events = vec![
            execution_event(
                &accepted.run_id,
                "evt-run-started",
                1,
                EventKind::RunStarted,
                serde_json::json!({"message": "run started"}),
            ),
            execution_event(
                &accepted.run_id,
                "evt-worker-running",
                2,
                EventKind::WorkItemRunning,
                serde_json::json!({
                    "message": "worker running",
                    "work_item_id": "wi-1",
                    "to": "Running"
                }),
            ),
            execution_event(
                &accepted.run_id,
                "evt-verification-failed",
                3,
                EventKind::VerificationFailed,
                serde_json::json!({"message": "Needs setup"}),
            ),
        ];

        let first = repo
            .project_execution_events_into_timeline(&accepted.run_id, &events, 2_010)
            .unwrap();
        assert_eq!(first.projected_count, 3);
        assert_eq!(first.duplicate_count, 0);
        assert_eq!(first.skipped_count, 0);
        assert_eq!(first.last_sequence, 3);
        assert_eq!(first.terminal_kind.as_deref(), Some("failed"));
        assert_eq!(
            repo.run_projection_state(&accepted.run_id)
                .unwrap()
                .as_deref(),
            Some("failed")
        );
        assert_eq!(
            repo.get_conversation(&cid).unwrap().unwrap().status,
            ConversationStatus::Failed,
            "terminal replayed events update the conversation summary read model"
        );

        let replay = repo
            .project_execution_events_into_timeline(&accepted.run_id, &events, 2_020)
            .unwrap();
        assert_eq!(replay.projected_count, 0);
        assert_eq!(replay.duplicate_count, 3);
        assert_eq!(replay.last_sequence, 3);

        let cursor = repo
            .stream_cursor_for_run(&accepted.run_id)
            .unwrap()
            .expect("stream cursor");
        assert_eq!(cursor.stream_id, accepted.stream_id);
        assert_eq!(cursor.last_sequence, 3);
        assert_eq!(cursor.terminal_kind.as_deref(), Some("failed"));
        let stream_metadata = repo
            .stream_metadata_for_run(&accepted.run_id)
            .unwrap()
            .expect("stream metadata");
        assert_eq!(stream_metadata.stream_id, accepted.stream_id);
        assert_eq!(stream_metadata.run_id, accepted.run_id);
        assert_eq!(stream_metadata.project_id, pid);
        assert_eq!(stream_metadata.conversation_id, cid);
        assert_eq!(stream_metadata.turn_id, accepted.turn_id);

        let items = repo.timeline_items_for_conversation(&cid, 20).unwrap();
        assert_eq!(items.len(), 5);
        let event_items: Vec<_> = items
            .iter()
            .filter(|item| item.id.starts_with("timeline-event-"))
            .collect();
        assert_eq!(event_items.len(), 3);
        assert_eq!(event_items[0].sequence, 2_000_001);
        assert_eq!(event_items[1].kind, TimelineItemKind::Worker);
        assert_eq!(event_items[1].payload["event_sequence"], 2);
        assert_eq!(event_items[2].kind, TimelineItemKind::Error);
        assert_eq!(event_items[2].payload["content_redacted"], "Needs setup");
    }

    #[test]
    fn execution_event_replay_projects_run_lifecycle_without_downgrades() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-run-state-events", "idem-run-state"),
                2_000,
            )
            .unwrap();

        let active_events = vec![
            execution_event(
                &accepted.run_id,
                "evt-start",
                1,
                EventKind::RunStarted,
                serde_json::json!({"message": "run started"}),
            ),
            execution_event(
                &accepted.run_id,
                "evt-paused",
                2,
                EventKind::RunPaused,
                serde_json::json!({"message": "paused"}),
            ),
            execution_event(
                &accepted.run_id,
                "evt-resumed",
                3,
                EventKind::RunResumed,
                serde_json::json!({"message": "resumed"}),
            ),
        ];
        repo.project_execution_events_into_timeline(&accepted.run_id, &active_events, 2_010)
            .unwrap();
        assert_eq!(
            repo.run_projection_state(&accepted.run_id)
                .unwrap()
                .as_deref(),
            Some("running")
        );

        let terminal_events = vec![
            execution_event(
                &accepted.run_id,
                "evt-failed",
                4,
                EventKind::VerificationFailed,
                serde_json::json!({"message": "verification failed"}),
            ),
            execution_event(
                &accepted.run_id,
                "evt-late-resume",
                5,
                EventKind::RunResumed,
                serde_json::json!({"message": "late resume"}),
            ),
            execution_event(
                &accepted.run_id,
                "evt-snapshot",
                6,
                EventKind::SnapshotWritten,
                serde_json::json!({"state": "completed", "message": "snapshot"}),
            ),
            execution_event(
                &accepted.run_id,
                "evt-late-completed",
                7,
                EventKind::RunCompleted,
                serde_json::json!({"message": "late completed"}),
            ),
        ];
        repo.project_execution_events_into_timeline(&accepted.run_id, &terminal_events, 2_020)
            .unwrap();
        assert_eq!(
            repo.run_projection_state(&accepted.run_id)
                .unwrap()
                .as_deref(),
            Some("failed"),
            "terminal event-derived state is sticky and snapshot metadata cannot overwrite it"
        );
    }

    #[test]
    fn execution_event_replay_keeps_role_worker_failures_diagnostic_after_run_completed() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(
                    &pid,
                    &cid,
                    "req-role-worker-diagnostic",
                    "idem-role-worker-diagnostic",
                ),
                2_000,
            )
            .unwrap();

        let events = vec![
            execution_event(
                &accepted.run_id,
                "evt-start",
                1,
                EventKind::RunStarted,
                serde_json::json!({"message": "run started"}),
            ),
            execution_event(
                &accepted.run_id,
                "evt-role-worker-failed",
                2,
                EventKind::VerificationFailed,
                serde_json::json!({
                    "agent_event_kind": "error",
                    "worker_id": "role-subcontract-turn-role-verification",
                    "payload": {
                        "code": "role_worker_model_call_failed",
                        "reason_code": "provider_role_call_failed",
                        "content_redacted": true
                    }
                }),
            ),
            execution_event(
                &accepted.run_id,
                "evt-role-work-item-failed",
                3,
                EventKind::VerificationFailed,
                serde_json::json!({
                    "work_item_id": "turn-role-abc-0-verification",
                    "lease_holder": "turn-supervisor",
                    "from": "Running",
                    "to": "Failed"
                }),
            ),
            execution_event(
                &accepted.run_id,
                "evt-completed",
                4,
                EventKind::RunCompleted,
                serde_json::json!({"message": "root assistant completed"}),
            ),
        ];

        let report = repo
            .project_execution_events_into_timeline(&accepted.run_id, &events, 2_010)
            .unwrap();
        assert_eq!(report.terminal_kind.as_deref(), Some("completed"));
        assert_eq!(
            repo.run_projection_state(&accepted.run_id)
                .unwrap()
                .as_deref(),
            Some("completed")
        );
        assert_eq!(
            repo.get_conversation(&cid).unwrap().unwrap().status,
            ConversationStatus::Completed
        );
        let items = repo.timeline_items_for_conversation(&cid, 20).unwrap();
        assert!(
            items.iter().any(|item| {
                item.id == "timeline-event-evt-role-worker-failed"
                    && item.kind == TimelineItemKind::Error
                    && item.payload["worker_id"] == "role-subcontract-turn-role-verification"
                    && item.payload["code"] == "role_worker_model_call_failed"
            }),
            "role worker failure should remain visible as diagnostics: {items:#?}"
        );
    }

    #[test]
    fn execution_event_replay_keeps_summary_running_while_other_projected_runs_are_active() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let first = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-projected-active-1", "idem-proj-1"),
                2_000,
            )
            .unwrap();
        let second = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-projected-active-2", "idem-proj-2"),
                2_010,
            )
            .unwrap();

        let first_failed_events = vec![
            execution_event(
                &first.run_id,
                "evt-first-start",
                1,
                EventKind::RunStarted,
                serde_json::json!({"message": "first started"}),
            ),
            execution_event(
                &first.run_id,
                "evt-first-failed",
                2,
                EventKind::VerificationFailed,
                serde_json::json!({"message": "first failed"}),
            ),
        ];
        repo.project_execution_events_into_timeline(&first.run_id, &first_failed_events, 2_020)
            .unwrap();
        assert_eq!(
            repo.run_projection_state(&first.run_id).unwrap().as_deref(),
            Some("failed")
        );
        assert_eq!(
            repo.get_conversation(&cid).unwrap().unwrap().status,
            ConversationStatus::Running,
            "second queued projection keeps the conversation summary active"
        );

        let second_failed_events = vec![
            execution_event(
                &second.run_id,
                "evt-second-start",
                1,
                EventKind::RunStarted,
                serde_json::json!({"message": "second started"}),
            ),
            execution_event(
                &second.run_id,
                "evt-second-failed",
                2,
                EventKind::VerificationFailed,
                serde_json::json!({"message": "second failed"}),
            ),
        ];
        repo.project_execution_events_into_timeline(&second.run_id, &second_failed_events, 2_030)
            .unwrap();
        assert_eq!(
            repo.get_conversation(&cid).unwrap().unwrap().status,
            ConversationStatus::Failed,
            "once all projected runs are terminal, the latest terminal state drives the summary"
        );
    }

    #[test]
    fn execution_event_rebuild_recomputes_from_events_instead_of_stale_projection_state() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-rebuild-events", "idem-rebuild"),
                2_000,
            )
            .unwrap();
        let events = vec![
            execution_event(
                &accepted.run_id,
                "evt-rebuild-start",
                1,
                EventKind::RunStarted,
                serde_json::json!({"message": "run started"}),
            ),
            execution_event(
                &accepted.run_id,
                "evt-rebuild-worker",
                2,
                EventKind::WorkItemRunning,
                serde_json::json!({"message": "worker running"}),
            ),
            execution_event(
                &accepted.run_id,
                "evt-rebuild-completed",
                3,
                EventKind::RunCompleted,
                serde_json::json!({"message": "run completed"}),
            ),
        ];
        repo.conn
            .execute(
                "UPDATE run_projections
                 SET state = 'failed',
                     last_sequence = 99
                 WHERE run_id = ?1",
                params![accepted.run_id],
            )
            .unwrap();
        repo.conn
            .execute(
                "UPDATE stream_cursors
                 SET last_sequence = 99,
                     terminal_kind = 'failed'
                 WHERE run_id = ?1",
                params![accepted.run_id],
            )
            .unwrap();
        repo.conn
            .execute(
                "INSERT INTO timeline_items(
                    id, project_id, conversation_id, turn_id, run_id, sequence,
                    kind, state, payload_redacted_json, created_at_ms, updated_at_ms
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    "timeline-event-stale",
                    pid,
                    cid,
                    accepted.turn_id,
                    accepted.run_id,
                    2_999_999_i64,
                    enum_to_str(&TimelineItemKind::Error),
                    "verification_failed",
                    "{}",
                    1_900_i64,
                    1_900_i64,
                ],
            )
            .unwrap();
        repo.set_status(&cid, ConversationStatus::Failed, 1_990)
            .unwrap();

        let report = repo
            .rebuild_execution_events_into_timeline(&accepted.run_id, &events, 2_020)
            .unwrap();
        assert_eq!(report.projected_count, 3);
        assert_eq!(report.duplicate_count, 0);
        assert_eq!(report.last_sequence, 3);
        assert_eq!(report.terminal_kind.as_deref(), Some("completed"));
        assert_eq!(
            repo.run_projection_state(&accepted.run_id)
                .unwrap()
                .as_deref(),
            Some("completed"),
            "full replay derives terminal completion from queued + events, not stale failed projection rows"
        );
        let cursor = repo
            .stream_cursor_for_run(&accepted.run_id)
            .unwrap()
            .expect("stream cursor");
        assert_eq!(cursor.last_sequence, 3);
        assert_eq!(cursor.terminal_kind.as_deref(), Some("completed"));
        assert_eq!(
            repo.get_conversation(&cid).unwrap().unwrap().status,
            ConversationStatus::Completed
        );

        let items = repo.timeline_items_for_conversation(&cid, 20).unwrap();
        assert!(!items.iter().any(|item| item.id == "timeline-event-stale"));
        let event_count = items
            .iter()
            .filter(|item| item.id.starts_with("timeline-event-"))
            .count();
        assert_eq!(event_count, 3);
    }

    #[test]
    fn execution_event_timeline_projection_redacts_payload_strings() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-timeline-redact", "idem-tl-redact"),
                2_000,
            )
            .unwrap();
        let secret = concat!("sk-", "or-v1-deadbeefdeadbeefdeadbeefdeadbeef");
        let events = vec![execution_event(
            &accepted.run_id,
            "evt-secret",
            1,
            EventKind::WorkItemRunning,
            serde_json::json!({
                "message": format!("Authorization: Bearer {secret}"),
                "nested": { "token": secret }
            }),
        )];

        let report = repo
            .project_execution_events_into_timeline(&accepted.run_id, &events, 2_010)
            .unwrap();
        assert_eq!(report.projected_count, 1);
        let items = repo.timeline_items_for_conversation(&cid, 10).unwrap();
        let event_item = items
            .iter()
            .find(|item| item.id == "timeline-event-evt-secret")
            .expect("event timeline item");
        let serialized = serde_json::to_string(&event_item.payload).unwrap();
        assert!(!serialized.contains(secret));
        assert!(serialized.contains("[REDACTED]"));
    }

    #[test]
    fn execution_event_replay_flattens_tool_worker_patch_details() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(
                    &pid,
                    &cid,
                    "req-timeline-details",
                    "idem-timeline-details",
                ),
                2_000,
            )
            .unwrap();
        let events = vec![
            execution_event(
                &accepted.run_id,
                "evt-tool-details",
                1,
                EventKind::WorkItemCompleted,
                serde_json::json!({
                    "agent_event_kind": "tool_call_completed",
                    "worker_id": "worker-code",
                    "work_item_id": "work-code",
                    "payload": {
                        "tool": "test.run_targeted",
                        "command_redacted": "cargo test -p opensks-cli push_cli",
                        "exit_code": 0,
                        "duration_ms": 42,
                        "timed_out": false,
                        "test_targets": ["opensks-cli::push_cli"]
                    }
                }),
            ),
            execution_event(
                &accepted.run_id,
                "evt-patch-details",
                2,
                EventKind::WorkItemRunning,
                serde_json::json!({
                    "agent_event_kind": "file_patch_applied",
                    "worker_id": "worker-code",
                    "payload": {
                        "code": "patch_applied",
                        "applied_files": ["crates/opensks-cli/src/lib.rs"],
                        "patch_count": 1,
                        "apply_result_count": 1,
                        "main_workspace_modified": false
                    }
                }),
            ),
        ];

        repo.project_execution_events_into_timeline(&accepted.run_id, &events, 2_010)
            .unwrap();

        let items = repo.timeline_items_for_conversation(&cid, 20).unwrap();
        let tool = items
            .iter()
            .find(|item| item.id == "timeline-event-evt-tool-details")
            .expect("tool timeline item");
        assert_eq!(tool.kind, TimelineItemKind::ToolCall);
        assert_eq!(tool.payload["agent_event_kind"], "tool_call_completed");
        assert_eq!(tool.payload["worker_id"], "worker-code");
        assert_eq!(tool.payload["work_item_id"], "work-code");
        assert_eq!(tool.payload["tool"], "test.run_targeted");
        assert_eq!(
            tool.payload["command_redacted"],
            "cargo test -p opensks-cli push_cli"
        );
        assert_eq!(tool.payload["exit_code"], 0);
        assert_eq!(tool.payload["duration_ms"], 42);
        assert_eq!(tool.payload["timed_out"], false);
        assert_eq!(tool.payload["test_targets"][0], "opensks-cli::push_cli");

        let patch = items
            .iter()
            .find(|item| item.id == "timeline-event-evt-patch-details")
            .expect("patch timeline item");
        assert_eq!(patch.kind, TimelineItemKind::Patch);
        assert_eq!(patch.payload["agent_event_kind"], "file_patch_applied");
        assert_eq!(patch.payload["worker_id"], "worker-code");
        assert_eq!(patch.payload["code"], "patch_applied");
        assert_eq!(
            patch.payload["applied_files"][0],
            "crates/opensks-cli/src/lib.rs"
        );
        assert_eq!(patch.payload["patch_count"], 1);
        assert_eq!(patch.payload["apply_result_count"], 1);
        assert_eq!(patch.payload["main_workspace_modified"], false);
    }

    #[test]
    fn execution_event_replay_flattens_assistant_text_event_details() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(
                    &pid,
                    &cid,
                    "req-timeline-assistant",
                    "idem-timeline-assistant",
                ),
                2_000,
            )
            .unwrap();
        let secret = concat!("sk-", "or-v1-assistantsecret00000000000000000000");
        let events = vec![
            execution_event(
                &accepted.run_id,
                "evt-assistant-delta",
                1,
                EventKind::WorkItemRunning,
                serde_json::json!({
                    "agent_event_kind": "assistant_text_delta",
                    "payload": {
                        "delta": "Drafting the release note",
                        "model_id": "model-writer"
                    }
                }),
            ),
            execution_event(
                &accepted.run_id,
                "evt-assistant-completed",
                2,
                EventKind::WorkItemCompleted,
                serde_json::json!({
                    "agent_event_kind": "assistant_text_completed",
                    "payload": {
                        "text": format!("Release note finished with token {secret}."),
                        "response_hash": "sha256:assistant-response",
                        "response_bytes": 128,
                        "finish_reason": "stop",
                        "model_id": "model-writer"
                    }
                }),
            ),
        ];

        repo.project_execution_events_into_timeline(&accepted.run_id, &events, 2_010)
            .unwrap();

        let items = repo.timeline_items_for_conversation(&cid, 20).unwrap();
        let delta = items
            .iter()
            .find(|item| item.id == "timeline-event-evt-assistant-delta")
            .expect("assistant delta timeline item");
        assert_eq!(delta.kind, TimelineItemKind::AssistantMessage);
        assert_eq!(delta.state, "streaming");
        assert_eq!(delta.payload["projection"], "assistant_execution_event");
        assert_eq!(delta.payload["agent_event_kind"], "assistant_text_delta");
        assert_eq!(
            delta.payload["assistant_message_id"],
            accepted.assistant_message_id
        );
        assert_eq!(
            delta.payload["assistant_delta"],
            "Drafting the release note"
        );
        assert_eq!(delta.payload["model_id"], "model-writer");

        let completed = items
            .iter()
            .find(|item| item.id == "timeline-event-evt-assistant-completed")
            .expect("assistant completed timeline item");
        assert_eq!(completed.kind, TimelineItemKind::AssistantMessage);
        assert_eq!(completed.state, "completed");
        assert_eq!(
            completed.payload["agent_event_kind"],
            "assistant_text_completed"
        );
        assert_eq!(
            completed.payload["assistant_message_id"],
            accepted.assistant_message_id
        );
        assert!(
            completed.payload["assistant_text"]
                .as_str()
                .unwrap()
                .contains("Release note finished")
        );
        let serialized = serde_json::to_string(&completed.payload).unwrap();
        assert!(!serialized.contains(secret));
        assert!(serialized.contains("[REDACTED]"));
        assert_eq!(
            completed.payload["response_hash"],
            "sha256:assistant-response"
        );
        assert_eq!(completed.payload["response_bytes"], 128);
        assert_eq!(completed.payload["completion_reason"], "stop");
    }

    #[test]
    fn execution_event_replay_materializes_git_receipts_as_typed_timeline_items() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(
                    &pid,
                    &cid,
                    "req-timeline-git-events",
                    "idem-tl-git-events",
                ),
                2_000,
            )
            .unwrap();
        let events = vec![
            execution_event(
                &accepted.run_id,
                "evt-git-commit",
                2,
                EventKind::GitCommitReceipt,
                serde_json::json!({
                    "content_redacted": "Commit deadbeef recorded.",
                    "commit": "deadbeefcafef00d",
                    "paths": ["src/lib.rs", "README.md"],
                    "message": "ship it",
                    "committed": true,
                    "reviewed_staged_diff_hash": "fnv1a64:revieweddiff",
                    "reviewed_staged_diff_ref": "git-staged-diff://fnv1a64:revieweddiff",
                    "integration_final_diff_hash": "fnv1a64:finaldiff",
                    "integration_final_diff_ref": "artifact://.opensks/runtime/integration-candidates/run-1/final.diff",
                    "integration_run_id": "run-1",
                    "integration_candidate_id": "candidate-1"
                }),
            ),
            execution_event(
                &accepted.run_id,
                "evt-git-push",
                3,
                EventKind::GitPushReceipt,
                serde_json::json!({
                    "content_redacted": "Push cafebabe to origin/feature recorded.",
                    "remote": "origin",
                    "ref": "feature",
                    "remote_oid": "cafebabecafebabe",
                    "local_oid": "feedfacefeedface",
                    "already_done": false,
                    "pushed": true,
                    "intent_id": "intent-1",
                    "effect_digest": "fnv1a64:1234",
                    "idempotency_key": "push:intent-1:feedface",
                    "remote_url_redacted": "https://github.com/acme/repo.git",
                    "approval_id": "approval-1",
                    "approval_matched": true
                }),
            ),
            execution_event(
                &accepted.run_id,
                "evt-git-push-failed",
                4,
                EventKind::GitPushFailed,
                serde_json::json!({
                    "content_redacted": "Push to origin/feature failed after approval.",
                    "remote": "origin",
                    "ref": "feature",
                    "local_oid": "feedfacefeedface",
                    "pushed": false,
                    "intent_id": "intent-1",
                    "idempotency_key": "push:intent-1:feedface",
                    "reason_code": "push_failed",
                    "diagnostic_id": "push:intent-1:feedface"
                }),
            ),
        ];

        let report = repo
            .project_execution_events_into_timeline(&accepted.run_id, &events, 2_010)
            .unwrap();
        assert_eq!(report.projected_count, 3);
        let replay = repo
            .project_execution_events_into_timeline(&accepted.run_id, &events, 2_020)
            .unwrap();
        assert_eq!(
            replay.duplicate_count, 3,
            "git receipt events replay idempotently"
        );

        let items = repo.timeline_items_for_conversation(&cid, 20).unwrap();
        let commit = items
            .iter()
            .find(|item| item.id == "timeline-event-evt-git-commit")
            .expect("commit receipt event");
        assert_eq!(commit.kind, TimelineItemKind::CommitReceipt);
        assert_eq!(commit.state, "committed");
        assert_eq!(commit.payload["projection"], "git_receipt_event");
        assert_eq!(commit.payload["commit"], "deadbeefcafef00d");
        assert_eq!(commit.payload["paths"][0], "src/lib.rs");
        assert_eq!(
            commit.payload["reviewed_staged_diff_hash"],
            "fnv1a64:revieweddiff"
        );
        assert_eq!(
            commit.payload["reviewed_staged_diff_ref"],
            "git-staged-diff://fnv1a64:revieweddiff"
        );
        assert_eq!(
            commit.payload["integration_final_diff_ref"],
            "artifact://.opensks/runtime/integration-candidates/run-1/final.diff"
        );
        assert_eq!(commit.payload["integration_run_id"], "run-1");
        assert_eq!(
            commit.payload["payload_redacted"]["commit"],
            "deadbeefcafef00d"
        );

        let push = items
            .iter()
            .find(|item| item.id == "timeline-event-evt-git-push")
            .expect("push receipt event");
        assert_eq!(push.kind, TimelineItemKind::PushReceipt);
        assert_eq!(push.state, "pushed");
        assert_eq!(push.payload["remote"], "origin");
        assert_eq!(push.payload["ref"], "feature");
        assert_eq!(push.payload["remote_oid"], "cafebabecafebabe");
        assert_eq!(push.payload["local_oid"], "feedfacefeedface");
        assert_eq!(push.payload["idempotency_key"], "push:intent-1:feedface");

        let failed = items
            .iter()
            .find(|item| item.id == "timeline-event-evt-git-push-failed")
            .expect("failed push receipt event");
        assert_eq!(failed.kind, TimelineItemKind::PushReceipt);
        assert_eq!(failed.state, "failed");
        assert_eq!(failed.payload["reason_code"], "push_failed");
        assert_eq!(failed.payload["pushed"], false);
    }

    #[test]
    fn execution_event_replay_materializes_image_artifacts_as_typed_timeline_items() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(
                    &pid,
                    &cid,
                    "req-timeline-image-events",
                    "idem-tl-image-events",
                ),
                2_000,
            )
            .unwrap();
        let events = vec![execution_event(
            &accepted.run_id,
            "evt-image-artifact",
            2,
            EventKind::ImageArtifactCreated,
            serde_json::json!({
                "content_redacted": "Image artifact cli-image-asset created.",
                "asset_id": "cli-image-asset",
                "provider_id": "provider-1",
                "model_id": "provider-1/image-model",
                "path": ".opensks/assets/candidates/cli-image-asset.ppm",
                "content_hash": "sha256:v1:assetbytes",
                "provenance_hash": "sha256:v1:provenance",
                "operation": "generate",
                "width": 512,
                "height": 512
            }),
        )];

        let report = repo
            .project_execution_events_into_timeline(&accepted.run_id, &events, 2_010)
            .unwrap();
        assert_eq!(report.projected_count, 1);
        let replay = repo
            .project_execution_events_into_timeline(&accepted.run_id, &events, 2_020)
            .unwrap();
        assert_eq!(
            replay.duplicate_count, 1,
            "image artifact events replay idempotently"
        );

        let items = repo.timeline_items_for_conversation(&cid, 20).unwrap();
        let image = items
            .iter()
            .find(|item| item.id == "timeline-event-evt-image-artifact")
            .expect("image artifact event");
        assert_eq!(image.kind, TimelineItemKind::ImageArtifact);
        assert_eq!(image.state, "created");
        assert_eq!(image.payload["projection"], "image_artifact_event");
        assert_eq!(image.payload["asset_id"], "cli-image-asset");
        assert_eq!(image.payload["provider_id"], "provider-1");
        assert_eq!(image.payload["model_id"], "provider-1/image-model");
        assert_eq!(
            image.payload["path"],
            ".opensks/assets/candidates/cli-image-asset.ppm"
        );
        assert_eq!(image.payload["content_hash"], "sha256:v1:assetbytes");
        assert_eq!(image.payload["provenance_hash"], "sha256:v1:provenance");
        assert_eq!(image.payload["operation"], "generate");
        assert_eq!(image.payload["width"], 512);
        assert_eq!(image.payload["height"], 512);
    }

    #[test]
    fn timeline_replays_persisted_git_receipts_in_sequence_order() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-tl-git", "idem-tl-git"),
                2_000,
            )
            .unwrap();
        let item = repo
            .append_timeline_item(
                &pid,
                &cid,
                TimelineItemKind::CommitReceipt,
                "committed",
                serde_json::json!({
                    "content_redacted": "Commit deadbeef",
                    "commit": "deadbeefcafef00d",
                    "paths": ["a.rs"],
                    "message": "ship it",
                    "projection": "git_receipt"
                }),
                2_020,
            )
            .unwrap();

        assert_eq!(item.sequence, 3);
        let items = repo.timeline_items_for_conversation(&cid, 10).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[1].run_id.as_deref(), Some(accepted.run_id.as_str()));
        assert_eq!(items[2].kind, TimelineItemKind::CommitReceipt);
        assert_eq!(items[2].state, "committed");
        assert_eq!(items[2].payload["commit"], "deadbeefcafef00d");
    }

    #[test]
    fn finishing_one_turn_keeps_conversation_running_while_other_runs_are_active() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let first = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-active-1", "idem-active-1"),
                2_000,
            )
            .unwrap();
        let second = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-active-2", "idem-active-2"),
                2_010,
            )
            .unwrap();
        let claimed_first = repo
            .claim_next_queued_turn("supervisor-a", 100, 2_020)
            .unwrap()
            .expect("first turn claimed");
        assert_eq!(claimed_first.run_id, first.run_id);

        repo.finish_turn_supervisor_lease(&claimed_first, "completed", 4, "completed", 2_030)
            .unwrap();
        assert_eq!(
            repo.get_conversation(&cid).unwrap().unwrap().status,
            ConversationStatus::Running,
            "second queued run keeps the conversation summary active"
        );

        let claimed_second = repo
            .claim_next_queued_turn("supervisor-b", 100, 2_040)
            .unwrap()
            .expect("second turn claimed");
        assert_eq!(claimed_second.run_id, second.run_id);
        repo.finish_turn_supervisor_lease(&claimed_second, "failed", 5, "failed", 2_050)
            .unwrap();
        assert_eq!(
            repo.get_conversation(&cid).unwrap().unwrap().status,
            ConversationStatus::Failed
        );
    }

    #[test]
    fn migrate_repairs_stale_running_summary_when_no_run_is_active() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let _accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-repair-1", "idem-repair-1"),
                2_000,
            )
            .unwrap();
        let lease = repo
            .claim_next_queued_turn("supervisor-repair", 100, 2_005)
            .unwrap()
            .expect("claim repair run");
        repo.finish_turn_supervisor_lease(&lease, "completed", 9, "completed", 2_010)
            .unwrap();
        repo.set_status(&cid, ConversationStatus::Running, 2_020)
            .unwrap();
        assert_eq!(
            repo.get_conversation(&cid).unwrap().unwrap().status,
            ConversationStatus::Running
        );
        assert!(!repo.table_has_column("turns", "state").unwrap());
        assert!(!repo.table_has_column("runs", "state").unwrap());

        repo.repair_conversation_statuses().unwrap();
        assert_eq!(
            repo.get_conversation(&cid).unwrap().unwrap().status,
            ConversationStatus::Completed
        );
    }

    #[test]
    fn create_list_rename_pin_archive_delete() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        repo.append_message(
            &pid,
            &cid,
            "t1",
            MessageRole::User,
            MessageState::Complete,
            "hello world",
            2_000,
        )
        .unwrap();
        repo.append_message(
            &pid,
            &cid,
            "t1",
            MessageRole::Assistant,
            MessageState::Complete,
            "hi there",
            2_001,
        )
        .unwrap();

        let listed = repo
            .list_conversations(&pid, ConversationFilter::All, 50)
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].message_count, 2);

        repo.rename_conversation(&cid, "Renamed", 3_000).unwrap();
        assert_eq!(
            repo.get_conversation(&cid).unwrap().unwrap().title,
            "Renamed"
        );
        assert_eq!(
            repo.get_conversation(&cid).unwrap().unwrap().title_source,
            TitleSource::User
        );

        repo.set_pinned(&cid, true, 3_100).unwrap();
        assert_eq!(
            repo.list_conversations(&pid, ConversationFilter::Pinned, 50)
                .unwrap()
                .len(),
            1
        );

        repo.set_archived(&cid, true, 3_200).unwrap();
        assert!(
            repo.list_conversations(&pid, ConversationFilter::All, 50)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            repo.list_conversations(&pid, ConversationFilter::Archived, 50)
                .unwrap()
                .len(),
            1
        );

        let counts = repo.delete_conversation(&cid).unwrap();
        assert_eq!(counts.messages, 2);
        assert!(repo.get_conversation(&cid).unwrap().is_none());
    }

    #[test]
    fn fork_copies_messages_into_a_new_conversation() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        for i in 0..5 {
            repo.append_message(
                &pid,
                &cid,
                "t",
                MessageRole::User,
                MessageState::Complete,
                &format!("m{i}"),
                2_000 + i,
            )
            .unwrap();
        }
        let fork = repo.fork_conversation(&cid, Some(3), 4_000).unwrap();
        assert_ne!(fork, cid);
        let page = repo.message_page(&fork, None, 100).unwrap();
        assert_eq!(page.len(), 3, "fork copies messages up to sequence 3");
        assert_eq!(page.first().unwrap().sequence, 1);
    }

    #[test]
    fn pagination_over_one_hundred_thousand_messages() {
        let mut repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        repo.seed_messages(&pid, &cid, "t", 100_000, 5_000).unwrap();

        let latest = repo.message_page(&cid, None, 100).unwrap();
        assert_eq!(latest.len(), 100);
        assert_eq!(latest.last().unwrap().sequence, 100_000);
        assert_eq!(latest.first().unwrap().sequence, 99_901);

        let older = repo
            .message_page(&cid, Some(latest.first().unwrap().sequence), 100)
            .unwrap();
        assert_eq!(older.len(), 100);
        assert_eq!(older.last().unwrap().sequence, 99_900);
    }

    #[test]
    fn secret_is_absent_from_stored_content_fts_and_summary() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let mid = repo
            .append_message(
                &pid,
                &cid,
                "t",
                MessageRole::User,
                MessageState::Complete,
                &format!("my key is {SECRET} ok"),
                2_000,
            )
            .unwrap();

        let page = repo.message_page(&cid, None, 10).unwrap();
        assert_eq!(page.len(), 1);
        assert!(
            !page[0].content_redacted.contains(SECRET),
            "redacted content must not contain the secret"
        );
        assert!(page[0].content_redacted.contains("[REDACTED]"));

        // FTS only indexes redacted content: searching a benign word still finds it.
        let hits = repo.search_messages(&cid, "key").unwrap();
        assert_eq!(hits, vec![mid]);

        // Shared summary is redacted too.
        repo.upsert_summary(&cid, &format!("user shared {SECRET}"), 1, 3_000)
            .unwrap();
        let summary = repo.get_summary(&cid).unwrap().unwrap();
        assert!(!summary.contains(SECRET));
        let digest = repo.get_digest(&cid).unwrap().unwrap();
        assert_eq!(digest.schema, CONVERSATION_DIGEST_SCHEMA);
        assert_eq!(digest.conversation_id, cid);
        assert_eq!(digest.source_message_sequence, 1);
        assert_eq!(digest.generated_at_ms, 3_000);
        assert!(!digest.summary_redacted.contains(SECRET));
    }

    #[test]
    fn encrypted_raw_content_is_separate_from_redacted_read_models() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let encrypted = MessageRawContentCiphertext {
            ciphertext: b"age-encryption.org/v1\nopaque-ciphertext".to_vec(),
            nonce: b"nonce-v1".to_vec(),
        };
        let mid = repo
            .append_message_with_raw_ciphertext(
                &pid,
                &cid,
                "turn-encrypted-raw",
                MessageRole::User,
                MessageState::Complete,
                &format!("configure {} for this test", openai_key_assignment(SECRET)),
                Some(&encrypted),
                2_000,
            )
            .unwrap();

        let page = repo.message_page(&cid, None, 10).unwrap();
        assert_eq!(page.len(), 1);
        assert!(page[0].content_redacted.contains("[REDACTED]"));
        assert!(!page[0].content_redacted.contains(SECRET));
        assert!(
            !serde_json::to_string(&page)
                .unwrap()
                .contains("opaque-ciphertext")
        );
        assert_eq!(
            repo.search_messages(&cid, "configure").unwrap(),
            vec![mid.clone()]
        );
        assert!(repo.search_messages(&cid, "opaque").unwrap().is_empty());

        assert_eq!(
            repo.message_raw_content_ciphertext(&mid).unwrap(),
            Some(encrypted.clone())
        );
        assert_eq!(
            repo.turn_user_message_raw_content_ciphertext("turn-encrypted-raw")
                .unwrap(),
            Some(encrypted)
        );

        repo.upsert_summary(
            &cid,
            &format!("Decision: keep {SECRET} encrypted"),
            1,
            3_000,
        )
        .unwrap();
        let digest = repo.get_digest(&cid).unwrap().unwrap();
        let digest_json = serde_json::to_string(&digest).unwrap();
        assert!(!digest_json.contains(SECRET));
        assert!(!digest_json.contains("opaque-ciphertext"));
    }
}
