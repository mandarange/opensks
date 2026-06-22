//! Code patch transaction contracts (recovery release §10).
//!
//! Workers propose changes as [`PatchProposal`]s (patch-only by default); the
//! engine validates pre-image hashes, applies atomically, and records a
//! [`PatchApplyResult`]. Verification (tests/static/security) is captured as a
//! [`VerificationResult`] so completion is evidence-backed, never assumed.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const PATCH_PROPOSAL_SCHEMA: &str = "opensks.patch-proposal.v1";
pub const PATCH_APPLY_RESULT_SCHEMA: &str = "opensks.patch-apply-result.v1";
pub const VERIFICATION_RESULT_SCHEMA: &str = "opensks.verification-result.v1";

/// The filesystem operation a single file patch performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileOperation {
    Create,
    Modify,
    Delete,
    Rename,
}

/// Coarse risk of applying a patch; drives approval requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Destructive,
}

/// A single file's change. `before_hash` is validated against the current
/// on-disk content before apply (optimistic concurrency / no silent overwrite).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilePatch {
    pub path: String,
    pub before_hash: String,
    pub after_hash: String,
    pub unified_diff: String,
    pub operation: FileOperation,
}

/// A proposed multi-file change produced by a worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PatchProposal {
    pub schema: String,
    pub proposal_id: String,
    pub run_id: String,
    pub worker_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    pub base_tree_hash: String,
    pub files: Vec<FilePatch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirement_ids: Vec<String>,
    pub rationale_summary: String,
    pub risk_level: RiskLevel,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
}

impl PatchProposal {
    /// A patch must require approval when it is high/destructive risk.
    pub fn requires_approval(&self) -> bool {
        matches!(self.risk_level, RiskLevel::High | RiskLevel::Destructive)
    }
}

/// The outcome of attempting to apply a proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PatchApplyResult {
    pub schema: String,
    pub proposal_id: String,
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflict_paths: Vec<String>,
    pub rolled_back: bool,
    pub reason_code: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
}

/// The category of verification run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerificationKind {
    Tests,
    Static,
    Security,
    Build,
}

/// An evidence-backed verification outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VerificationResult {
    pub schema: String,
    pub run_id: String,
    pub kind: VerificationKind,
    pub passed: bool,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details_redacted: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_round_trips_and_gates_high_risk() {
        let proposal = PatchProposal {
            schema: PATCH_PROPOSAL_SCHEMA.to_string(),
            proposal_id: "pp-1".to_string(),
            run_id: "r1".to_string(),
            worker_id: "w1".to_string(),
            base_commit: Some("abc".to_string()),
            base_tree_hash: "tree-hash".to_string(),
            files: vec![FilePatch {
                path: "src/lib.rs".to_string(),
                before_hash: "h1".to_string(),
                after_hash: "h2".to_string(),
                unified_diff: "@@ -1 +1 @@\n-a\n+b\n".to_string(),
                operation: FileOperation::Modify,
            }],
            requirement_ids: vec!["req-1".to_string()],
            rationale_summary: "fix parser".to_string(),
            risk_level: RiskLevel::Low,
            evidence_refs: vec![],
        };
        let json = serde_json::to_string(&proposal).unwrap();
        let parsed: PatchProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(proposal, parsed);
        assert!(!proposal.requires_approval());

        let mut destructive = proposal.clone();
        destructive.risk_level = RiskLevel::Destructive;
        assert!(destructive.requires_approval());
    }
}
