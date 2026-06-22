//! Durable push-outbox DTOs (PR-036).
//!
//! These typed shapes describe the *approval-gated, at-most-once* remote push
//! flow exposed as subcommands of the `git` verb: `push-enqueue`,
//! `push-approve`, `push-execute`, and `push-status`. The durable store and the
//! push executor live in the `opensks-git` crate; this module owns only the wire
//! shapes so the daemon, editor, and CLI share one source of truth.
//!
//! Invariants:
//! - A push is the only contract here that models a *remote* effect, and it is
//!   gated behind a two-step intent → approval → execute handshake.
//! - The `effect_digest` binds the redacted remote URL, the ref, the local oid,
//!   and the remote-expected oid. An approval is only valid for the digest it
//!   was recorded against; if the oid or ref moves, the digest changes and the
//!   stale approval no longer matches (`digest_mismatch`).
//! - Remote URLs that leave the crate are credential-redacted
//!   (`remote_url_redacted`); a raw URL is never modeled in a field.
//! - Protected refs (`main`/`master`/…) require an explicit `--ack-protected`
//!   acknowledgement at approval time.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const PUSH_INTENT_SCHEMA: &str = "opensks.push-intent.v1";
pub const PUSH_APPROVAL_SCHEMA: &str = "opensks.push-approval.v1";
pub const PUSH_RECEIPT_SCHEMA: &str = "opensks.push-receipt.v1";
pub const PUSH_STATUS_SCHEMA: &str = "opensks.push-status.v1";
pub const PUSH_ERROR_SCHEMA: &str = "opensks.git-error.v1";

/// A persisted push intent: the redacted remote, the ref, the local oid to be
/// pushed, the remote's currently-observed oid (or `None`), and the
/// `effect_digest` that binds them. Emitted by `push-enqueue` and recovered from
/// the durable store by `push-status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PushIntent {
    pub schema: String,
    pub intent_id: String,
    /// Stable hash over {redacted remote url, ref, local_oid, remote_expected_oid}.
    pub effect_digest: String,
    pub remote: String,
    /// Credential-redacted remote URL; never carries userinfo.
    pub remote_url_redacted: String,
    #[serde(rename = "ref")]
    pub r#ref: String,
    pub local_oid: String,
    /// The remote's observed oid for `ref` at enqueue time, or `None` when the
    /// ref does not yet exist on the remote (a create).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_expected_oid: Option<String>,
    /// True when `ref` is a protected branch (`main`/`master`/…); execution then
    /// requires an approval recorded with `--ack-protected`.
    pub protected: bool,
}

impl PushIntent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        intent_id: impl Into<String>,
        effect_digest: impl Into<String>,
        remote: impl Into<String>,
        remote_url_redacted: impl Into<String>,
        r#ref: impl Into<String>,
        local_oid: impl Into<String>,
        remote_expected_oid: Option<String>,
        protected: bool,
    ) -> Self {
        Self {
            schema: PUSH_INTENT_SCHEMA.to_string(),
            intent_id: intent_id.into(),
            effect_digest: effect_digest.into(),
            remote: remote.into(),
            remote_url_redacted: remote_url_redacted.into(),
            r#ref: r#ref.into(),
            local_oid: local_oid.into(),
            remote_expected_oid,
            protected,
        }
    }
}

/// A recorded approval for a specific intent at a specific digest. Only an
/// approval whose `intent_id` and digest still match the intent's *current*
/// digest authorizes a push.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PushApproval {
    pub schema: String,
    pub approval_id: String,
    pub intent_id: String,
    /// Always true on a recorded approval; the wire shape carries it explicitly
    /// so a future deny path can reuse the contract.
    pub matched: bool,
}

impl PushApproval {
    pub fn new(approval_id: impl Into<String>, intent_id: impl Into<String>) -> Self {
        Self {
            schema: PUSH_APPROVAL_SCHEMA.to_string(),
            approval_id: approval_id.into(),
            intent_id: intent_id.into(),
            matched: true,
        }
    }
}

/// The receipt for an executed push. `already_done` is true when a prior
/// completed receipt with the same idempotency key was reused (the push ran at
/// most once).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PushReceipt {
    pub schema: String,
    pub pushed: bool,
    pub remote_oid: String,
    pub idempotency_key: String,
    pub already_done: bool,
}

impl PushReceipt {
    pub fn new(
        remote_oid: impl Into<String>,
        idempotency_key: impl Into<String>,
        already_done: bool,
    ) -> Self {
        Self {
            schema: PUSH_RECEIPT_SCHEMA.to_string(),
            pushed: true,
            remote_oid: remote_oid.into(),
            idempotency_key: idempotency_key.into(),
            already_done,
        }
    }
}

/// The durable push outbox state, recovered from SQLite: intents still awaiting
/// approval, intents approved but not yet executed, and completed receipts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PushStatus {
    pub schema: String,
    #[serde(default)]
    pub pending: Vec<PushIntent>,
    #[serde(default)]
    pub approved: Vec<PushIntent>,
    #[serde(default)]
    pub completed: Vec<PushReceipt>,
}

impl PushStatus {
    pub fn new(
        pending: Vec<PushIntent>,
        approved: Vec<PushIntent>,
        completed: Vec<PushReceipt>,
    ) -> Self {
        Self {
            schema: PUSH_STATUS_SCHEMA.to_string(),
            pending,
            approved,
            completed,
        }
    }
}

