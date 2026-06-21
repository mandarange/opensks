use std::path::Path;

use opensks_artifacts::redact_secrets;
use opensks_contracts::{EventKind, ExecutionEventEnvelope, Sensitivity};
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

pub const ENGINE_DB_RELATIVE_PATH: &str = ".opensks/runtime/engine.sqlite3";
pub const MIGRATION_VERSION: i64 = 1;

#[derive(Debug, Error)]
pub enum EventStoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("event append must commit before snapshot mutation")]
    MissingCommittedEvent,
}

pub struct EventStore {
    conn: Connection,
}

impl EventStore {
    pub fn open(path: &Path) -> Result<Self, EventStoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_workspace(workspace: &Path) -> Result<Self, EventStoreError> {
        Self::open(&workspace.join(ENGINE_DB_RELATIVE_PATH))
    }

    pub fn open_memory() -> Result<Self, EventStoreError> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn migrate(&self) -> Result<(), EventStoreError> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS runs (
                run_id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                sequence INTEGER NOT NULL,
                occurred_at TEXT NOT NULL,
                actor TEXT NOT NULL,
                causation_id TEXT,
                correlation_id TEXT,
                kind TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                sensitivity TEXT NOT NULL,
                UNIQUE(run_id, sequence)
            );
            CREATE TABLE IF NOT EXISTS snapshots (
                run_id TEXT PRIMARY KEY,
                last_sequence INTEGER NOT NULL,
                snapshot_hash TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS evidence (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                event_id TEXT NOT NULL,
                evidence_ref TEXT NOT NULL
            );
            ",
        )?;
        self.conn
            .pragma_update(None, "user_version", MIGRATION_VERSION)?;
        Ok(())
    }

    pub fn migration_version(&self) -> Result<i64, EventStoreError> {
        Ok(self
            .conn
            .query_row("SELECT user_version FROM pragma_user_version", [], |row| {
                row.get(0)
            })?)
    }

    pub fn integrity_check(&self) -> Result<String, EventStoreError> {
        Ok(self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?)
    }

    pub fn next_sequence(&self, run_id: &str) -> Result<u64, EventStoreError> {
        let seq: Option<i64> = self
            .conn
            .query_row(
                "SELECT MAX(sequence) FROM events WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(seq.unwrap_or(0) as u64 + 1)
    }

    pub fn append_event(
        &mut self,
        mut event: ExecutionEventEnvelope,
    ) -> Result<ExecutionEventEnvelope, EventStoreError> {
        if event.sequence == 0 {
            event.sequence = self.next_sequence(&event.run_id)?;
        }
        if event.sensitivity != Sensitivity::Public {
            event.payload = redact_value(event.payload);
        }

        let payload_json = serde_json::to_string(&event.payload)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO runs (run_id) VALUES (?1)",
            params![event.run_id],
        )?;
        tx.execute(
            "INSERT INTO events (
                id, run_id, sequence, occurred_at, actor, causation_id,
                correlation_id, kind, payload_json, sensitivity
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                event.id,
                event.run_id,
                event.sequence as i64,
                event.occurred_at,
                event.actor,
                event.causation_id,
                event.correlation_id,
                event.kind.as_str(),
                payload_json,
                event.sensitivity.as_str()
            ],
        )?;
        for evidence_ref in &event.evidence_refs {
            tx.execute(
                "INSERT INTO evidence (run_id, event_id, evidence_ref) VALUES (?1, ?2, ?3)",
                params![event.run_id, event.id, evidence_ref],
            )?;
        }
        tx.commit()?;
        Ok(event)
    }

    pub fn write_snapshot(
        &self,
        run_id: &str,
        last_sequence: u64,
        payload: serde_json::Value,
    ) -> Result<(), EventStoreError> {
        let committed = self
            .conn
            .query_row(
                "SELECT 1 FROM events WHERE run_id = ?1 AND sequence = ?2",
                params![run_id, last_sequence as i64],
                |_| Ok(()),
            )
            .optional()?;
        if committed.is_none() {
            return Err(EventStoreError::MissingCommittedEvent);
        }
        let payload_json = serde_json::to_string(&payload)?;
        let snapshot_hash = stable_hash(&payload_json);
        self.conn.execute(
            "INSERT INTO snapshots (run_id, last_sequence, snapshot_hash, payload_json)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(run_id) DO UPDATE SET
                last_sequence = excluded.last_sequence,
                snapshot_hash = excluded.snapshot_hash,
                payload_json = excluded.payload_json,
                updated_at = CURRENT_TIMESTAMP",
            params![run_id, last_sequence as i64, snapshot_hash, payload_json],
        )?;
        Ok(())
    }

    pub fn replay(&self, run_id: &str) -> Result<Vec<ExecutionEventEnvelope>, EventStoreError> {
        self.replay_since(run_id, 0)
    }

    pub fn replay_since(
        &self,
        run_id: &str,
        since_sequence: u64,
    ) -> Result<Vec<ExecutionEventEnvelope>, EventStoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, sequence, occurred_at, actor, causation_id,
                    correlation_id, kind, payload_json, sensitivity
             FROM events WHERE run_id = ?1 AND sequence > ?2 ORDER BY sequence ASC",
        )?;
        let rows = stmt.query_map(params![run_id, since_sequence as i64], |row| {
            let kind_raw: String = row.get(7)?;
            let sensitivity_raw: String = row.get(9)?;
            let payload_json: String = row.get(8)?;
            Ok(ExecutionEventEnvelope {
                schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
                id: row.get(0)?,
                run_id: row.get(1)?,
                sequence: row.get::<_, i64>(2)? as u64,
                occurred_at: row.get(3)?,
                actor: row.get(4)?,
                causation_id: row.get(5)?,
                correlation_id: row.get(6)?,
                kind: EventKind::parse_label(&kind_raw),
                payload: serde_json::from_str(&payload_json)
                    .unwrap_or_else(|_| serde_json::json!({"decode_error": true})),
                sensitivity: Sensitivity::parse_label(&sensitivity_raw),
                evidence_refs: Vec::new(),
            })
        })?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        for event in &mut events {
            event.evidence_refs = self.evidence_refs_for_event(&event.id)?;
        }
        Ok(events)
    }

    fn evidence_refs_for_event(&self, event_id: &str) -> Result<Vec<String>, EventStoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT evidence_ref FROM evidence WHERE event_id = ?1 ORDER BY id ASC")?;
        let rows = stmt.query_map(params![event_id], |row| row.get(0))?;
        let mut evidence_refs = Vec::new();
        for row in rows {
            evidence_refs.push(row?);
        }
        Ok(evidence_refs)
    }
}

