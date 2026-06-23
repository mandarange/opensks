//! Durable project/conversation/message persistence (PR-024).
//!
//! A WAL-mode SQLite repository for projects, conversations, and messages with
//! cursor pagination and an FTS index over secret-redacted content. There is NO
//! engine dispatch here — that lands in PR-027. Raw message text is redacted
//! before it is stored in the searchable `content_redacted` column / FTS index;
//! the original may be held encrypted out of band (content_ciphertext column).

use std::{collections::HashMap, path::Path};

use opensks_contracts::{
    CONVERSATION_MESSAGE_SCHEMA, CONVERSATION_SUMMARY_SCHEMA, CONVERSATION_TURN_ACCEPTED_SCHEMA,
    ConversationDeleteCounts, ConversationFilter, ConversationMessage, ConversationStatus,
    ConversationSummary, ConversationThreadSettings, ConversationTurnAccepted,
    ConversationTurnSettings, ConversationTurnStartRequest, MessageRole, MessageState,
    RunProjectionState, TIMELINE_ITEM_SCHEMA, TimelineItem, TimelineItemKind, TitleSource,
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};

const MIGRATION_VERSION: i32 = 4;
const CONVERSATION_DB_RELATIVE_PATH: &str = ".opensks/runtime/conversations.sqlite3";

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

fn run_projection_state_from_str(raw: &str) -> RunProjectionState {
    match raw {
        "queued" => RunProjectionState::Queued,
        "running" => RunProjectionState::Running,
        "paused" => RunProjectionState::Paused,
        "completed" => RunProjectionState::Completed,
        "failed" => RunProjectionState::Failed,
        "cancelled" => RunProjectionState::Cancelled,
        _ => RunProjectionState::Queued,
    }
}

