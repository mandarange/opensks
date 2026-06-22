//! Line-level text diff DTOs (PR-033).
//!
//! These typed shapes describe the result of comparing an editor's current
//! buffer against the on-disk file. The diff engine lives in the `opensks-cli`
//! crate; this module owns only the wire shapes so the daemon, editor, and CLI
//! share one source of truth.
//!
//! Invariant: a [`TextDiff`] never carries the full file content. Only the
//! changed lines (prefixed `+`/`-`) inside each [`DiffHunk`] are included, so an
//! unchanged region is never echoed back.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const TEXT_DIFF_SCHEMA: &str = "opensks.text-diff.v1";

/// The nature of a single diff hunk relative to the on-disk (old) file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiffHunkKind {
    /// Only new lines were introduced (no old lines removed).
    Added,
    /// Only old lines were dropped (no new lines introduced).
    Removed,
    /// Old lines were replaced by new lines.
    Changed,
}

impl DiffHunkKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Changed => "changed",
        }
    }
}

/// One contiguous block of difference between the on-disk file and the buffer.
///
/// `old_start`/`old_lines` index the on-disk file; `new_start`/`new_lines`
/// index the buffer. Line numbers are 1-based; a zero `*_lines` count with a
/// `*_start` pointing at the insertion/deletion site is used for pure
/// additions/removals (matching unified-diff conventions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiffHunk {
    pub kind: DiffHunkKind,
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    /// The changed lines, each prefixed with `-` (removed from disk) or `+`
    /// (added in the buffer). Removed lines precede added lines within a hunk.
    pub lines: Vec<String>,
}

/// The full line-level diff of a buffer against its on-disk counterpart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TextDiff {
    pub schema: String,
    /// Workspace-relative path of the file being compared.
    pub path: String,
    /// True when the buffer differs from disk (i.e. at least one hunk).
    pub changed: bool,
    #[serde(default)]
    pub hunks: Vec<DiffHunk>,
    pub added_lines: usize,
    pub removed_lines: usize,
}

impl TextDiff {
    /// Construct a diff result, deriving `changed`, `added_lines`, and
    /// `removed_lines` from the supplied hunks.
    pub fn new(path: impl Into<String>, hunks: Vec<DiffHunk>) -> Self {
        let added_lines = hunks
            .iter()
            .map(|hunk| {
                hunk.lines
                    .iter()
                    .filter(|line| line.starts_with('+'))
                    .count()
            })
            .sum();
        let removed_lines = hunks
            .iter()
            .map(|hunk| {
                hunk.lines
                    .iter()
                    .filter(|line| line.starts_with('-'))
                    .count()
            })
            .sum();
        Self {
            schema: TEXT_DIFF_SCHEMA.to_string(),
            path: path.into(),
            changed: !hunks.is_empty(),
            hunks,
            added_lines,
            removed_lines,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_diff_is_unchanged() {
        let diff = TextDiff::new("src/lib.rs", Vec::new());
        assert!(!diff.changed);
        assert_eq!(diff.added_lines, 0);
        assert_eq!(diff.removed_lines, 0);
        let json = serde_json::to_string(&diff).expect("serialize");
        assert!(json.contains("\"schema\":\"opensks.text-diff.v1\""));
    }

    #[test]
    fn totals_are_derived_from_hunks() {
        let diff = TextDiff::new(
            "src/lib.rs",
            vec![DiffHunk {
                kind: DiffHunkKind::Changed,
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 2,
                lines: vec![
                    "-old".to_string(),
                    "+new-a".to_string(),
                    "+new-b".to_string(),
                ],
            }],
        );
        assert!(diff.changed);
        assert_eq!(diff.added_lines, 2);
        assert_eq!(diff.removed_lines, 1);
        let decoded: TextDiff =
            serde_json::from_str(&serde_json::to_string(&diff).expect("ser")).expect("de");
        assert_eq!(decoded, diff);
    }
}
