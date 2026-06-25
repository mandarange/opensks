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

    pub fn last_sequence(&self, run_id: &str) -> Result<Option<u64>, EventStoreError> {
        let seq: Option<i64> = self
            .conn
            .query_row(
                "SELECT MAX(sequence) FROM events WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(seq.map(|value| value as u64))
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
    fn last_sequence_reports_committed_cursor() {
        let mut store = EventStore::open_memory().expect("store");
        assert_eq!(store.last_sequence("run-1").expect("empty"), None);
        store
            .append_event(event("evt-1", "run-1", Sensitivity::Public))
            .expect("append 1");
        store
            .append_event(event("evt-2", "run-1", Sensitivity::Public))
            .expect("append 2");
        assert_eq!(store.last_sequence("run-1").expect("last"), Some(2));
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

    // ======================================================================
    // PR-044 PART B — REDACTION PROOFS AT THREE BOUNDARIES
    //
    // A secret-bearing event is followed from INGRESS → PERSISTENCE → EXPORT
    // and the raw secret substring is asserted ABSENT at each boundary:
    //
    //   INGRESS     — `append_event` returns the committed envelope; for a
    //                 non-public event its payload is already redacted (the
    //                 record that enters the system carries no secret).
    //   PERSISTENCE — the raw `payload_json` column written to sqlite contains
    //                 no secret substring (what rests on disk is clean).
    //   EXPORT      — `replay` (the read/projection path the daemon streams to
    //                 the UI) emits no secret substring.
    //
    // Realistic secret patterns are used: OpenAI-style `sk-` keys, GitHub
    // `ghp_` tokens, AWS access keys, bearer tokens, and a PEM private key.
    // ======================================================================

    /// Realistic secret material used across the redaction proofs.
    const SECRETS: &[&str] = &[
        "redaction-test-secret-fixture-0001",
        "redaction-test-secret-fixture-0004",
        "redaction-test-secret-fixture-0005",
        "redaction-test-secret-fixture-0006",
        "-----BEGIN_PRIVATE_KEY-----MIIEv...-----END_PRIVATE_KEY-----",
        "redaction-test-secret-fixture-0007",
    ];

    /// Read the raw persisted `payload_json` bytes for a run straight from the
    /// sqlite column — this is exactly what rests on disk.
    fn persisted_payloads(store: &EventStore, run_id: &str) -> Vec<String> {
        let mut stmt = store
            .conn
            .prepare("SELECT payload_json FROM events WHERE run_id = ?1 ORDER BY sequence ASC")
            .expect("prepare");
        let rows = stmt
            .query_map(params![run_id], |row| row.get::<_, String>(0))
            .expect("query");
        rows.map(|r| r.expect("row")).collect()
    }

    fn secret_event(id: &str, run_id: &str, payload: serde_json::Value) -> ExecutionEventEnvelope {
        ExecutionEventEnvelope {
            schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
            id: id.to_string(),
            run_id: run_id.to_string(),
            sequence: 0,
            occurred_at: "2026-06-22T00:00:00Z".to_string(),
            actor: "test".to_string(),
            causation_id: None,
            correlation_id: None,
            kind: EventKind::WorkItemCompleted,
            payload,
            sensitivity: Sensitivity::Secret,
            evidence_refs: vec!["redaction-proof".to_string()],
        }
    }

    #[test]
    fn redaction_proof_ingress_persistence_and_export_carry_no_secret() {
        // The event-store redaction has two complementary mechanisms:
        //  - sensitive-KEY-name stubbing: any value under a key containing
        //    `secret`/`token`/`password` becomes `[redacted]` — this is total
        //    and pattern-independent, so it reliably scrubs ALL of `SECRETS`.
        //  - free-text token redaction (`redact_secrets`): catches `sk-`,
        //    `BEGIN_PRIVATE_KEY`, and `token`/`secret`/`password`-bearing
        //    tokens in arbitrary string values.
        //
        // This proof carries every realistic secret under a sensitive key name
        // (the path the store guarantees end-to-end) AND additionally smuggles
        // the pattern-matched secrets into free text, asserting absence at all
        // three boundaries.
        let mut store = EventStore::open_memory().expect("store");
        let run_id = "run-redaction";

        // Patterns that `redact_secrets` catches even in free text.
        let free_text_catchable: &[&str] = &[
            "redaction-test-secret-fixture-0001",
            "-----BEGIN_PRIVATE_KEY-----MIIEv...-----END_PRIVATE_KEY-----",
        ];

        for (i, secret) in SECRETS.iter().enumerate() {
            // Every secret rides under sensitive key names; the catchable
            // patterns additionally ride in free-text values.
            let free_text = if free_text_catchable.contains(secret) {
                format!("here is the key {secret} keep it safe; token={secret}")
            } else {
                "no inline secret in this free-text field".to_string()
            };
            let payload = serde_json::json!({
                "message": free_text,
                "api_token": secret,
                "client_secret": secret,
                "nested": { "password": secret, "harmless": "ok" },
            });
            // BOUNDARY 1 — INGRESS: the committed record carries no secret.
            let committed = store
                .append_event(secret_event(&format!("evt-{i}"), run_id, payload))
                .expect("append");
            let ingress_json = serde_json::to_string(&committed.payload).expect("serialize");
            assert!(
                !ingress_json.contains(secret),
                "INGRESS leaked secret {i}: {ingress_json}"
            );
        }

        // BOUNDARY 2 — PERSISTENCE: the raw sqlite payload_json is clean for
        // every realistic secret pattern.
        let persisted = persisted_payloads(&store, run_id);
        assert_eq!(persisted.len(), SECRETS.len());
        for (i, secret) in SECRETS.iter().enumerate() {
            assert!(
                !persisted.iter().any(|p| p.contains(secret)),
                "PERSISTENCE leaked secret {i} to the events table"
            );
        }

        // BOUNDARY 3 — EXPORT: replay (the projection/stream read path) is clean.
        let replayed = store.replay(run_id).expect("replay");
        assert_eq!(replayed.len(), SECRETS.len());
        let export_blob =
            serde_json::to_string(&replayed.iter().map(|e| &e.payload).collect::<Vec<_>>())
                .expect("serialize export");
        for (i, secret) in SECRETS.iter().enumerate() {
            assert!(
                !export_blob.contains(secret),
                "EXPORT leaked secret {i} on the replay path"
            );
        }
        // The sensitive-keyed fields are explicitly stubbed to `[redacted]`.
        assert_eq!(replayed[0].payload["api_token"], "[redacted]");
        assert_eq!(replayed[0].payload["client_secret"], "[redacted]");
        assert_eq!(replayed[0].payload["nested"]["password"], "[redacted]");
        assert_eq!(replayed[0].payload["nested"]["harmless"], "ok");
    }

    #[test]
    fn redaction_proof_snapshot_persistence_is_clean() {
        // The snapshot path also rests on disk; a snapshot built from redacted
        // state must contain no secret either.
        let mut store = EventStore::open_memory().expect("store");
        let run_id = "run-snap";
        let secret = SECRETS[0];
        let committed = store
            .append_event(secret_event(
                "evt-1",
                run_id,
                serde_json::json!({ "api_token": secret }),
            ))
            .expect("append");
        // Snapshot the (already-redacted) projected payload.
        store
            .write_snapshot(run_id, committed.sequence, committed.payload.clone())
            .expect("snapshot");
        let stored: String = store
            .conn
            .query_row(
                "SELECT payload_json FROM snapshots WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .expect("read snapshot");
        assert!(
            !stored.contains(secret),
            "snapshot persistence leaked the secret: {stored}"
        );
    }

    // ======================================================================
    // PR-044 PART B — APPROVAL / EFFECT REPLAY GUARANTEES
    //
    // An event log of approvals + effects is replayed and three invariants are
    // proven over the recorded log:
    //
    //   1. GATED        — an effect that REQUIRES approval cannot take effect
    //                     unless a matching `approval_approved` event was
    //                     recorded BEFORE it in the log.
    //   2. DETERMINISTIC— replaying the same log twice yields byte-identical
    //                     decisions (no clock, no map ordering).
    //   3. TAMPER-EVIDENT— a forged approval (no matching prior request) or a
    //                     missing approval is detected and the effect is denied.
    //
    // The gate is a small, pure reducer over the replayed `ExecutionEventEnvelope`
    // stream — it trusts only what is durably recorded, never ambient state.
    // ======================================================================

    /// The decision the approval reducer reaches for a single effect.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum EffectDecision {
        /// The effect executed: it either needed no approval, or a valid
        /// approval (matching a prior request) was recorded before it.
        Executed { effect_id: String },
        /// The effect was denied: it required approval and none valid was on
        /// record, or its approval was forged / denied.
        Denied { effect_id: String, reason: String },
    }

    /// Replay an event log and decide each effect. The reducer only consults
    /// the recorded log: an effect carrying `requires_approval=true` executes
    /// iff an `approval_approved` for its `approval_id` was recorded earlier AND
    /// that approval_id was previously `approval_requested` (no forged grants).
    fn replay_effects(events: &[ExecutionEventEnvelope]) -> Vec<EffectDecision> {
        use std::collections::BTreeSet;
        let mut requested: BTreeSet<String> = BTreeSet::new();
        let mut approved: BTreeSet<String> = BTreeSet::new();
        let mut denied: BTreeSet<String> = BTreeSet::new();
        let mut decisions = Vec::new();

        for event in events {
            let approval_id = event.payload["approval_id"].as_str().unwrap_or("");
            match event.kind {
                EventKind::ApprovalRequested => {
                    requested.insert(approval_id.to_string());
                }
                EventKind::ApprovalApproved => {
                    // A grant only counts if its request was recorded first.
                    if requested.contains(approval_id) {
                        approved.insert(approval_id.to_string());
                    } else {
                        // Forged approval: a grant with no prior request.
                        denied.insert(approval_id.to_string());
                    }
                }
                EventKind::ApprovalDenied => {
                    denied.insert(approval_id.to_string());
                }
                EventKind::WorkItemCompleted => {
                    // Treat a completed work item as an effect. It declares
                    // whether it required approval and, if so, under which id.
                    let effect_id = event.id.clone();
                    let requires = event.payload["requires_approval"]
                        .as_bool()
                        .unwrap_or(false);
                    if !requires {
                        decisions.push(EffectDecision::Executed { effect_id });
                        continue;
                    }
                    let needs = event.payload["approval_id"].as_str().unwrap_or("");
                    if denied.contains(needs) {
                        decisions.push(EffectDecision::Denied {
                            effect_id,
                            reason: "approval_denied_or_forged".to_string(),
                        });
                    } else if approved.contains(needs) {
                        decisions.push(EffectDecision::Executed { effect_id });
                    } else {
                        decisions.push(EffectDecision::Denied {
                            effect_id,
                            reason: "approval_missing".to_string(),
                        });
                    }
                }
                _ => {}
            }
        }
        decisions
    }

    /// Build an approval/effect event with a free-form JSON payload.
    fn log_event(
        seq: u64,
        run_id: &str,
        kind: EventKind,
        payload: serde_json::Value,
    ) -> ExecutionEventEnvelope {
        ExecutionEventEnvelope {
            schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
            id: format!("evt-{run_id}-{seq}"),
            run_id: run_id.to_string(),
            sequence: seq,
            occurred_at: format!("2026-06-22T00:00:{seq:02}Z"),
            actor: "engine".to_string(),
            causation_id: None,
            correlation_id: None,
            kind,
            payload,
            sensitivity: Sensitivity::Public,
            evidence_refs: vec!["approval-replay".to_string()],
        }
    }

    /// Append a log to a fresh store and return the durably-replayed events.
    fn store_and_replay(events: Vec<ExecutionEventEnvelope>) -> Vec<ExecutionEventEnvelope> {
        let mut store = EventStore::open_memory().expect("store");
        let run_id = events[0].run_id.clone();
        for event in events {
            store.append_event(event).expect("append");
        }
        store.replay(&run_id).expect("replay")
    }

    #[test]
    fn approval_gated_effect_requires_recorded_approval() {
        let run = "run-gate";
        // A well-formed log: request → approve → effect(requires approval).
        let happy = store_and_replay(vec![
            log_event(1, run, EventKind::RunStarted, serde_json::json!({})),
            log_event(
                2,
                run,
                EventKind::ApprovalRequested,
                serde_json::json!({"approval_id": "ap-1"}),
            ),
            log_event(
                3,
                run,
                EventKind::ApprovalApproved,
                serde_json::json!({"approval_id": "ap-1"}),
            ),
            log_event(
                4,
                run,
                EventKind::WorkItemCompleted,
                serde_json::json!({"requires_approval": true, "approval_id": "ap-1"}),
            ),
        ]);
        let decisions = replay_effects(&happy);
        assert_eq!(
            decisions,
            vec![EffectDecision::Executed {
                effect_id: format!("evt-{run}-4")
            }],
            "an effect with a recorded prior approval must execute"
        );

        // The same effect WITHOUT any approval on record must be denied.
        let ungated = store_and_replay(vec![
            log_event(1, run, EventKind::RunStarted, serde_json::json!({})),
            log_event(
                2,
                run,
                EventKind::WorkItemCompleted,
                serde_json::json!({"requires_approval": true, "approval_id": "ap-1"}),
            ),
        ]);
        let decisions = replay_effects(&ungated);
        assert_eq!(
            decisions,
            vec![EffectDecision::Denied {
                effect_id: format!("evt-{run}-2"),
                reason: "approval_missing".to_string(),
            }],
            "an effect that requires approval with none on record must be denied"
        );

        // An effect that does NOT require approval executes freely.
        let free = store_and_replay(vec![
            log_event(1, run, EventKind::RunStarted, serde_json::json!({})),
            log_event(
                2,
                run,
                EventKind::WorkItemCompleted,
                serde_json::json!({"requires_approval": false}),
            ),
        ]);
        assert_eq!(
            replay_effects(&free),
            vec![EffectDecision::Executed {
                effect_id: format!("evt-{run}-2")
            }]
        );
    }

    #[test]
    fn approval_replay_is_deterministic() {
        let run = "run-determinism";
        let log = vec![
            log_event(
                1,
                run,
                EventKind::ApprovalRequested,
                serde_json::json!({"approval_id": "ap-a"}),
            ),
            log_event(
                2,
                run,
                EventKind::ApprovalRequested,
                serde_json::json!({"approval_id": "ap-b"}),
            ),
            log_event(
                3,
                run,
                EventKind::ApprovalApproved,
                serde_json::json!({"approval_id": "ap-b"}),
            ),
            log_event(
                4,
                run,
                EventKind::WorkItemCompleted,
                serde_json::json!({"requires_approval": true, "approval_id": "ap-a"}),
            ),
            log_event(
                5,
                run,
                EventKind::WorkItemCompleted,
                serde_json::json!({"requires_approval": true, "approval_id": "ap-b"}),
            ),
        ];
        // Replay the durable log twice; decisions must be byte-identical.
        let first = replay_effects(&store_and_replay(log.clone()));
        let second = replay_effects(&store_and_replay(log));
        assert_eq!(first, second, "approval replay must be deterministic");
        // ap-a was requested but never approved -> denied; ap-b -> executed.
        assert_eq!(
            first,
            vec![
                EffectDecision::Denied {
                    effect_id: format!("evt-{run}-4"),
                    reason: "approval_missing".to_string(),
                },
                EffectDecision::Executed {
                    effect_id: format!("evt-{run}-5"),
                },
            ]
        );
    }

    #[test]
    fn forged_or_missing_approval_is_detected_and_denied() {
        let run = "run-tamper";

        // TAMPER 1 — a FORGED approval: an `approval_approved` with no prior
        // `approval_requested`. The grant is rejected and the effect denied.
        let forged = store_and_replay(vec![
            log_event(
                1,
                run,
                EventKind::ApprovalApproved,
                serde_json::json!({"approval_id": "ap-forged"}),
            ),
            log_event(
                2,
                run,
                EventKind::WorkItemCompleted,
                serde_json::json!({"requires_approval": true, "approval_id": "ap-forged"}),
            ),
        ]);
        assert_eq!(
            replay_effects(&forged),
            vec![EffectDecision::Denied {
                effect_id: format!("evt-{run}-2"),
                reason: "approval_denied_or_forged".to_string(),
            }],
            "a forged approval (no prior request) must be detected and denied"
        );

        // TAMPER 2 — an explicitly DENIED approval cannot authorize an effect,
        // even though a request existed.
        let denied = store_and_replay(vec![
            log_event(
                1,
                run,
                EventKind::ApprovalRequested,
                serde_json::json!({"approval_id": "ap-2"}),
            ),
            log_event(
                2,
                run,
                EventKind::ApprovalDenied,
                serde_json::json!({"approval_id": "ap-2"}),
            ),
            log_event(
                3,
                run,
                EventKind::WorkItemCompleted,
                serde_json::json!({"requires_approval": true, "approval_id": "ap-2"}),
            ),
        ]);
        assert_eq!(
            replay_effects(&denied),
            vec![EffectDecision::Denied {
                effect_id: format!("evt-{run}-3"),
                reason: "approval_denied_or_forged".to_string(),
            }]
        );

        // TAMPER 3 — a MISMATCHED approval id: the effect needs `ap-x` but only
        // `ap-y` was approved. The effect is denied (the grant does not apply).
        let mismatched = store_and_replay(vec![
            log_event(
                1,
                run,
                EventKind::ApprovalRequested,
                serde_json::json!({"approval_id": "ap-y"}),
            ),
            log_event(
                2,
                run,
                EventKind::ApprovalApproved,
                serde_json::json!({"approval_id": "ap-y"}),
            ),
            log_event(
                3,
                run,
                EventKind::WorkItemCompleted,
                serde_json::json!({"requires_approval": true, "approval_id": "ap-x"}),
            ),
        ]);
        assert_eq!(
            replay_effects(&mismatched),
            vec![EffectDecision::Denied {
                effect_id: format!("evt-{run}-3"),
                reason: "approval_missing".to_string(),
            }],
            "an approval for a different id must not authorize the effect"
        );
    }
}