fn conversation_status_for_run_state(raw: &str) -> &'static str {
    match raw {
        "completed" => "completed",
        "failed" => "failed",
        "paused" | "cancelled" => "paused",
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
                state TEXT NOT NULL,
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
                state TEXT NOT NULL,
                last_event_sequence INTEGER NOT NULL DEFAULT -1,
                lease_owner TEXT NULL,
                lease_expires_at_ms INTEGER NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            ",
        )?;
        self.ensure_turns_model_routing_column()?;
        self.repair_conversation_statuses()?;
        self.conn
            .pragma_update(None, "user_version", MIGRATION_VERSION)?;
        Ok(())
    }

    fn repair_conversation_statuses(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE conversations
             SET status = COALESCE((
                 SELECT CASE r.state
                     WHEN 'completed' THEN 'completed'
                     WHEN 'failed' THEN 'failed'
                     WHEN 'paused' THEN 'paused'
                     WHEN 'cancelled' THEN 'paused'
                     WHEN 'queued' THEN 'running'
                     WHEN 'running' THEN 'running'
                     ELSE 'idle'
                 END
                 FROM conversation_runs cr
                 JOIN runs r ON r.id = cr.run_id
                 WHERE cr.conversation_id = conversations.id
                 ORDER BY cr.created_at_ms DESC, cr.run_id DESC
                 LIMIT 1
             ), 'idle')
             WHERE archived = 0
               AND status = 'running'
               AND NOT EXISTS (
                   SELECT 1
                   FROM conversation_runs cr
                   JOIN runs r ON r.id = cr.run_id
                   WHERE cr.conversation_id = conversations.id
                     AND r.state IN ('queued', 'running')
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
        let content_redacted = redact_secrets(content_raw);
        let id = self.new_id()?;
        let sequence: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM messages WHERE conversation_id = ?1",
            params![conversation_id],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO messages(id, project_id, conversation_id, turn_id, role, state, content_redacted, created_at_ms, updated_at_ms, sequence)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9)",
            params![id, project_id, conversation_id, turn_id, enum_to_str(&role), enum_to_str(&state), content_redacted, now_ms as i64, sequence],
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
        let content_redacted = redact_secrets(content_raw);
        let n = self.conn.execute(
            "UPDATE messages SET content_redacted = ?1, state = ?2, updated_at_ms = ?3 WHERE id = ?4",
            params![content_redacted, enum_to_str(&state), now_ms as i64, message_id],
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
            lhs.sequence
                .cmp(&rhs.sequence)
                .then(lhs.created_at_ms.cmp(&rhs.created_at_ms))
                .then(lhs.id.cmp(&rhs.id))
        });
        if items.len() > limit {
            items = items.split_off(items.len() - limit);
        }
        Ok(items)
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
                 idempotency_key, state, effective_settings_json, settings_digest,
                 model_routing_decision_json, created_at_ms, updated_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
             ON CONFLICT(conversation_id, idempotency_key) DO UPDATE SET
                 state = excluded.state,
                 model_routing_decision_json = COALESCE(turns.model_routing_decision_json, excluded.model_routing_decision_json),
                 updated_at_ms = excluded.updated_at_ms",
            params![
                snapshot.turn_id,
                snapshot.project_id,
                snapshot.conversation_id,
                snapshot.client_turn_id,
                snapshot.request_id,
                snapshot.idempotency_key,
                snapshot.state,
                snapshot.effective_settings_json,
                snapshot.settings_digest,
                snapshot.model_routing_decision_json,
                snapshot.now_ms as i64,
            ],
        )?;
        self.conn.execute(
            "INSERT INTO runs(
                 id, turn_id, state, last_event_sequence, lease_owner,
                 lease_expires_at_ms, created_at_ms, updated_at_ms)
             VALUES(?1, ?2, ?3, -1, NULL, NULL, ?4, ?4)
             ON CONFLICT(id) DO UPDATE SET
                 state = excluded.state,
                 updated_at_ms = excluded.updated_at_ms",
            params![
                snapshot.run_id,
                snapshot.turn_id,
                snapshot.state,
                snapshot.now_ms as i64,
            ],
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
        self.conn.execute(
            "UPDATE turns SET state = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![state, now_ms as i64, turn_id],
        )?;
        match last_event_sequence {
            Some(last_event_sequence) => {
                self.conn.execute(
                    "UPDATE runs
                     SET state = ?1,
                         last_event_sequence = MAX(last_event_sequence, ?2),
                         updated_at_ms = ?3
                     WHERE id = ?4",
                    params![state, last_event_sequence as i64, now_ms as i64, run_id],
                )?;
            }
            None => {
                self.conn.execute(
                    "UPDATE runs SET state = ?1, updated_at_ms = ?2 WHERE id = ?3",
                    params![state, now_ms as i64, run_id],
                )?;
            }
        }
        Ok(())
    }

    /// Finish a supervisor-owned lease and publish the terminal run state into
    /// the conversation read models. This clears the lease so expired-lease
    /// recovery never requeues an already terminal turn.
    pub fn finish_turn_supervisor_lease(
        &self,
        turn_id: &str,
        run_id: &str,
        state: &str,
        last_event_sequence: u64,
        terminal_kind: &str,
        now_ms: u64,
    ) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            self.conn.execute(
                "UPDATE turns SET state = ?1, updated_at_ms = ?2 WHERE id = ?3",
                params![state, now_ms as i64, turn_id],
            )?;
            self.conn.execute(
                "UPDATE runs
                 SET state = ?1,
                     last_event_sequence = MAX(last_event_sequence, ?2),
                     lease_owner = NULL,
                     lease_expires_at_ms = NULL,
                     updated_at_ms = ?3
                 WHERE id = ?4",
                params![state, last_event_sequence as i64, now_ms as i64, run_id],
            )?;
            self.conn.execute(
                "UPDATE run_projections
                 SET state = ?1,
                     last_sequence = ?2,
                     updated_at_ms = ?3
                 WHERE run_id = ?4",
                params![state, last_event_sequence as i64, now_ms as i64, run_id],
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
                    run_id
                ],
            )?;
            let conversation_id = self
                .conn
                .query_row(
                    "SELECT conversation_id FROM conversation_runs WHERE run_id = ?1",
                    params![run_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(conversation_id) = conversation_id {
                let active_runs: i64 = self.conn.query_row(
                    "SELECT COUNT(*)
                     FROM conversation_runs cr
                     JOIN runs r ON r.id = cr.run_id
                     WHERE cr.conversation_id = ?1
                       AND cr.run_id <> ?2
                       AND r.state IN ('queued', 'running')",
                    params![conversation_id, run_id],
                    |row| row.get(0),
                )?;
                if active_runs == 0 {
                    self.conn.execute(
                        "UPDATE conversations
                         SET status = ?1,
                             updated_at_ms = ?2
                         WHERE id = ?3",
                        params![
                            conversation_status_for_run_state(state),
                            now_ms as i64,
                            conversation_id
                        ],
                    )?;
                }
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

    pub fn run_last_event_sequence(&self, run_id: &str) -> Result<Option<u64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT last_event_sequence FROM runs WHERE id = ?1")?;
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
                         t.model_routing_decision_json
                     FROM turns t
                     JOIN runs r ON r.turn_id = t.id
                     JOIN conversation_runs cr
                       ON cr.turn_id = t.id
                      AND cr.run_id = r.id
                      AND cr.relation = 'primary'
                     WHERE t.state = 'queued'
                       AND r.state = 'queued'
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
                        })
                    })
                    .transpose()?
            };
            let Some(claimed) = claimed else {
                return Ok(None);
            };
            self.conn.execute(
                "UPDATE turns SET state = 'running', updated_at_ms = ?1 WHERE id = ?2",
                params![now_ms as i64, claimed.turn_id],
            )?;
            self.conn.execute(
                "UPDATE runs
                 SET state = 'running',
                     lease_owner = ?1,
                     lease_expires_at_ms = ?2,
                     updated_at_ms = ?3
                 WHERE id = ?4",
                params![
                    supervisor_id,
                    lease_expires_at_ms as i64,
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
               AND state = 'running'",
            params![
                lease_expires_at_ms as i64,
                now_ms as i64,
                run_id,
                supervisor_id
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
                    "SELECT id FROM runs
                     WHERE state = 'running'
                       AND lease_expires_at_ms IS NOT NULL
                       AND lease_expires_at_ms <= ?1",
                )?;
                let rows = stmt.query_map(params![now_ms as i64], |row| row.get(0))?;
                rows.collect::<std::result::Result<Vec<String>, _>>()?
            };
            for run_id in &run_ids {
                self.conn.execute(
                    "UPDATE runs
                     SET state = 'queued',
                         lease_owner = NULL,
                         lease_expires_at_ms = NULL,
                         updated_at_ms = ?1
                     WHERE id = ?2",
                    params![now_ms as i64, run_id],
                )?;
                self.conn.execute(
                    "UPDATE turns
                     SET state = 'queued',
                         updated_at_ms = ?1
                     WHERE id = (SELECT turn_id FROM runs WHERE id = ?2)",
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
        if let Some(existing) =
            self.lookup_turn_idempotency(&request.idempotency_key, &request.conversation_id)?
        {
            let state = self
                .run_projection_state(&existing.run_id)?
                .as_deref()
                .map(run_projection_state_from_str)
                .unwrap_or(RunProjectionState::Queued);
            return Ok(ConversationTurnAccepted {
                schema: CONVERSATION_TURN_ACCEPTED_SCHEMA.to_string(),
                request_id: request.request_id.clone(),
                turn_id: existing.turn_id.clone(),
                run_id: existing.run_id.clone(),
                user_message_id: existing.user_message_id,
                assistant_message_id: existing.assistant_message_id,
                stream_id: format!("stream-{}", existing.turn_id),
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
            let effective_settings =
                self.effective_turn_settings_for_accept(&request.conversation_id, now_ms)?;
            let effective_settings_json = serde_json::to_string(&effective_settings)?;
            let settings_digest = sha256_v1(&effective_settings_json);
            let user_sequence: i64 = self.conn.query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM messages WHERE conversation_id = ?1",
                params![request.conversation_id],
                |r| r.get(0),
            )?;
            let assistant_sequence = user_sequence + 1;
            self.conn.execute(
                "INSERT INTO messages(id, project_id, conversation_id, turn_id, role, state, content_redacted, created_at_ms, updated_at_ms, sequence)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9)",
                params![
                    user_message_id,
                    request.project_id,
                    request.conversation_id,
                    turn_id,
                    enum_to_str(&MessageRole::User),
                    enum_to_str(&MessageState::Complete),
                    user_content_redacted,
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
                     idempotency_key, state, effective_settings_json, settings_digest,
                     model_routing_decision_json, created_at_ms, updated_at_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, ?8, NULL, ?9, ?9)",
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
                     id, turn_id, state, last_event_sequence, lease_owner,
                     lease_expires_at_ms, created_at_ms, updated_at_ms)
                 VALUES(?1, ?2, 'queued', 0, NULL, NULL, ?3, ?3)",
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

        Ok(ConversationTurnAccepted {
            schema: CONVERSATION_TURN_ACCEPTED_SCHEMA.to_string(),
            request_id: request.request_id.clone(),
            turn_id,
            run_id,
            user_message_id,
            assistant_message_id,
            stream_id,
            state: RunProjectionState::Queued,
        })
    }

    fn effective_turn_settings_for_accept(
        &self,
        conversation_id: &str,
        now_ms: u64,
    ) -> Result<ConversationTurnSettings> {
        let thread_settings = match self.get_thread_settings(conversation_id)? {
            Some(raw) => serde_json::from_str::<ConversationThreadSettings>(&raw)?,
            None => ConversationThreadSettings::default_for(conversation_id, now_ms),
        };
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
        token_budget: None,
        cost_budget_usd: None,
        timeout_ms: None,
        image_model_id: settings.image_model_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "redaction-test-secret-fixture-0002";

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
            settings: opensks_contracts::ConversationTurnSettings {
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
            },
            context: opensks_contracts::TurnContextSelection::default(),
            idempotency_key: idempotency_key.to_string(),
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
        request.settings.pipeline_id = "client-sent-ignored".to_string();
        request.settings.max_parallelism = 99;

        let accepted = repo.accept_conversation_turn(&request, 2_000).unwrap();
        let effective_raw = repo
            .turn_effective_settings_json(&accepted.turn_id)
            .unwrap()
            .expect("turn settings snapshot");
        let effective: ConversationTurnSettings = serde_json::from_str(&effective_raw).unwrap();

        assert_eq!(effective.pipeline_id, "parallel-build");
        assert_eq!(effective.max_parallelism, 7);
        assert_eq!(effective.verifier_count, 3);
        assert_eq!(effective.tool_policy_id, "strict-tools");
        assert_eq!(effective.approval_policy_id, "review-everything");
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
            repo.heartbeat_turn_supervisor_lease(&first.run_id, "supervisor-a", 200, 2_050)
                .unwrap()
        );
        assert!(
            !repo
                .heartbeat_turn_supervisor_lease(&first.run_id, "wrong-supervisor", 200, 2_060)
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
        assert_eq!(
            repo.turn_user_message_text(&first.turn_id)
                .unwrap()
                .as_deref(),
            Some("accept this turn")
        );

        repo.finish_turn_supervisor_lease(
            &first.turn_id,
            &first.run_id,
            "completed",
            7,
            "completed",
            2_300,
        )
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
    fn timeline_replays_messages_with_run_projection_state() {
        let repo = ConversationRepository::open_memory().unwrap();
        let (pid, cid) = project_and_conversation(&repo);
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-timeline", "idem-timeline"),
                2_000,
            )
            .unwrap();
        repo.finish_turn_supervisor_lease(
            &accepted.turn_id,
            &accepted.run_id,
            "completed",
            7,
            "completed",
            2_010,
        )
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

        repo.finish_turn_supervisor_lease(
            &first.turn_id,
            &first.run_id,
            "completed",
            4,
            "completed",
            2_030,
        )
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
        repo.finish_turn_supervisor_lease(
            &second.turn_id,
            &second.run_id,
            "failed",
            5,
            "failed",
            2_050,
        )
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
        let accepted = repo
            .accept_conversation_turn(
                &sample_turn_start_request(&pid, &cid, "req-repair-1", "idem-repair-1"),
                2_000,
            )
            .unwrap();
        repo.finish_turn_supervisor_lease(
            &accepted.turn_id,
            &accepted.run_id,
            "completed",
            9,
            "completed",
            2_010,
        )
        .unwrap();
        repo.set_status(&cid, ConversationStatus::Running, 2_020)
            .unwrap();
        assert_eq!(
            repo.get_conversation(&cid).unwrap().unwrap().status,
            ConversationStatus::Running
        );

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
    }
}