/// A machine-readable code for a refused push operation, serialized as the
/// shared `opensks.git-error.v1` contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PushErrorCode {
    /// The supplied `--effect-digest` did not match the intent's current digest
    /// (the oid or ref moved). No usable approval is recorded; execution is
    /// refused.
    DigestMismatch,
    /// No still-valid approval exists for the intent; execution is refused.
    NoMatchingApproval,
    /// The ref is protected and no approval acknowledged it (`--ack-protected`).
    ProtectedBranch,
    /// The remote push itself failed (e.g. an unreachable remote). The local
    /// commit and the pending intent are preserved for retry.
    PushFailed,
    /// The referenced intent does not exist in the durable store.
    UnknownIntent,
}

impl PushErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DigestMismatch => "digest_mismatch",
            Self::NoMatchingApproval => "no_matching_approval",
            Self::ProtectedBranch => "protected_branch",
            Self::PushFailed => "push_failed",
            Self::UnknownIntent => "unknown_intent",
        }
    }
}

/// The inner error object carried by [`PushError`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PushErrorBody {
    pub code: PushErrorCode,
}

/// A refused push operation, serialized as `opensks.git-error.v1`. Shares the
/// `git-error` envelope with the local-mutation errors so the editor parses one
/// refusal shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PushError {
    pub schema: String,
    pub error: PushErrorBody,
}

impl PushError {
    pub fn new(code: PushErrorCode) -> Self {
        Self {
            schema: PUSH_ERROR_SCHEMA.to_string(),
            error: PushErrorBody { code },
        }
    }

    pub fn digest_mismatch() -> Self {
        Self::new(PushErrorCode::DigestMismatch)
    }

    pub fn no_matching_approval() -> Self {
        Self::new(PushErrorCode::NoMatchingApproval)
    }

    pub fn protected_branch() -> Self {
        Self::new(PushErrorCode::ProtectedBranch)
    }

    pub fn push_failed() -> Self {
        Self::new(PushErrorCode::PushFailed)
    }

    pub fn unknown_intent() -> Self {
        Self::new(PushErrorCode::UnknownIntent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_intent_serializes_ref_key_and_redacted_url() {
        let intent = PushIntent::new(
            "intent-1",
            "fnv1a64:deadbeefdeadbeef",
            "origin",
            "https://github.com/acme/repo.git",
            "feature",
            "abc123",
            Some("def456".to_string()),
            false,
        );
        let json = serde_json::to_string(&intent).expect("ser");
        assert!(json.contains("\"schema\":\"opensks.push-intent.v1\""));
        // The Rust field `r#ref` serializes as the JSON key `ref`.
        assert!(json.contains("\"ref\":\"feature\""));
        assert!(json.contains("\"remote_url_redacted\":\"https://github.com/acme/repo.git\""));
        let decoded: PushIntent = serde_json::from_str(&json).expect("de");
        assert_eq!(decoded, intent);
    }

    #[test]
    fn push_intent_omits_null_remote_expected_oid() {
        let intent = PushIntent::new(
            "intent-2",
            "fnv1a64:0000000000000000",
            "origin",
            "https://host/repo.git",
            "feature",
            "abc123",
            None,
            false,
        );
        let json = serde_json::to_string(&intent).expect("ser");
        assert!(!json.contains("remote_expected_oid"));
        let decoded: PushIntent = serde_json::from_str(&json).expect("de");
        assert_eq!(decoded.remote_expected_oid, None);
    }

    #[test]
    fn push_approval_matched_roundtrips() {
        let approval = PushApproval::new("approval-1", "intent-1");
        let json = serde_json::to_string(&approval).expect("ser");
        assert!(json.contains("\"schema\":\"opensks.push-approval.v1\""));
        assert!(json.contains("\"matched\":true"));
        let decoded: PushApproval = serde_json::from_str(&json).expect("de");
        assert_eq!(decoded, approval);
    }

    #[test]
    fn push_receipt_carries_already_done() {
        let first = PushReceipt::new("def456", "idem-1", false);
        assert!(first.pushed);
        assert!(!first.already_done);
        let json = serde_json::to_string(&first).expect("ser");
        assert!(json.contains("\"schema\":\"opensks.push-receipt.v1\""));
        let repeat = PushReceipt::new("def456", "idem-1", true);
        assert!(repeat.already_done);
    }

    #[test]
    fn push_error_codes_serialize_snake_case() {
        for (error, needle) in [
            (PushError::digest_mismatch(), "digest_mismatch"),
            (PushError::no_matching_approval(), "no_matching_approval"),
            (PushError::protected_branch(), "protected_branch"),
            (PushError::push_failed(), "push_failed"),
            (PushError::unknown_intent(), "unknown_intent"),
        ] {
            let json = serde_json::to_string(&error).expect("ser");
            assert!(json.contains("\"schema\":\"opensks.git-error.v1\""));
            assert!(
                json.contains(needle),
                "error JSON {json} should contain {needle}"
            );
            let decoded: PushError = serde_json::from_str(&json).expect("de");
            assert_eq!(decoded, error);
        }
    }

    #[test]
    fn push_status_roundtrips_empty() {
        let status = PushStatus::new(Vec::new(), Vec::new(), Vec::new());
        let json = serde_json::to_string(&status).expect("ser");
        assert!(json.contains("\"schema\":\"opensks.push-status.v1\""));
        let decoded: PushStatus = serde_json::from_str(&json).expect("de");
        assert_eq!(decoded, status);
    }
}
