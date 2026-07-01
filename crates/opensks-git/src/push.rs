//! Durable, approval-gated, at-most-once push outbox (PR-036).
//!
//! This module implements the *only* code path in the workspace that performs a
//! real `git push`. Every push is gated behind a two-step handshake recorded in
//! a SQLite store under the workspace state dir so it survives a restart:
//!
//! 1. [`PushOutbox::enqueue`] computes the current `local_oid` for a ref and the
//!    remote's currently-observed oid (`git ls-remote`), persists a [`PushIntent`]
//!    with an [`effect_digest`](PushOutbox::compute_effect_digest), and flags
//!    protected refs.
//! 2. [`PushOutbox::approve`] records an approval *only* when the supplied digest
//!    equals the intent's current digest; a stale digest (the oid or ref moved)
//!    is refused with `digest_mismatch` and records no usable approval.
//! 3. [`PushOutbox::execute`] refuses unless a still-valid approval exists
//!    (`no_matching_approval` / `digest_mismatch` / `protected_branch`). It
//!    derives an idempotency key from `{intent, local_oid}`; a repeat execute
//!    with a completed receipt returns `already_done: true` *without* pushing
//!    again. Otherwise it runs `git push <remote> <ref>` to the configured
//!    remote, verifies the pushed oid via `git ls-remote`, and persists the
//!    receipt. On push failure nothing is persisted as completed, the intent is
//!    left pending, and the local commit is untouched (push and commit are
//!    separate effects).
//!
//! # Remote-write safety
//!
//! The executor only ever pushes to the *workspace's configured remote* (looked
//! up via `git remote get-url <name>`); the caller supplies the remote *name*,
//! never a URL. The tests in this module push exclusively to a local **bare**
//! repository created in a temp dir and added as that named remote, so no test
//! ever contacts a network remote.

use std::path::{Path, PathBuf};
use std::process::Command;

