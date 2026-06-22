//! Local Git mutation DTOs (PR-035).
//!
//! These typed shapes describe the *local-only* Git mutations exposed as
//! subcommands of the `git` verb: switch-preflight, create-branch, switch,
//! stage, unstage, commit-preview, and commit. The implementation lives in the
//! `opensks-git-service` crate (mutation module); this module owns only the wire
//! shapes so the daemon, editor, and CLI share one source of truth.
//!
//! Invariants:
//! - No remote write is ever modeled here. There is no push, fetch, or pull
//!   shape; these contracts only ever describe *local* index/branch/commit
//!   mutations.
//! - Secret-looking paths and data-plane paths are never staged or committed.
//!   They surface as `rejected` entries on [`GitStageResult`] and as a hard
//!   refusal on commit.
//! - A commit is gated by a stable `index_hash` computed over the staged path
//!   list and their staged blob oids; a stale preview (a hash that no longer
//!   matches the live index) is refused with [`GitMutationErrorCode::IndexChanged`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const GIT_SWITCH_PREFLIGHT_SCHEMA: &str = "opensks.git-switch-preflight.v1";
pub const GIT_CREATE_BRANCH_SCHEMA: &str = "opensks.git-create-branch.v1";
pub const GIT_SWITCH_SCHEMA: &str = "opensks.git-switch.v1";
pub const GIT_STAGE_SCHEMA: &str = "opensks.git-stage.v1";
pub const GIT_UNSTAGE_SCHEMA: &str = "opensks.git-unstage.v1";
pub const GIT_COMMIT_PREVIEW_SCHEMA: &str = "opensks.git-commit-preview.v1";
pub const GIT_COMMIT_SCHEMA: &str = "opensks.git-commit.v1";
pub const GIT_ERROR_SCHEMA: &str = "opensks.git-error.v1";

/// Why a switch cannot proceed: the worktree is dirty, or there is an
/// unresolved merge conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GitSwitchBlockerKind {
    /// The worktree has uncommitted tracked changes (staged or unstaged) that a
    /// switch would overwrite.
    DirtyWorktree,
    /// There is at least one unresolved merge conflict.
    Conflict,
}

impl GitSwitchBlockerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DirtyWorktree => "dirty_worktree",
            Self::Conflict => "conflict",
        }
    }
}

/// One reason a branch switch is blocked, with the workspace-relative paths that
/// triggered it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitSwitchBlocker {
    pub kind: GitSwitchBlockerKind,
    #[serde(default)]
    pub paths: Vec<String>,
}

/// The result of a read-only switch preflight check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitSwitchPreflight {
    pub schema: String,
    /// True when a switch can proceed without `--force` (no blockers).
    pub can_switch: bool,
    #[serde(default)]
    pub blockers: Vec<GitSwitchBlocker>,
}

impl GitSwitchPreflight {
    pub fn new(blockers: Vec<GitSwitchBlocker>) -> Self {
        Self {
            schema: GIT_SWITCH_PREFLIGHT_SCHEMA.to_string(),
            can_switch: blockers.is_empty(),
            blockers,
        }
    }
}

/// The result of creating a local branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitCreateBranch {
    pub schema: String,
    pub created: bool,
    pub branch: String,
    /// The commit the new branch points at.
    pub head: String,
}

impl GitCreateBranch {
    pub fn new(branch: impl Into<String>, head: impl Into<String>) -> Self {
        Self {
            schema: GIT_CREATE_BRANCH_SCHEMA.to_string(),
            created: true,
            branch: branch.into(),
            head: head.into(),
        }
    }
}

/// The result of switching to a local branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitSwitch {
    pub schema: String,
    pub switched: bool,
    pub branch: String,
}

impl GitSwitch {
    pub fn new(branch: impl Into<String>) -> Self {
        Self {
            schema: GIT_SWITCH_SCHEMA.to_string(),
            switched: true,
            branch: branch.into(),
        }
    }
}

