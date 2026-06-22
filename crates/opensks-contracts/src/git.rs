//! Read-only Git inspection DTOs (PR-034).
//!
//! These typed shapes describe the result of inspecting a workspace's Git
//! repository in read-only mode: working-tree status, the local branch list,
//! and a parsed unified diff. The read-only service implementation lives in the
//! `opensks-git-service` crate; this module owns only the wire shapes so the
//! daemon, editor, and CLI share one source of truth.
//!
//! Invariants:
//! - No mutation is ever modeled here. There is no commit/stage/switch/push
//!   shape; these contracts only ever *report* repository state.
//! - Remote and upstream strings never carry credentials. A URL such as
//!   `https://user:token@host/repo.git` is redacted before it reaches any field
//!   in these structs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const GIT_STATUS_SCHEMA: &str = "opensks.git-status.v1";
pub const GIT_BRANCHES_SCHEMA: &str = "opensks.git-branches.v1";
pub const GIT_DIFF_SCHEMA: &str = "opensks.git-diff.v1";

/// The classification of a single status entry, derived from the porcelain XY
/// code pair. This is the editor-facing label; the raw `index_status` /
/// `worktree_status` characters are also preserved for callers that need them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GitStatusKind {
    /// Tracked content changed (in the index, the worktree, or both).
    Modified,
    /// A new path was added to the index.
    Added,
    /// A tracked path was deleted.
    Deleted,
    /// A tracked path was renamed (carries `orig_path`).
    Renamed,
    /// A tracked path was copied from another (carries `orig_path`).
    Copied,
    /// An untracked path present in the worktree.
    Untracked,
    /// A path with an unresolved merge conflict.
    Conflicted,
    /// An ignored path surfaced by status.
    Ignored,
}

impl GitStatusKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Modified => "modified",
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Copied => "copied",
            Self::Untracked => "untracked",
            Self::Conflicted => "conflicted",
            Self::Ignored => "ignored",
        }
    }
}

/// One entry from `git status`. `index_status` / `worktree_status` hold the raw
/// porcelain XY characters (a single character each; `" "` means unmodified in
/// that column). `orig_path` is populated only for renames and copies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitStatusEntry {
    /// Workspace-relative path (the destination path for a rename/copy).
    pub path: String,
    /// The source path for a rename/copy; `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orig_path: Option<String>,
    /// Raw porcelain index (staged) status character.
    pub index_status: String,
    /// Raw porcelain worktree (unstaged) status character.
    pub worktree_status: String,
    /// The editor-facing classification of this entry.
    pub kind: GitStatusKind,
}

/// The result of a read-only `git status` inspection.
///
/// When the workspace is not inside a Git repository, only `schema` and
/// `in_repo: false` are meaningful; the remaining fields carry their defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitStatus {
    pub schema: String,
    /// True when the workspace resolves to a Git repository.
    pub in_repo: bool,
    /// Current branch name, or `None` when detached/unborn or not in a repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// True when HEAD is detached.
    #[serde(default)]
    pub detached: bool,
    /// Configured upstream (e.g. `origin/main`), credentials redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// Commits ahead of upstream.
    #[serde(default)]
    pub ahead: u32,
    /// Commits behind upstream.
    #[serde(default)]
    pub behind: u32,
    /// True when any tracked file differs from HEAD or any untracked file is
    /// present (i.e. there is at least one status entry).
    #[serde(default)]
    pub is_dirty: bool,
    #[serde(default)]
    pub entries: Vec<GitStatusEntry>,
}

impl GitStatus {
    /// A minimal status object for a workspace that is not a Git repository.
    pub fn not_in_repo() -> Self {
        Self {
            schema: GIT_STATUS_SCHEMA.to_string(),
            in_repo: false,
            branch: None,
            detached: false,
            upstream: None,
            ahead: 0,
            behind: 0,
            is_dirty: false,
            entries: Vec::new(),
        }
    }
}

/// One branch in the local branch list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitBranchInfo {
    pub name: String,
    pub is_current: bool,
    /// Configured upstream for this branch, credentials redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    #[serde(default)]
    pub ahead: u32,
    #[serde(default)]
    pub behind: u32,
    /// Absolute path of the worktree this branch is checked out in, when it is
    /// checked out in a *different* worktree than the current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    /// True when this branch is checked out in another worktree (and so cannot
    /// be checked out here without detaching it first).
    #[serde(default)]
    pub checked_out_elsewhere: bool,
}

/// The result of a read-only `git branch` inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitBranches {
    pub schema: String,
    /// Current branch name, or `None` when detached/unborn or not in a repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    #[serde(default)]
    pub branches: Vec<GitBranchInfo>,
}

impl GitBranches {
    /// An empty branch listing for a workspace that is not a Git repository.
    pub fn not_in_repo() -> Self {
        Self {
            schema: GIT_BRANCHES_SCHEMA.to_string(),
            current: None,
            branches: Vec::new(),
        }
    }
}

/// One contiguous hunk of a unified diff for a single file.
///
/// `old_start`/`old_lines` index the pre-image; `new_start`/`new_lines` index
/// the post-image. Line numbers are 1-based, matching unified-diff conventions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitDiffHunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    /// The hunk body lines, each prefixed with ` ` (context), `+` (added), or
    /// `-` (removed). Context lines are retained so the hunk can be rendered.
    pub lines: Vec<String>,
}

/// The per-file portion of a diff result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitDiffFile {
    /// Workspace-relative path (the destination path for a rename).
    pub path: String,
    /// The source path for a rename; `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orig_path: Option<String>,
    /// True when git reported this file as binary; `hunks` is then empty.
    #[serde(default)]
    pub is_binary: bool,
    #[serde(default)]
    pub hunks: Vec<GitDiffHunk>,
}

/// The result of a read-only `git diff` inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitDiff {
    pub schema: String,
    #[serde(default)]
    pub files: Vec<GitDiffFile>,
}

impl GitDiff {
    pub fn new(files: Vec<GitDiffFile>) -> Self {
        Self {
            schema: GIT_DIFF_SCHEMA.to_string(),
            files,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_kind_serializes_snake_case() {
        let json = serde_json::to_string(&GitStatusKind::Untracked).expect("ser");
        assert_eq!(json, "\"untracked\"");
        assert_eq!(GitStatusKind::Renamed.as_str(), "renamed");
    }

    #[test]
    fn not_in_repo_status_is_minimal() {
        let status = GitStatus::not_in_repo();
        let json = serde_json::to_string(&status).expect("ser");
        assert!(json.contains("\"schema\":\"opensks.git-status.v1\""));
        assert!(json.contains("\"in_repo\":false"));
        // Optional fields are omitted when None.
        assert!(!json.contains("\"branch\""));
        assert!(!json.contains("\"upstream\""));
        let decoded: GitStatus = serde_json::from_str(&json).expect("de");
        assert_eq!(decoded, status);
    }

    #[test]
    fn diff_file_roundtrips_with_hunk() {
        let diff = GitDiff::new(vec![GitDiffFile {
            path: "a.rs".to_string(),
            orig_path: None,
            is_binary: false,
            hunks: vec![GitDiffHunk {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                lines: vec!["-old".to_string(), "+new".to_string()],
            }],
        }]);
        let json = serde_json::to_string(&diff).expect("ser");
        assert!(json.contains("\"schema\":\"opensks.git-diff.v1\""));
        let decoded: GitDiff = serde_json::from_str(&json).expect("de");
        assert_eq!(decoded, diff);
    }
}