use opensks_contracts::{
    PushApproval, PushError, PushErrorCode, PushFailureDiagnostic, PushIntent, PushReceipt,
    PushStatus,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::GitError;

/// Where the durable push-outbox SQLite database lives, relative to the
/// workspace root.
pub const PUSH_OUTBOX_DB_RELATIVE_PATH: &str = ".opensks/runtime/push-outbox.sqlite3";

/// The schema version stamped into `user_version` for the push-outbox store.
pub const PUSH_OUTBOX_MIGRATION_VERSION: i64 = 2;

/// Protected refs that require an explicit `--ack-protected` acknowledgement at
/// approval time before they can be pushed.
const PROTECTED_REFS: &[&str] = &["main", "master", "trunk", "release", "production"];

/// A durable push outbox backed by SQLite under the workspace state dir.
///
/// Reopening the same database path recovers all pending intents, recorded
/// approvals, and completed receipts (restart recovery).
pub struct PushOutbox {
    conn: Connection,
    workspace: PathBuf,
}

impl PushOutbox {
    /// Open (creating if needed) the durable push outbox for `workspace`. The
    /// database lives at [`PUSH_OUTBOX_DB_RELATIVE_PATH`] under the workspace.
    pub fn open_workspace(workspace: &Path) -> Result<Self, GitError> {
        Self::open(workspace, &workspace.join(PUSH_OUTBOX_DB_RELATIVE_PATH))
    }

    /// Open the durable push outbox at an explicit `db_path`, with `workspace`
    /// as the repository the executor shells `git` against.
    pub fn open(workspace: &Path, db_path: &Path) -> Result<Self, GitError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path).map_err(sqlite)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(sqlite)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(sqlite)?;
        let outbox = Self {
            conn,
            workspace: workspace.to_path_buf(),
        };
        outbox.migrate()?;
        Ok(outbox)
    }

    fn migrate(&self) -> Result<(), GitError> {
        self.conn
            .execute_batch(
                "
                CREATE TABLE IF NOT EXISTS push_intents (
                    intent_id TEXT PRIMARY KEY,
                    effect_digest TEXT NOT NULL,
                    remote TEXT NOT NULL,
                    remote_url_redacted TEXT NOT NULL,
                    ref_name TEXT NOT NULL,
                    local_oid TEXT NOT NULL,
                    remote_expected_oid TEXT,
                    protected INTEGER NOT NULL,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE TABLE IF NOT EXISTS push_approvals (
                    approval_id TEXT PRIMARY KEY,
                    intent_id TEXT NOT NULL,
                    effect_digest TEXT NOT NULL,
                    ack_protected INTEGER NOT NULL,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    FOREIGN KEY (intent_id) REFERENCES push_intents (intent_id)
                );
                CREATE TABLE IF NOT EXISTS push_receipts (
                    idempotency_key TEXT PRIMARY KEY,
                    intent_id TEXT NOT NULL,
                    remote_oid TEXT NOT NULL,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    FOREIGN KEY (intent_id) REFERENCES push_intents (intent_id)
                );
                CREATE TABLE IF NOT EXISTS push_failure_diagnostics (
                    idempotency_key TEXT PRIMARY KEY,
                    intent_id TEXT NOT NULL,
                    effect_digest TEXT NOT NULL,
                    remote TEXT NOT NULL,
                    remote_url_redacted TEXT NOT NULL,
                    ref_name TEXT NOT NULL,
                    local_oid TEXT NOT NULL,
                    remote_expected_oid TEXT,
                    code TEXT NOT NULL,
                    attempts INTEGER NOT NULL,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    FOREIGN KEY (intent_id) REFERENCES push_intents (intent_id)
                );
                ",
            )
            .map_err(sqlite)?;
        self.conn
            .pragma_update(None, "user_version", PUSH_OUTBOX_MIGRATION_VERSION)
            .map_err(sqlite)?;
        Ok(())
    }

    /// The schema version stamped in the store (restart-recovery sanity check).
    pub fn migration_version(&self) -> Result<i64, GitError> {
        self.conn
            .query_row("SELECT user_version FROM pragma_user_version", [], |row| {
                row.get(0)
            })
            .map_err(sqlite)
    }

    // --- effect digest -------------------------------------------------------

    /// The stable `effect_digest`: an FNV-1a 64 hash over the redacted remote
    /// URL, the ref, the local oid, and the remote-expected oid (or the literal
    /// `null` when the remote ref does not yet exist). Any change to the oid, the
    /// ref, the redacted remote, or the remote-expected oid flips the digest, so
    /// an approval recorded against an old digest no longer matches.
    pub fn compute_effect_digest(
        remote_url_redacted: &str,
        r#ref: &str,
        local_oid: &str,
        remote_expected_oid: Option<&str>,
    ) -> String {
        let mut hash: u64 = 0xcbf29ce484222325;
        let mut absorb = |bytes: &[u8]| {
            for byte in bytes {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x100000001b3);
            }
            // Record separator so adjacent fields cannot be confused.
            hash ^= 0x1e;
            hash = hash.wrapping_mul(0x100000001b3);
        };
        absorb(remote_url_redacted.as_bytes());
        absorb(r#ref.as_bytes());
        absorb(local_oid.as_bytes());
        absorb(remote_expected_oid.unwrap_or("null").as_bytes());
        format!("fnv1a64:{hash:016x}")
    }

    /// The idempotency key for a push, derived from `{intent, local_oid}`. A
    /// completed receipt under this key means the push already ran; a repeat
    /// execute returns `already_done` without pushing again.
    fn idempotency_key(intent_id: &str, local_oid: &str) -> String {
        format!("push:{intent_id}:{local_oid}")
    }

    // --- enqueue -------------------------------------------------------------

    /// Compute the current local oid for `r#ref` and the remote's observed oid,
    /// persist a [`PushIntent`] with its `effect_digest`, and flag protected
    /// refs. The remote URL is resolved from the *configured* remote `remote`
    /// and credential-redacted before it is hashed or stored.
    pub fn enqueue(
        &self,
        intent_id: &str,
        remote: &str,
        r#ref: &str,
    ) -> Result<PushIntent, GitError> {
        let local_oid = self.local_oid(r#ref)?;
        let remote_url = self.remote_url(remote)?;
        let remote_url_redacted = redact_remote(&remote_url);
        let remote_expected_oid = self.remote_oid(remote, r#ref)?;
        let protected = is_protected_ref(r#ref);
        let effect_digest = Self::compute_effect_digest(
            &remote_url_redacted,
            r#ref,
            &local_oid,
            remote_expected_oid.as_deref(),
        );

        self.conn
            .execute(
                "INSERT OR REPLACE INTO push_intents (
                    intent_id, effect_digest, remote, remote_url_redacted,
                    ref_name, local_oid, remote_expected_oid, protected
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    intent_id,
                    effect_digest,
                    remote,
                    remote_url_redacted,
                    r#ref,
                    local_oid,
                    remote_expected_oid,
                    protected as i64,
                ],
            )
            .map_err(sqlite)?;

        Ok(PushIntent::new(
            intent_id,
            effect_digest,
            remote,
            remote_url_redacted,
            r#ref,
            local_oid,
            remote_expected_oid,
            protected,
        ))
    }

    /// Load a persisted intent by id, or `None` when it does not exist.
    pub fn intent(&self, intent_id: &str) -> Result<Option<PushIntent>, GitError> {
        self.conn
            .query_row(
                "SELECT intent_id, effect_digest, remote, remote_url_redacted,
                        ref_name, local_oid, remote_expected_oid, protected
                 FROM push_intents WHERE intent_id = ?1",
                params![intent_id],
                row_to_intent,
            )
            .optional()
            .map_err(sqlite)
    }

    // --- approve -------------------------------------------------------------

    /// Record an approval for `intent_id` **only** when `effect_digest` equals
    /// the intent's current digest. A mismatch (a stale digest, because the oid
    /// or ref moved) returns `Ok(Err(digest_mismatch))` and records nothing. A
    /// protected ref records the approval with `ack_protected` from `ack_protected`.
    pub fn approve(
        &self,
        approval_id: &str,
        intent_id: &str,
        effect_digest: &str,
        ack_protected: bool,
    ) -> Result<Result<PushApproval, PushError>, GitError> {
        let Some(intent) = self.intent(intent_id)? else {
            return Ok(Err(PushError::unknown_intent()));
        };
        // The supplied digest must match the intent's CURRENT digest. An approval
        // for a wrong/old oid or ref is rejected here so it can never authorize a
        // later execute.
        if intent.effect_digest != effect_digest {
            return Ok(Err(PushError::digest_mismatch()));
        }
        self.conn
            .execute(
                "INSERT OR REPLACE INTO push_approvals (
                    approval_id, intent_id, effect_digest, ack_protected
                ) VALUES (?1, ?2, ?3, ?4)",
                params![approval_id, intent_id, effect_digest, ack_protected as i64],
            )
            .map_err(sqlite)?;
        Ok(Ok(PushApproval::new(approval_id, intent_id)))
    }

    /// The recorded approval for an intent whose digest still matches the
    /// intent's current digest, or `None`. A protected intent additionally
    /// requires the approval to have acknowledged the protected ref.
    fn valid_approval(&self, intent: &PushIntent) -> Result<Option<(String, bool)>, GitError> {
        self.conn
            .query_row(
                "SELECT approval_id, ack_protected FROM push_approvals
                 WHERE intent_id = ?1 AND effect_digest = ?2
                 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                params![intent.intent_id, intent.effect_digest],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? != 0)),
            )
            .optional()
            .map_err(sqlite)
    }

    // --- execute -------------------------------------------------------------

    /// Execute the push for `intent_id`, refusing unless a still-valid approval
    /// exists.
    ///
    /// Refusal cases (each returns `Ok(Err(..))`):
    /// - `unknown_intent` when the intent is not in the store,
    /// - `no_matching_approval` when no approval was recorded for the current
    ///   digest,
    /// - `digest_mismatch` when an approval exists but the recorded digest no
    ///   longer matches the intent's current digest (the oid/ref moved after the
    ///   approval),
    /// - `protected_branch` when the ref is protected and the approval did not
    ///   acknowledge it,
    /// - `push_failed` when the remote push itself fails. On `push_failed`,
    ///   nothing is persisted as completed and the intent stays pending; the
    ///   local commit is untouched.
    ///
    /// On a repeat execute where a completed receipt already exists for
    /// `{intent, local_oid}`, the push is **not** run again and the receipt is
    /// returned with `already_done: true`.
    pub fn execute(&self, intent_id: &str) -> Result<Result<PushReceipt, PushError>, GitError> {
        let Some(intent) = self.intent(intent_id)? else {
            return Ok(Err(PushError::unknown_intent()));
        };
        let idempotency_key = Self::idempotency_key(&intent.intent_id, &intent.local_oid);

        // Idempotency: a completed receipt under this key means the push already
        // ran. Return it without pushing again (push at most once).
        if let Some(remote_oid) = self.completed_remote_oid(&idempotency_key)? {
            return Ok(Ok(PushReceipt::new(remote_oid, idempotency_key, true)));
        }

        // The approval must still match the intent's CURRENT digest. If the
        // intent moved after approval there is no row at the current digest, so a
        // stale approval surfaces as no_matching_approval; a digest recorded at a
        // different value is treated as digest_mismatch.
        let Some((_approval_id, ack_protected)) = self.valid_approval(&intent)? else {
            // Distinguish "an approval exists but at a different digest" from
            // "no approval at all" so callers see the precise refusal.
            if self.has_any_approval(&intent.intent_id)? {
                return Ok(Err(PushError::digest_mismatch()));
            }
            return Ok(Err(PushError::no_matching_approval()));
        };

        if intent.protected && !ack_protected {
            return Ok(Err(PushError::protected_branch()));
        }

        // Run the real push to the configured remote. On failure, persist NOTHING
        // as completed and leave the intent pending for retry.
        if self.run_push(&intent.remote, &intent.r#ref).is_err() {
            self.record_failure(&intent, &idempotency_key, PushErrorCode::PushFailed)?;
            return Ok(Err(PushError::push_failed()));
        }

        // Verify the remote oid landed; this is what we record as the receipt.
        let remote_oid = match self.remote_oid(&intent.remote, &intent.r#ref)? {
            Some(oid) => oid,
            // A push that reports success but leaves no observable oid is treated
            // as a failure: record nothing, keep the intent pending.
            None => {
                self.record_failure(&intent, &idempotency_key, PushErrorCode::PushFailed)?;
                return Ok(Err(PushError::push_failed()));
            }
        };

        self.conn
            .execute(
                "INSERT OR IGNORE INTO push_receipts (idempotency_key, intent_id, remote_oid)
                 VALUES (?1, ?2, ?3)",
                params![idempotency_key, intent.intent_id, remote_oid],
            )
            .map_err(sqlite)?;
        self.conn
            .execute(
                "DELETE FROM push_failure_diagnostics WHERE idempotency_key = ?1",
                params![idempotency_key],
            )
            .map_err(sqlite)?;

        Ok(Ok(PushReceipt::new(remote_oid, idempotency_key, false)))
    }

    fn record_failure(
        &self,
        intent: &PushIntent,
        idempotency_key: &str,
        code: PushErrorCode,
    ) -> Result<(), GitError> {
        self.conn
            .execute(
                "INSERT INTO push_failure_diagnostics (
                    idempotency_key, intent_id, effect_digest, remote,
                    remote_url_redacted, ref_name, local_oid, remote_expected_oid,
                    code, attempts, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, CURRENT_TIMESTAMP)
                ON CONFLICT(idempotency_key) DO UPDATE SET
                    effect_digest = excluded.effect_digest,
                    remote = excluded.remote,
                    remote_url_redacted = excluded.remote_url_redacted,
                    ref_name = excluded.ref_name,
                    local_oid = excluded.local_oid,
                    remote_expected_oid = excluded.remote_expected_oid,
                    code = excluded.code,
                    attempts = push_failure_diagnostics.attempts + 1,
                    updated_at = CURRENT_TIMESTAMP",
                params![
                    idempotency_key,
                    intent.intent_id,
                    intent.effect_digest,
                    intent.remote,
                    intent.remote_url_redacted,
                    intent.r#ref,
                    intent.local_oid,
                    intent.remote_expected_oid,
                    code.as_str(),
                ],
            )
            .map_err(sqlite)?;
        Ok(())
    }

    fn completed_remote_oid(&self, idempotency_key: &str) -> Result<Option<String>, GitError> {
        self.conn
            .query_row(
                "SELECT remote_oid FROM push_receipts WHERE idempotency_key = ?1",
                params![idempotency_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite)
    }

    fn has_any_approval(&self, intent_id: &str) -> Result<bool, GitError> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM push_approvals WHERE intent_id = ?1",
                params![intent_id],
                |row| row.get(0),
            )
            .map_err(sqlite)?;
        Ok(count > 0)
    }

    /// True when a completed receipt exists for the intent at its current local
    /// oid (the push has run at least once).
    pub fn is_completed(&self, intent_id: &str) -> Result<bool, GitError> {
        let Some(intent) = self.intent(intent_id)? else {
            return Ok(false);
        };
        let key = Self::idempotency_key(&intent.intent_id, &intent.local_oid);
        Ok(self.completed_remote_oid(&key)?.is_some())
    }

    // --- status --------------------------------------------------------------

    /// Recover the full outbox state from the store: intents with no completed
    /// receipt and no valid approval (`pending`), intents approved but not yet
    /// completed (`approved`), and completed receipts (`completed`). Survives a
    /// restart because it reads only from SQLite.
    pub fn status(&self) -> Result<PushStatus, GitError> {
        let intents = self.all_intents()?;
        let mut pending = Vec::new();
        let mut approved = Vec::new();
        for intent in intents {
            let key = Self::idempotency_key(&intent.intent_id, &intent.local_oid);
            if self.completed_remote_oid(&key)?.is_some() {
                // Completed intents surface only via `completed` receipts.
                continue;
            }
            if self.valid_approval(&intent)?.is_some() {
                approved.push(intent);
            } else {
                pending.push(intent);
            }
        }
        let failures = self.all_failure_diagnostics()?;
        let completed = self.all_receipts()?;
        Ok(PushStatus::new(pending, approved, failures, completed))
    }

    fn all_intents(&self) -> Result<Vec<PushIntent>, GitError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT intent_id, effect_digest, remote, remote_url_redacted,
                        ref_name, local_oid, remote_expected_oid, protected
                 FROM push_intents ORDER BY created_at ASC, rowid ASC",
            )
            .map_err(sqlite)?;
        let rows = stmt.query_map([], row_to_intent).map_err(sqlite)?;
        let mut intents = Vec::new();
        for row in rows {
            intents.push(row.map_err(sqlite)?);
        }
        Ok(intents)
    }

    fn all_receipts(&self) -> Result<Vec<PushReceipt>, GitError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT idempotency_key, remote_oid FROM push_receipts
                 ORDER BY created_at ASC, rowid ASC",
            )
            .map_err(sqlite)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(PushReceipt::new(
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(0)?,
                    true,
                ))
            })
            .map_err(sqlite)?;
        let mut receipts = Vec::new();
        for row in rows {
            receipts.push(row.map_err(sqlite)?);
        }
        Ok(receipts)
    }

    fn all_failure_diagnostics(&self) -> Result<Vec<PushFailureDiagnostic>, GitError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT failure.intent_id, failure.idempotency_key, failure.effect_digest,
                        failure.remote, failure.remote_url_redacted, failure.ref_name,
                        failure.local_oid, failure.remote_expected_oid, failure.code,
                        failure.attempts
                 FROM push_failure_diagnostics failure
                 LEFT JOIN push_receipts receipt
                    ON receipt.idempotency_key = failure.idempotency_key
                 WHERE receipt.idempotency_key IS NULL
                 ORDER BY failure.updated_at ASC, failure.rowid ASC",
            )
            .map_err(sqlite)?;
        let rows = stmt
            .query_map([], |row| {
                let code = push_error_code_from_str(&row.get::<_, String>(8)?);
                Ok(PushFailureDiagnostic::new(
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    code,
                    row.get::<_, i64>(9)? as u32,
                ))
            })
            .map_err(sqlite)?;
        let mut diagnostics = Vec::new();
        for row in rows {
            diagnostics.push(row.map_err(sqlite)?);
        }
        Ok(diagnostics)
    }

    // --- git plumbing --------------------------------------------------------

    /// The current local oid for `r#ref` (`git rev-parse <ref>`).
    fn local_oid(&self, r#ref: &str) -> Result<String, GitError> {
        let out = git(&self.workspace, &["rev-parse", "--verify", r#ref])?;
        Ok(out.trim().to_string())
    }

    /// The configured URL for the named remote (`git remote get-url <name>`).
    fn remote_url(&self, remote: &str) -> Result<String, GitError> {
        let out = git(&self.workspace, &["remote", "get-url", remote])?;
        Ok(out.trim().to_string())
    }

    /// The remote's currently-observed oid for `r#ref` via `git ls-remote`, or
    /// `None` when the ref does not exist on the remote (a create).
    fn remote_oid(&self, remote: &str, r#ref: &str) -> Result<Option<String>, GitError> {
        // `git ls-remote <remote> <ref>` prints "<oid>\t<refname>" lines, or
        // nothing when the ref is absent. Match the heads ref to avoid tags.
        let out = git(&self.workspace, &["ls-remote", remote, r#ref])?;
        let heads_ref = format!("refs/heads/{ref}", r#ref = r#ref);
        for line in out.lines() {
            let mut fields = line.split('\t');
            let oid = fields.next().unwrap_or("").trim();
            let name = fields.next().unwrap_or("").trim();
            if oid.is_empty() {
                continue;
            }
            if name == heads_ref || name == r#ref {
                return Ok(Some(oid.to_string()));
            }
        }
        // No line matched the heads ref (e.g. only a same-named tag exists);
        // treat this as "not present as a branch on the remote" (a create).
        Ok(None)
    }

    /// Run the real `git push <remote> <ref>` to the *configured* remote. This is
    /// the only remote-writing git invocation in the workspace.
    fn run_push(&self, remote: &str, r#ref: &str) -> Result<(), GitError> {
        // `git push <remote> <ref>:<ref>` makes the destination ref explicit so a
        // local branch always lands as `refs/heads/<ref>` on the remote.
        let refspec = format!("{ref}:{ref}", r#ref = r#ref);
        git(&self.workspace, &["push", remote, &refspec]).map(|_| ())
    }
}

fn row_to_intent(row: &rusqlite::Row<'_>) -> rusqlite::Result<PushIntent> {
    Ok(PushIntent::new(
        row.get::<_, String>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, String>(2)?,
        row.get::<_, String>(3)?,
        row.get::<_, String>(4)?,
        row.get::<_, String>(5)?,
        row.get::<_, Option<String>>(6)?,
        row.get::<_, i64>(7)? != 0,
    ))
}

fn push_error_code_from_str(value: &str) -> PushErrorCode {
    match value {
        "digest_mismatch" => PushErrorCode::DigestMismatch,
        "no_matching_approval" => PushErrorCode::NoMatchingApproval,
        "protected_branch" => PushErrorCode::ProtectedBranch,
        "unknown_intent" => PushErrorCode::UnknownIntent,
        _ => PushErrorCode::PushFailed,
    }
}

/// True when `r#ref` is in the small protected set requiring `--ack-protected`.
pub fn is_protected_ref(r#ref: &str) -> bool {
    PROTECTED_REFS.contains(&r#ref)
}

/// Strip credentials from a remote URL so it can be hashed/stored without
/// leaking userinfo. Mirrors `opensks_git_service::redact_remote` so the two
/// crates agree, but is duplicated here to keep `opensks-git` free of a
/// dependency on the read-only inspection service.
pub fn redact_remote(value: &str) -> String {
    // scheme://[userinfo@]host/...
    if let Some(scheme_end) = value.find("://") {
        let (scheme, rest) = value.split_at(scheme_end + 3);
        let authority_end = rest.find('/').unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(authority_end);
        if let Some(at) = authority.rfind('@') {
            let host = &authority[at + 1..];
            return format!("{scheme}{host}{tail}");
        }
        return value.to_string();
    }
    // scp-like user@host:path — use the LAST '@' so a multi-`@` userinfo
    // (`a@b:tok@host:path`) is fully stripped, and redact when the host part
    // after it contains a ':' OR a '/' so `user:tok@host/path` is never echoed
    // verbatim. A bare `foo@bar` (no ':' or '/') is left untouched. Kept in lock
    // step with `opensks_git_service::redact_remote`.
    if let Some(at) = value.rfind('@') {
        let host_part = &value[at + 1..];
        if host_part.contains(':') || host_part.contains('/') {
            return host_part.to_string();
        }
    }
    value.to_string()
}

fn sqlite(error: rusqlite::Error) -> GitError {
    GitError::GitCommand(format!("push outbox store: {error}"))
}

/// Run a git command in `cwd`. The push executor uses this for both read-only
/// plumbing (`rev-parse`, `ls-remote`, `remote get-url`) and the single
/// remote-writing `push`.
fn git(cwd: &Path, args: &[&str]) -> Result<String, GitError> {
    let output = Command::new("git").args(args).current_dir(cwd).output()?;
    if !output.status.success() {
        return Err(GitError::GitCommand(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A throwaway temp dir, unique per process+call so parallel tests never
    /// collide and we never touch the real opensks repo.
    fn temp_dir(name: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut dir = std::env::temp_dir();
        dir.push(format!("opensks-push-{name}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        dir.canonicalize().expect("canonicalize temp dir")
    }

    fn run(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    fn capture(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A SAFE fixture: a source work repo on branch `branch`, plus a SEPARATE
    /// LOCAL BARE repo added as remote `origin` via an absolute path. Nothing
    /// here ever contacts a network remote.
    struct Fixture {
        source: PathBuf,
        bare: PathBuf,
    }

    fn make_fixture(name: &str, branch: &str) -> Fixture {
        let root = temp_dir(name);
        let source = root.join("source");
        let bare = root.join("remote.git");
        fs::create_dir_all(&source).expect("source dir");

        // Source work repo with one commit on `branch`.
        run(&source, &["init"]);
        run(&source, &["config", "user.email", "opensks@example.test"]);
        run(&source, &["config", "user.name", "OpenSKS Test"]);
        run(&source, &["config", "commit.gpgsign", "false"]);
        run(&source, &["checkout", "-B", branch]);
        fs::write(source.join("file.txt"), "v1\n").expect("write");
        run(&source, &["add", "file.txt"]);
        run(&source, &["commit", "-m", "init"]);

        // A SEPARATE LOCAL BARE repo as the remote — file path only, never a URL.
        // Run `git init --bare <path>` from an existing cwd (the source repo)
        // since the bare dir does not exist yet; git creates it.
        run(&source, &["init", "--bare", bare.to_str().unwrap()]);
        run(
            &source,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );

        Fixture { source, bare }
    }

    fn bare_ref_oid(bare: &Path, branch: &str) -> Option<String> {
        let out = capture(bare, &["for-each-ref", &format!("refs/heads/{branch}")]);
        if out.is_empty() {
            return None;
        }
        // "<oid> commit\trefs/heads/<branch>"
        out.split_whitespace().next().map(str::to_string)
    }

    fn open_outbox(fixture: &Fixture) -> PushOutbox {
        PushOutbox::open_workspace(&fixture.source).expect("open outbox")
    }

    #[test]
    fn execute_before_approval_refuses_and_leaves_remote_unchanged() {
        let fixture = make_fixture("no-approval", "feature");
        let outbox = open_outbox(&fixture);
        outbox
            .enqueue("intent-1", "origin", "feature")
            .expect("enqueue");

        // No approval yet → execute must refuse and never touch the remote.
        let result = outbox.execute("intent-1").expect("execute call");
        let error = result.expect_err("execute without approval refuses");
        assert_eq!(error.error.code.as_str(), "no_matching_approval");

        // The bare remote has no `feature` ref: no remote write happened.
        assert!(
            bare_ref_oid(&fixture.bare, "feature").is_none(),
            "no remote ref must exist before approval"
        );
    }

    #[test]
    fn approve_with_stale_digest_is_rejected_and_execute_still_refuses() {
        let fixture = make_fixture("stale-digest", "feature");
        let outbox = open_outbox(&fixture);
        let intent = outbox
            .enqueue("intent-1", "origin", "feature")
            .expect("enqueue");
        let stale_digest = intent.effect_digest.clone();

        // Move the local oid: a new commit changes rev-parse(feature).
        fs::write(fixture.source.join("file.txt"), "v2\n").expect("write");
        run(&fixture.source, &["add", "file.txt"]);
        run(&fixture.source, &["commit", "-m", "move oid"]);
        // Re-enqueue so the intent's CURRENT digest reflects the moved oid.
        let moved = outbox
            .enqueue("intent-1", "origin", "feature")
            .expect("re-enqueue");
        assert_ne!(moved.effect_digest, stale_digest, "oid move flips digest");

        // Approving with the OLD digest must be rejected with digest_mismatch.
        let approve = outbox
            .approve("approval-1", "intent-1", &stale_digest, false)
            .expect("approve call");
        let error = approve.expect_err("stale digest is rejected");
        assert_eq!(error.error.code.as_str(), "digest_mismatch");

        // Execute still refuses (no usable approval was recorded).
        let exec = outbox.execute("intent-1").expect("execute call");
        let exec_error = exec.expect_err("execute refuses without valid approval");
        assert_eq!(exec_error.error.code.as_str(), "no_matching_approval");
        assert!(bare_ref_oid(&fixture.bare, "feature").is_none());
    }

    #[test]
    fn happy_path_enqueue_approve_execute_pushes_to_bare_remote() {
        let fixture = make_fixture("happy", "feature");
        let outbox = open_outbox(&fixture);
        let intent = outbox
            .enqueue("intent-1", "origin", "feature")
            .expect("enqueue");

        let approve = outbox
            .approve("approval-1", "intent-1", &intent.effect_digest, false)
            .expect("approve call")
            .expect("matching digest approved");
        assert!(approve.matched);

        let receipt = outbox
            .execute("intent-1")
            .expect("execute call")
            .expect("execute pushes");
        assert!(receipt.pushed);
        assert!(!receipt.already_done);

        // The bare remote now has the local oid.
        let local = capture(&fixture.source, &["rev-parse", "feature"]);
        assert_eq!(
            bare_ref_oid(&fixture.bare, "feature").as_deref(),
            Some(local.as_str()),
            "remote oid equals the pushed local oid"
        );
        assert_eq!(receipt.remote_oid, local);
    }

    #[test]
    fn second_execute_is_idempotent_and_does_not_push_again() {
        let fixture = make_fixture("idempotent", "feature");
        let outbox = open_outbox(&fixture);
        let intent = outbox
            .enqueue("intent-1", "origin", "feature")
            .expect("enqueue");
        outbox
            .approve("approval-1", "intent-1", &intent.effect_digest, false)
            .expect("approve call")
            .expect("approved");

        let first = outbox
            .execute("intent-1")
            .expect("execute call")
            .expect("first push");
        assert!(!first.already_done);
        let remote_after_first = bare_ref_oid(&fixture.bare, "feature").expect("remote oid");

        // A second execute must NOT push again: already_done, same remote oid.
        let second = outbox
            .execute("intent-1")
            .expect("execute call")
            .expect("second execute");
        assert!(second.already_done, "repeat execute is idempotent");
        assert_eq!(second.idempotency_key, first.idempotency_key);
        let remote_after_second = bare_ref_oid(&fixture.bare, "feature").expect("remote oid");
        assert_eq!(
            remote_after_first, remote_after_second,
            "remote oid is unchanged by the idempotent repeat"
        );
    }

    #[test]
    fn restart_recovery_reopens_store_and_status_shows_intent() {
        let fixture = make_fixture("restart", "feature");
        let db_path = fixture.source.join(PUSH_OUTBOX_DB_RELATIVE_PATH);
        {
            let outbox = open_outbox(&fixture);
            outbox
                .enqueue("intent-1", "origin", "feature")
                .expect("enqueue");
            // Drop the store handle at the end of this scope (simulate restart).
        }

        // Reopen from the SAME SQLite path and recover the pending intent.
        let reopened = PushOutbox::open(&fixture.source, &db_path).expect("reopen");
        let status = reopened.status().expect("status");
        assert_eq!(
            status.pending.len(),
            1,
            "pending intent recovered after restart: {status:?}"
        );
        assert_eq!(status.pending[0].intent_id, "intent-1");
        assert!(status.approved.is_empty());
        assert!(status.completed.is_empty());
        // The schema version survives too.
        assert_eq!(
            reopened.migration_version().expect("version"),
            PUSH_OUTBOX_MIGRATION_VERSION
        );
    }

    #[test]
    fn push_failure_preserves_local_commit_and_pending_intent() {
        let fixture = make_fixture("push-failure", "feature");
        let outbox = open_outbox(&fixture);
        let intent = outbox
            .enqueue("intent-1", "origin", "feature")
            .expect("enqueue");
        outbox
            .approve("approval-1", "intent-1", &intent.effect_digest, false)
            .expect("approve call")
            .expect("approved");

        // Point origin at a bogus LOCAL path (still never a network remote) so
        // the push fails.
        let bogus = fixture.source.join("does-not-exist.git");
        run(
            &fixture.source,
            &["remote", "set-url", "origin", bogus.to_str().unwrap()],
        );
        let local_before = capture(&fixture.source, &["rev-parse", "feature"]);

        let exec = outbox.execute("intent-1").expect("execute call");
        let error = exec.expect_err("push to a bogus remote fails");
        assert_eq!(error.error.code.as_str(), "push_failed");

        // The local commit is untouched (push and commit are separate effects).
        let local_after = capture(&fixture.source, &["rev-parse", "feature"]);
        assert_eq!(local_before, local_after, "local commit is preserved");

        // The intent is still pending (nothing completed), ready for retry.
        let status = outbox.status().expect("status");
        assert!(
            status.completed.is_empty(),
            "no completed receipt on failure"
        );
        assert!(
            !outbox.is_completed("intent-1").expect("is_completed"),
            "intent is not marked completed after a failed push"
        );
        // It is still approved-but-not-completed, i.e. retryable.
        assert_eq!(status.approved.len(), 1);
        assert_eq!(status.approved[0].intent_id, "intent-1");
        assert_eq!(
            status.failures.len(),
            1,
            "failed push diagnostic is recovered from the durable outbox"
        );
        let failure = &status.failures[0];
        assert_eq!(failure.schema, "opensks.push-failure-diagnostic.v1");
        assert_eq!(failure.intent_id, "intent-1");
        assert_eq!(failure.reason_code, "push_failed");
        assert_eq!(failure.code.as_str(), "push_failed");
        assert_eq!(
            failure.idempotency_key,
            format!("push:intent-1:{local_before}")
        );
        assert_eq!(
            failure.remote_url_redacted,
            fixture.bare.to_string_lossy().as_ref()
        );
        assert_eq!(failure.attempts, 1);

        let reopened =
            PushOutbox::open_workspace(&fixture.source).expect("reopen failed-push outbox");
        let recovered = reopened.status().expect("recovered status");
        assert_eq!(
            recovered.failures, status.failures,
            "failed push diagnostic survives reopening the outbox"
        );
    }

    #[test]
    fn protected_ref_requires_ack_then_allows_push() {
        let fixture = make_fixture("protected", "main");
        let outbox = open_outbox(&fixture);
        let intent = outbox
            .enqueue("intent-main", "origin", "main")
            .expect("enqueue");
        assert!(intent.protected, "main is a protected ref");

        // Approve WITHOUT --ack-protected, then execute → protected_branch.
        outbox
            .approve("approval-1", "intent-main", &intent.effect_digest, false)
            .expect("approve call")
            .expect("approved without ack");
        let exec = outbox.execute("intent-main").expect("execute call");
        let error = exec.expect_err("protected ref without ack is refused");
        assert_eq!(error.error.code.as_str(), "protected_branch");
        assert!(
            bare_ref_oid(&fixture.bare, "main").is_none(),
            "no remote write for an un-acked protected ref"
        );

        // Re-approve WITH --ack-protected → execute now pushes.
        outbox
            .approve("approval-2", "intent-main", &intent.effect_digest, true)
            .expect("approve call")
            .expect("approved with ack");
        let receipt = outbox
            .execute("intent-main")
            .expect("execute call")
            .expect("acked protected push");
        assert!(receipt.pushed);
        let local = capture(&fixture.source, &["rev-parse", "main"]);
        assert_eq!(
            bare_ref_oid(&fixture.bare, "main").as_deref(),
            Some(local.as_str())
        );
    }

    #[test]
    fn effect_digest_changes_with_each_bound_field() {
        let base = PushOutbox::compute_effect_digest("https://h/r.git", "feature", "oid1", None);
        assert_ne!(
            base,
            PushOutbox::compute_effect_digest("https://h/r.git", "feature", "oid2", None),
            "local oid is bound"
        );
        assert_ne!(
            base,
            PushOutbox::compute_effect_digest("https://h/r.git", "other", "oid1", None),
            "ref is bound"
        );
        assert_ne!(
            base,
            PushOutbox::compute_effect_digest("https://other/r.git", "feature", "oid1", None),
            "redacted remote url is bound"
        );
        assert_ne!(
            base,
            PushOutbox::compute_effect_digest("https://h/r.git", "feature", "oid1", Some("rmt")),
            "remote expected oid is bound"
        );
    }

    #[test]
    fn redact_remote_strips_credentials() {
        assert_eq!(
            redact_remote("https://alice:s3cr3t@github.com/acme/repo.git"),
            "https://github.com/acme/repo.git"
        );
        assert_eq!(
            redact_remote("git@github.com:acme/repo.git"),
            "github.com:acme/repo.git"
        );
        // A plain local path is left untouched.
        assert_eq!(redact_remote("/tmp/remote.git"), "/tmp/remote.git");
        // Regression: an scp-shaped credential whose path uses '/' (no ':' in the
        // host tail) must NOT be echoed verbatim — this is the leak the outbox
        // could otherwise persist.
        let red = redact_remote("user:tok@host/acme/repo.git");
        assert!(!red.contains(":tok@"), "leaked credential: {red}");
        assert_eq!(red, "host/acme/repo.git");
        // Regression: a malformed multi-`@` userinfo is fully stripped (last '@').
        assert_eq!(
            redact_remote("a@b:tok@host:acme/repo.git"),
            "host:acme/repo.git"
        );
        // A bare `foo@bar` (no ':' or '/') is not a remote and is left untouched.
        assert_eq!(redact_remote("foo@bar"), "foo@bar");
    }
}