/// Why a path was refused for staging/commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GitStageRejectReason {
    /// The path name looks like a secret (e.g. `id_rsa`, `.env`, `*.pem`).
    SecretRestricted,
    /// The path falls under a local data-plane rule and must never be tracked.
    DataPlane,
}

impl GitStageRejectReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SecretRestricted => "secret_restricted",
            Self::DataPlane => "data_plane",
        }
    }
}

/// One path refused for staging, with the reason it was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitStageRejection {
    pub path: String,
    pub reason: GitStageRejectReason,
}

/// The result of staging one or more paths. Secret/data-plane paths are never
/// added to the index; they appear in `rejected` with the matching reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitStageResult {
    pub schema: String,
    #[serde(default)]
    pub staged: Vec<String>,
    #[serde(default)]
    pub rejected: Vec<GitStageRejection>,
}

impl GitStageResult {
    pub fn new(staged: Vec<String>, rejected: Vec<GitStageRejection>) -> Self {
        Self {
            schema: GIT_STAGE_SCHEMA.to_string(),
            staged,
            rejected,
        }
    }
}

/// The result of unstaging one or more paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitUnstageResult {
    pub schema: String,
    #[serde(default)]
    pub unstaged: Vec<String>,
}

impl GitUnstageResult {
    pub fn new(unstaged: Vec<String>) -> Self {
        Self {
            schema: GIT_UNSTAGE_SCHEMA.to_string(),
            unstaged,
        }
    }
}

/// A preview of the current index: the stable `index_hash` over the staged path
/// list and their staged blob oids, plus the staged paths themselves. Callers
/// pass `index_hash` back to `commit` as `--expected-index-hash`; a mismatch
/// means the index changed since the preview and the commit is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitCommitPreview {
    pub schema: String,
    pub index_hash: String,
    #[serde(default)]
    pub staged_paths: Vec<String>,
    pub has_staged: bool,
}

impl GitCommitPreview {
    pub fn new(index_hash: impl Into<String>, staged_paths: Vec<String>) -> Self {
        let has_staged = !staged_paths.is_empty();
        Self {
            schema: GIT_COMMIT_PREVIEW_SCHEMA.to_string(),
            index_hash: index_hash.into(),
            staged_paths,
            has_staged,
        }
    }
}

/// The result of a local commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitCommit {
    pub schema: String,
    pub committed: bool,
    pub commit: String,
    #[serde(default)]
    pub paths: Vec<String>,
}

impl GitCommit {
    pub fn new(commit: impl Into<String>, paths: Vec<String>) -> Self {
        Self {
            schema: GIT_COMMIT_SCHEMA.to_string(),
            committed: true,
            commit: commit.into(),
            paths,
        }
    }
}

/// A machine-readable code for a refused mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GitMutationErrorCode {
    /// A switch was blocked by a dirty worktree or conflict and `--force` was
    /// not supplied. Carries the blockers.
    SwitchBlocked,
    /// The live index hash no longer matches the expected hash from a preview;
    /// the preview is stale and the commit is refused.
    IndexChanged,
    /// A staged path is secret-looking or a data-plane path; the commit is
    /// refused rather than publishing restricted content.
    SecretRestricted,
    /// Nothing is staged, so there is nothing to commit.
    NothingStaged,
}

impl GitMutationErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SwitchBlocked => "switch_blocked",
            Self::IndexChanged => "index_changed",
            Self::SecretRestricted => "secret_restricted",
            Self::NothingStaged => "nothing_staged",
        }
    }
}

/// The inner error object carried by [`GitMutationError`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitMutationErrorBody {
    pub code: GitMutationErrorCode,
    /// Switch blockers; present only for `switch_blocked`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<GitSwitchBlocker>,
    /// Restricted paths; present only for `secret_restricted`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

/// A refused local Git mutation, serialized as `opensks.git-error.v1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitMutationError {
    pub schema: String,
    pub error: GitMutationErrorBody,
}

