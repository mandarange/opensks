//! Durable project/conversation/message persistence (PR-024).
//!
//! A WAL-mode SQLite repository for projects, conversations, and messages with
//! cursor pagination and an FTS index over secret-redacted content. There is NO
//! engine dispatch here — that lands in PR-027. Raw message text is redacted
//! before it is stored in the searchable `content_redacted` column / FTS index;
//! the original may be held encrypted out of band (content_ciphertext column).

use std::path::Path;

use opensks_contracts::{
    CONVERSATION_MESSAGE_SCHEMA, CONVERSATION_SUMMARY_SCHEMA, ConversationDeleteCounts,
    ConversationFilter, ConversationMessage, ConversationStatus, ConversationSummary, MessageRole,
    MessageState, TitleSource,
};
use rusqlite::{Connection, Row, params};

const MIGRATION_VERSION: i32 = 2;
const CONVERSATION_DB_RELATIVE_PATH: &str = ".opensks/runtime/conversations.sqlite3";

#[derive(Debug, thiserror::Error)]
pub enum ConversationError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
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

/// Redact secret-looking tokens from text before it is stored in the searchable
/// copy or an FTS index. Whitespace within a line is normalized; line breaks are
/// preserved. Catches common key prefixes and long high-entropy tokens.
pub fn redact_secrets(input: &str) -> String {
    input
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(|tok| if looks_secret(tok) { "[REDACTED]" } else { tok })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn looks_secret(token: &str) -> bool {
    let core = token.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    if core.len() < 12 {
        return false;
    }
    const PREFIXES: [&str; 6] = ["sk-", "sk_", "ghp_", "xoxb-", "aws_", "akia"];
    let lower = core.to_ascii_lowercase();
    if PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return true;
    }
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
            ",
        )?;
        self.conn
            .pragma_update(None, "user_version", MIGRATION_VERSION)?;
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
        self.conn.execute(
            "INSERT INTO run_projections(
                 run_id, project_id, conversation_id, turn_id, state,
                 pipeline_id, graph_revision, last_sequence, projection_json, updated_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, NULL, NULL, 0, '{}', ?6)
             ON CONFLICT(run_id) DO UPDATE SET
                 state = excluded.state,
                 updated_at_ms = excluded.updated_at_ms",
            params![
                run_id,
                project_id,
                conversation_id,
                turn_id,
                state,
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