fn redact_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(value) => serde_json::Value::String(redact_secrets(&value)),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(redact_value).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    let redacted = if lower.contains("secret")
                        || lower.contains("token")
                        || lower.contains("password")
                    {
                        serde_json::Value::String("[redacted]".to_string())
                    } else {
                        redact_value(value)
                    };
                    (key, redacted)
                })
                .collect(),
        ),
        other => other,
    }
}

fn stable_hash(input: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, run_id: &str, sensitivity: Sensitivity) -> ExecutionEventEnvelope {
        ExecutionEventEnvelope {
            schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
            id: id.to_string(),
            run_id: run_id.to_string(),
            sequence: 0,
            occurred_at: "2026-06-21T00:00:00Z".to_string(),
            actor: "test".to_string(),
            causation_id: None,
            correlation_id: Some("corr-1".to_string()),
            kind: EventKind::RunStarted,
            payload: serde_json::json!({"message": "hello", "api_token": "sk-test-token"}),
            sensitivity,
            evidence_refs: vec!["unit-test".to_string()],
        }
    }

    #[test]
    fn migration_sets_user_version_and_integrity_is_ok() {
        let store = EventStore::open_memory().expect("store");
        assert_eq!(
            store.migration_version().expect("version"),
            MIGRATION_VERSION
        );
        assert_eq!(store.integrity_check().expect("integrity"), "ok");
    }

    #[test]
    fn append_then_replay_preserves_order_and_redacts_sensitive_payload() {
        let mut store = EventStore::open_memory().expect("store");
        store
            .append_event(event("evt-1", "run-1", Sensitivity::Secret))
            .expect("append");
        store
            .append_event(event("evt-2", "run-1", Sensitivity::Public))
            .expect("append");

        let events = store.replay("run-1").expect("replay");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].sequence, 1);
        assert_eq!(events[1].sequence, 2);
        assert_eq!(events[0].payload["api_token"], "[redacted]");
        assert_eq!(events[1].payload["api_token"], "sk-test-token");
        assert_eq!(events[0].evidence_refs, vec!["unit-test"]);
        assert_eq!(events[1].evidence_refs, vec!["unit-test"]);
    }

    #[test]
    fn replay_since_filters_committed_sequence_and_preserves_evidence() {
        let mut store = EventStore::open_memory().expect("store");
        store
            .append_event(event("evt-1", "run-1", Sensitivity::Public))
            .expect("append 1");
        store
            .append_event(event("evt-2", "run-1", Sensitivity::Public))
            .expect("append 2");
        store
            .append_event(event("evt-3", "run-1", Sensitivity::Public))
            .expect("append 3");

        let events = store.replay_since("run-1", 1).expect("replay since");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].sequence, 2);
        assert_eq!(events[1].sequence, 3);
        assert_eq!(events[0].evidence_refs, vec!["unit-test"]);
    }

    #[test]
    fn snapshot_is_blocked_until_event_is_committed() {
        let store = EventStore::open_memory().expect("store");
        let err = store
            .write_snapshot("run-missing", 1, serde_json::json!({"state": "bad"}))
            .expect_err("snapshot before event");
        assert!(matches!(err, EventStoreError::MissingCommittedEvent));
    }

    #[test]
    fn snapshot_after_event_commit_succeeds() {
        let mut store = EventStore::open_memory().expect("store");
        let committed = store
            .append_event(event("evt-1", "run-1", Sensitivity::Public))
            .expect("append");
        store
            .write_snapshot(
                "run-1",
                committed.sequence,
                serde_json::json!({"state": "snapshotted"}),
            )
            .expect("snapshot");
    }
}