impl GitMutationError {
    pub fn new(code: GitMutationErrorCode) -> Self {
        Self {
            schema: GIT_ERROR_SCHEMA.to_string(),
            error: GitMutationErrorBody {
                code,
                blockers: Vec::new(),
                paths: Vec::new(),
            },
        }
    }

    pub fn switch_blocked(blockers: Vec<GitSwitchBlocker>) -> Self {
        let mut error = Self::new(GitMutationErrorCode::SwitchBlocked);
        error.error.blockers = blockers;
        error
    }

    pub fn index_changed() -> Self {
        Self::new(GitMutationErrorCode::IndexChanged)
    }

    pub fn secret_restricted(paths: Vec<String>) -> Self {
        let mut error = Self::new(GitMutationErrorCode::SecretRestricted);
        error.error.paths = paths;
        error
    }

    pub fn nothing_staged() -> Self {
        Self::new(GitMutationErrorCode::NothingStaged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_preflight_can_switch_when_no_blockers() {
        let preflight = GitSwitchPreflight::new(Vec::new());
        assert!(preflight.can_switch);
        let json = serde_json::to_string(&preflight).expect("ser");
        assert!(json.contains("\"schema\":\"opensks.git-switch-preflight.v1\""));
        assert!(json.contains("\"can_switch\":true"));
    }

    #[test]
    fn switch_preflight_blocked_serializes_blocker_kind() {
        let preflight = GitSwitchPreflight::new(vec![GitSwitchBlocker {
            kind: GitSwitchBlockerKind::DirtyWorktree,
            paths: vec!["a.rs".to_string()],
        }]);
        assert!(!preflight.can_switch);
        let json = serde_json::to_string(&preflight).expect("ser");
        assert!(json.contains("\"dirty_worktree\""));
        let decoded: GitSwitchPreflight = serde_json::from_str(&json).expect("de");
        assert_eq!(decoded, preflight);
    }

    #[test]
    fn stage_result_serializes_reason() {
        let result = GitStageResult::new(
            vec!["a.rs".to_string()],
            vec![GitStageRejection {
                path: "id_rsa".to_string(),
                reason: GitStageRejectReason::SecretRestricted,
            }],
        );
        let json = serde_json::to_string(&result).expect("ser");
        assert!(json.contains("\"secret_restricted\""));
        let decoded: GitStageResult = serde_json::from_str(&json).expect("de");
        assert_eq!(decoded, result);
    }

    #[test]
    fn commit_preview_has_staged_reflects_paths() {
        let empty = GitCommitPreview::new("h0", Vec::new());
        assert!(!empty.has_staged);
        let some = GitCommitPreview::new("h1", vec!["a.rs".to_string()]);
        assert!(some.has_staged);
    }

    #[test]
    fn mutation_error_index_changed_roundtrips() {
        let error = GitMutationError::index_changed();
        let json = serde_json::to_string(&error).expect("ser");
        assert!(json.contains("\"schema\":\"opensks.git-error.v1\""));
        assert!(json.contains("\"index_changed\""));
        // blockers/paths are empty and omitted.
        assert!(!json.contains("\"blockers\""));
        assert!(!json.contains("\"paths\""));
        let decoded: GitMutationError = serde_json::from_str(&json).expect("de");
        assert_eq!(decoded, error);
    }

    #[test]
    fn mutation_error_switch_blocked_carries_blockers() {
        let error = GitMutationError::switch_blocked(vec![GitSwitchBlocker {
            kind: GitSwitchBlockerKind::DirtyWorktree,
            paths: vec!["a.rs".to_string()],
        }]);
        let json = serde_json::to_string(&error).expect("ser");
        assert!(json.contains("\"switch_blocked\""));
        assert!(json.contains("\"dirty_worktree\""));
        let decoded: GitMutationError = serde_json::from_str(&json).expect("de");
        assert_eq!(decoded, error);
    }
}
