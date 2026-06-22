//! Read-only Git inspection service (PR-034).
//!
//! This crate shells `git` in **read-only mode only** to report repository
//! state: working-tree status, the local branch list, and parsed diffs. It is
//! the production counterpart of the wire shapes in [`opensks_contracts::git`].
//!
//! # Read-only invariant
//!
//! Every git invocation in this crate is an inspection command (`status`,
//! `branch`, `worktree list`, `diff`, `rev-parse`). There is deliberately **no**
//! code path that commits, stages, switches, or pushes — the only mutating
//! verbs git offers are never assembled here. Callers that need mutation use the
//! approval-gated outbox in `opensks-git`; this crate intentionally exposes none
//! of it.
//!
//! # Redaction invariant
//!
//! Remote and upstream strings can contain credentials when a remote URL is of
//! the form `https://user:token@host/repo.git` (or an `scp`-like
//! `user@host:path`). Although `git status`/`git branch` normally emit the short
//! `remote/branch` form rather than a URL, this crate defends in depth: every
//! upstream/remote string that leaves the crate is passed through
//! [`redact_remote`], which strips any URL userinfo before it reaches a typed
//! field.

use std::path::Path;
use std::process::Command;

use opensks_contracts::{
    GitBranchInfo, GitBranches, GitDiff, GitDiffFile, GitDiffHunk, GitStatus, GitStatusEntry,
    GitStatusKind,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitServiceError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("git command failed: {0}")]
    GitCommand(String),
    #[error("could not parse git output: {0}")]
    Parse(String),
}

/// Options for [`diff`].
#[derive(Debug, Clone, Default)]
pub struct DiffOptions {
    /// Limit the diff to a single workspace-relative path.
    pub path: Option<String>,
    /// Diff the index against HEAD (`git diff --staged`) instead of the
    /// worktree against the index (`git diff`).
    pub staged: bool,
}

/// True when `workspace` resolves to a Git repository.
pub fn in_repo(workspace: &Path) -> bool {
    matches!(
        git(workspace, &["rev-parse", "--is-inside-work-tree"]),
        Ok(output) if output.trim() == "true"
    )
}

/// Read-only `git status`. Prefers porcelain v2 (`--porcelain=v2 --branch -z`),
/// which is robust to renames and paths with spaces/newlines, and falls back to
/// porcelain v1 when v2 is unsupported. Returns a minimal object when the
/// workspace is not a Git repository.
pub fn status(workspace: &Path) -> Result<GitStatus, GitServiceError> {
    if !in_repo(workspace) {
        return Ok(GitStatus::not_in_repo());
    }
    match git(
        workspace,
        &["status", "--porcelain=v2", "--branch", "-z", "--"],
    ) {
        Ok(raw) => parse_status_v2(&raw),
        // v2 unsupported (ancient git) — fall back to v1.
        Err(GitServiceError::GitCommand(_)) => {
            let raw = git(
                workspace,
                &["status", "--porcelain", "--branch", "-z", "--"],
            )?;
            parse_status_v1(&raw)
        }
        Err(other) => Err(other),
    }
}

/// Read-only branch list. Combines `git branch --format=...` with
/// `git worktree list --porcelain` so a branch checked out in another worktree
/// is flagged. Returns an empty listing when the workspace is not a repository.
pub fn branches(workspace: &Path) -> Result<GitBranches, GitServiceError> {
    if !in_repo(workspace) {
        return Ok(GitBranches::not_in_repo());
    }
    // %(HEAD) is "*" for the current branch; the upstream short name and
    // ahead/behind come from %(upstream:short) and %(upstream:track). The unit
    // separator keeps fields unambiguous even if a name contains spaces.
    let format = "%(HEAD)\x1f%(refname:short)\x1f%(upstream:short)\x1f%(upstream:track)";
    let raw = git(
        workspace,
        &["branch", "--list", &format!("--format={format}")],
    )?;
    let occupancy = worktree_occupancy(workspace)?;
    let mut current = None;
    let mut branches = Vec::new();
    for line in raw.lines() {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.splitn(4, '\x1f');
        let head = fields.next().unwrap_or("");
        let name = match fields.next() {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => continue,
        };
        let upstream_raw = fields.next().unwrap_or("");
        let track = fields.next().unwrap_or("");
        let is_current = head == "*";
        if is_current {
            current = Some(name.clone());
        }
        let upstream = if upstream_raw.is_empty() {
            None
        } else {
            Some(redact_remote(upstream_raw))
        };
        let (ahead, behind) = parse_track(track);
        let worktree_path = occupancy
            .iter()
            .find(|(branch, _)| branch == &name)
            .map(|(_, path)| path.clone());
        // A branch is "checked out elsewhere" only when occupied by a worktree
        // that is not this one (the current branch occupies *this* worktree).
        let checked_out_elsewhere = worktree_path.is_some() && !is_current;
        branches.push(GitBranchInfo {
            name,
            is_current,
            upstream,
            ahead,
            behind,
            worktree_path: if checked_out_elsewhere {
                worktree_path
            } else {
                None
            },
            checked_out_elsewhere,
        });
    }
    Ok(GitBranches {
        schema: opensks_contracts::GIT_BRANCHES_SCHEMA.to_string(),
        current,
        branches,
    })
}

/// Read-only `git diff` (worktree vs index) or `git diff --staged` (index vs
/// HEAD), optionally scoped to one path. Returns an empty file list when the
/// workspace is not a repository.
pub fn diff(workspace: &Path, options: &DiffOptions) -> Result<GitDiff, GitServiceError> {
    if !in_repo(workspace) {
        return Ok(GitDiff::new(Vec::new()));
    }
    let mut args: Vec<&str> = vec!["diff", "--no-color", "--no-ext-diff"];
    if options.staged {
        args.push("--staged");
    }
    args.push("--");
    if let Some(path) = options.path.as_deref() {
        args.push(path);
    }
    let raw = git(workspace, &args)?;
    Ok(GitDiff::new(parse_unified_diff(&raw)))
}

// --- porcelain v2 parsing ---------------------------------------------------

/// Parse `git status --porcelain=v2 --branch -z` output.
///
/// Records are separated by NUL. Each record's leading token identifies its
/// kind:
/// - `#` header lines (`# branch.head`, `# branch.upstream`, `# branch.ab`)
/// - `1` ordinary changed entry: `1 XY sub mH mI mW hH hI <path>`
/// - `2` renamed/copied entry: `2 XY sub mH mI mW hH hI Xscore <path>\0<orig>`
///   (the orig path is the *next* NUL-separated record)
/// - `u` unmerged (conflict) entry: `u XY ... <path>`
/// - `?` untracked, `!` ignored: `? <path>` / `! <path>`
fn parse_status_v2(raw: &str) -> Result<GitStatus, GitServiceError> {
    let mut records = raw.split('\0').peekable();
    let mut branch = None;
    let mut detached = false;
    let mut upstream = None;
    let mut ahead = 0u32;
    let mut behind = 0u32;
    let mut entries = Vec::new();

    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        match record.as_bytes()[0] {
            b'#' => {
                let header = record.trim_start_matches('#').trim();
                if let Some(value) = header.strip_prefix("branch.head ") {
                    let value = value.trim();
                    if value == "(detached)" {
                        detached = true;
                    } else {
                        branch = Some(value.to_string());
                    }
                } else if let Some(value) = header.strip_prefix("branch.upstream ") {
                    upstream = Some(redact_remote(value.trim()));
                } else if let Some(value) = header.strip_prefix("branch.ab ") {
                    let (a, b) = parse_ab(value.trim());
                    ahead = a;
                    behind = b;
                }
            }
            b'1' => {
                let entry = parse_v2_ordinary(record)?;
                entries.push(entry);
            }
            b'2' => {
                // Rename/copy: the original path is the following NUL record.
                let orig = records.next().map(str::to_string);
                let entry = parse_v2_rename(record, orig)?;
                entries.push(entry);
            }
            b'u' => {
                let entry = parse_v2_unmerged(record)?;
                entries.push(entry);
            }
            b'?' => {
                let path = record[1..].trim_start().to_string();
                entries.push(GitStatusEntry {
                    path,
                    orig_path: None,
                    index_status: " ".to_string(),
                    worktree_status: "?".to_string(),
                    kind: GitStatusKind::Untracked,
                });
            }
            b'!' => {
                let path = record[1..].trim_start().to_string();
                entries.push(GitStatusEntry {
                    path,
                    orig_path: None,
                    index_status: " ".to_string(),
                    worktree_status: "!".to_string(),
                    kind: GitStatusKind::Ignored,
                });
            }
            _ => {}
        }
    }

    let is_dirty = !entries.is_empty();
    Ok(GitStatus {
        schema: opensks_contracts::GIT_STATUS_SCHEMA.to_string(),
        in_repo: true,
        branch,
        detached,
        upstream,
        ahead,
        behind,
        is_dirty,
        entries,
    })
}

/// `1 XY sub mH mI mW hH hI <path>` — fields are space-separated; the path is
/// the 9th field and may itself contain spaces, so we split with a bounded
/// count and keep the remainder verbatim.
fn parse_v2_ordinary(record: &str) -> Result<GitStatusEntry, GitServiceError> {
    let mut parts = record.splitn(9, ' ');
    let _tag = parts.next();
    let xy = parts
        .next()
        .ok_or_else(|| GitServiceError::Parse(format!("v2 ordinary missing XY: {record:?}")))?;
    // Skip sub, mH, mI, mW, hH, hI (6 fields).
    for _ in 0..6 {
        parts.next();
    }
    let path = parts
        .next()
        .ok_or_else(|| GitServiceError::Parse(format!("v2 ordinary missing path: {record:?}")))?
        .to_string();
    let (index_status, worktree_status) = split_xy(xy);
    let kind = classify_xy(&index_status, &worktree_status);
    Ok(GitStatusEntry {
        path,
        orig_path: None,
        index_status,
        worktree_status,
        kind,
    })
}

/// `2 XY sub mH mI mW hH hI Xscore <path>` — like ordinary but with a rename
/// score field before the path; the original path arrives as the next record.
fn parse_v2_rename(
    record: &str,
    orig_path: Option<String>,
) -> Result<GitStatusEntry, GitServiceError> {
    let mut parts = record.splitn(10, ' ');
    let _tag = parts.next();
    let xy = parts
        .next()
        .ok_or_else(|| GitServiceError::Parse(format!("v2 rename missing XY: {record:?}")))?;
    // Skip sub, mH, mI, mW, hH, hI, Xscore (7 fields).
    for _ in 0..7 {
        parts.next();
    }
    let path = parts
        .next()
        .ok_or_else(|| GitServiceError::Parse(format!("v2 rename missing path: {record:?}")))?
        .to_string();
    let (index_status, worktree_status) = split_xy(xy);
    // The rename/copy intent is carried in the XY code (R or C).
    let kind = classify_xy(&index_status, &worktree_status);
    Ok(GitStatusEntry {
        path,
        orig_path,
        index_status,
        worktree_status,
        kind,
    })
}

/// `u XY sub m1 m2 m3 mW h1 h2 h3 <path>` — an unmerged (conflicted) entry.
fn parse_v2_unmerged(record: &str) -> Result<GitStatusEntry, GitServiceError> {
    let mut parts = record.splitn(11, ' ');
    let _tag = parts.next();
    let xy = parts
        .next()
        .ok_or_else(|| GitServiceError::Parse(format!("v2 unmerged missing XY: {record:?}")))?;
    // Skip sub, m1, m2, m3, mW, h1, h2, h3 (8 fields).
    for _ in 0..8 {
        parts.next();
    }
    let path = parts
        .next()
        .ok_or_else(|| GitServiceError::Parse(format!("v2 unmerged missing path: {record:?}")))?
        .to_string();
    let (index_status, worktree_status) = split_xy(xy);
    Ok(GitStatusEntry {
        path,
        orig_path: None,
        index_status,
        worktree_status,
        kind: GitStatusKind::Conflicted,
    })
}

// --- porcelain v1 fallback --------------------------------------------------

/// Parse `git status --porcelain --branch -z` output (v1). Used only when v2 is
/// unavailable. Records are NUL-separated; renames put the orig path in the
/// *next* record.
fn parse_status_v1(raw: &str) -> Result<GitStatus, GitServiceError> {
    let mut records = raw.split('\0').peekable();
    let mut branch = None;
    let mut detached = false;
    let mut upstream = None;
    let mut ahead = 0u32;
    let mut behind = 0u32;
    let mut entries = Vec::new();

    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        if let Some(rest) = record.strip_prefix("## ") {
            // e.g. "main...origin/main [ahead 1, behind 2]" or "HEAD (no branch)".
            let (name_part, track_part) = match rest.split_once(" [") {
                Some((name, track)) => (name, Some(track.trim_end_matches(']'))),
                None => (rest, None),
            };
            if let Some((local, up)) = name_part.split_once("...") {
                branch = Some(local.to_string());
                upstream = Some(redact_remote(up));
            } else if name_part.contains("(no branch)") || name_part == "HEAD" {
                detached = true;
            } else {
                branch = Some(name_part.to_string());
            }
            if let Some(track) = track_part {
                let (a, b) = parse_v1_track(track);
                ahead = a;
                behind = b;
            }
            continue;
        }
        if record.len() < 3 {
            continue;
        }
        let xy = &record[0..2];
        let path = record[3..].to_string();
        let (index_status, worktree_status) = split_xy(xy);
        let kind = classify_xy(&index_status, &worktree_status);
        // A v1 rename ("R ") consumes the next record as the original path.
        let orig_path = if matches!(kind, GitStatusKind::Renamed | GitStatusKind::Copied) {
            records.next().map(str::to_string)
        } else {
            None
        };
        // For renames, v1 lists "orig -> dest" only without -z; with -z the dest
        // is `path` and orig is the following record, so `path` is already dest.
        entries.push(GitStatusEntry {
            path,
            orig_path,
            index_status,
            worktree_status,
            kind,
        });
    }

    let is_dirty = !entries.is_empty();
    Ok(GitStatus {
        schema: opensks_contracts::GIT_STATUS_SCHEMA.to_string(),
        in_repo: true,
        branch,
        detached,
        upstream,
        ahead,
        behind,
        is_dirty,
        entries,
    })
}

// --- XY classification ------------------------------------------------------

/// Split a two-character XY code into (index, worktree) single-char strings,
/// preserving spaces as `" "`.
fn split_xy(xy: &str) -> (String, String) {
    let mut chars = xy.chars();
    let x = chars.next().unwrap_or(' ');
    let y = chars.next().unwrap_or(' ');
    (x.to_string(), y.to_string())
}

/// Map a porcelain XY code pair to the editor-facing classification.
///
/// Precedence follows git semantics: a conflict marker (any of the unmerged
/// combinations such as `UU`, `AA`, `DD`, or a `U` in either column) wins,
/// then rename/copy/add/delete from the index column, then worktree
/// modification/deletion, defaulting to "modified".
fn classify_xy(index: &str, worktree: &str) -> GitStatusKind {
    let x = index.chars().next().unwrap_or(' ');
    let y = worktree.chars().next().unwrap_or(' ');

    // Unmerged / conflict combinations.
    if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
        return GitStatusKind::Conflicted;
    }
    if x == '?' || y == '?' {
        return GitStatusKind::Untracked;
    }
    if x == '!' || y == '!' {
        return GitStatusKind::Ignored;
    }
    if x == 'R' || y == 'R' {
        return GitStatusKind::Renamed;
    }
    if x == 'C' || y == 'C' {
        return GitStatusKind::Copied;
    }
    if x == 'A' {
        return GitStatusKind::Added;
    }
    if x == 'D' || y == 'D' {
        return GitStatusKind::Deleted;
    }
    GitStatusKind::Modified
}

// --- ahead/behind parsing ---------------------------------------------------

/// Parse a v2 `branch.ab` value, e.g. `+1 -2` -> (1, 2).
fn parse_ab(value: &str) -> (u32, u32) {
    let mut ahead = 0;
    let mut behind = 0;
    for token in value.split_whitespace() {
        if let Some(rest) = token.strip_prefix('+') {
            ahead = rest.parse().unwrap_or(0);
        } else if let Some(rest) = token.strip_prefix('-') {
            behind = rest.parse().unwrap_or(0);
        }
    }
    (ahead, behind)
}

/// Parse a `%(upstream:track)` value, e.g. `[ahead 1, behind 2]`, `[ahead 3]`,
/// `[gone]`, or empty.
fn parse_track(value: &str) -> (u32, u32) {
    let inner = value.trim().trim_start_matches('[').trim_end_matches(']');
    parse_v1_track(inner)
}

/// Parse the inside of a `[ahead N, behind M]` track string.
fn parse_v1_track(value: &str) -> (u32, u32) {
    let mut ahead = 0;
    let mut behind = 0;
    for clause in value.split(',') {
        let clause = clause.trim();
        if let Some(rest) = clause.strip_prefix("ahead ") {
            ahead = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = clause.strip_prefix("behind ") {
            behind = rest.trim().parse().unwrap_or(0);
        }
    }
    (ahead, behind)
}

// --- worktree occupancy -----------------------------------------------------

/// Map of branch name -> worktree path, from `git worktree list --porcelain`.
/// Only branches actually checked out in some worktree appear.
fn worktree_occupancy(workspace: &Path) -> Result<Vec<(String, String)>, GitServiceError> {
    let raw = git(workspace, &["worktree", "list", "--porcelain"])?;
    let mut occupancy = Vec::new();
    let mut current_path: Option<String> = None;
    for line in raw.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(path.to_string());
        } else if let Some(branch_ref) = line.strip_prefix("branch ") {
            // branch is a full ref, e.g. refs/heads/feature.
            let short = branch_ref
                .strip_prefix("refs/heads/")
                .unwrap_or(branch_ref)
                .to_string();
            if let Some(path) = current_path.clone() {
                occupancy.push((short, path));
            }
        } else if line.is_empty() {
            current_path = None;
        }
    }
    Ok(occupancy)
}

// --- unified diff parsing ---------------------------------------------------

/// Parse `git diff` unified output into per-file hunks, flagging binary files.
fn parse_unified_diff(raw: &str) -> Vec<GitDiffFile> {
    let mut files: Vec<GitDiffFile> = Vec::new();
    let mut lines = raw.lines().peekable();

    while let Some(line) = lines.next() {
        if !line.starts_with("diff --git ") {
            continue;
        }
        let (mut path, mut orig_path) = parse_diff_git_header(line);
        let mut is_binary = false;
        let mut hunks: Vec<GitDiffHunk> = Vec::new();

        while let Some(&peek) = lines.peek() {
            if peek.starts_with("diff --git ") {
                break;
            }
            let line = lines.next().unwrap();
            if let Some(rest) = line.strip_prefix("rename from ") {
                orig_path = Some(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("rename to ") {
                path = rest.to_string();
            } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
                is_binary = true;
            } else if let Some(rest) = line.strip_prefix("--- ") {
                if let Some(p) = strip_diff_path(rest) {
                    orig_path.get_or_insert(p);
                }
            } else if let Some(rest) = line.strip_prefix("+++ ") {
                if let Some(p) = strip_diff_path(rest) {
                    path = p;
                }
            } else if line.starts_with("@@ ") {
                if let Some(mut hunk) = parse_hunk_header(line) {
                    while let Some(&body) = lines.peek() {
                        if body.starts_with("@@ ") || body.starts_with("diff --git ") {
                            break;
                        }
                        // "\ No newline at end of file" is metadata, not a body line.
                        let body = lines.next().unwrap();
                        if body.starts_with('\\') {
                            continue;
                        }
                        if body.starts_with('+') || body.starts_with('-') || body.starts_with(' ') {
                            hunk.lines.push(body.to_string());
                        } else if body.is_empty() {
                            // A truly empty line is a context line with no marker.
                            hunk.lines.push(" ".to_string());
                        }
                    }
                    hunks.push(hunk);
                }
            }
        }

        // Normalize orig_path: only meaningful when it differs from path.
        if orig_path.as_deref() == Some(path.as_str()) {
            orig_path = None;
        }
        files.push(GitDiffFile {
            path,
            orig_path,
            is_binary,
            hunks,
        });
    }
    files
}

/// Parse the `diff --git a/<old> b/<new>` header into (new_path, orig_path).
/// Quoted/space paths in this line are best-effort; the authoritative paths come
/// from the `---`/`+++`/`rename` lines that follow.
fn parse_diff_git_header(line: &str) -> (String, Option<String>) {
    let rest = line.trim_start_matches("diff --git ");
    if let Some((a, b)) = rest.split_once(" b/") {
        let old = a.trim_start_matches("a/").to_string();
        let new = b.to_string();
        let orig = (old != new).then_some(old);
        return (new, orig);
    }
    (rest.to_string(), None)
}

/// Strip the `a/`/`b/` prefix from a `---`/`+++` path, returning `None` for
/// `/dev/null` (an add or delete).
fn strip_diff_path(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw == "/dev/null" {
        return None;
    }
    let stripped = raw
        .strip_prefix("a/")
        .or_else(|| raw.strip_prefix("b/"))
        .unwrap_or(raw);
    Some(stripped.to_string())
}

/// Parse `@@ -oldStart,oldLines +newStart,newLines @@ ...` into a hunk shell
/// (with empty `lines`). When a count is omitted git means 1.
fn parse_hunk_header(line: &str) -> Option<GitDiffHunk> {
    let body = line.trim_start_matches("@@ ");
    let body = body.split(" @@").next()?;
    let mut parts = body.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let (old_start, old_lines) = parse_range(old);
    let (new_start, new_lines) = parse_range(new);
    Some(GitDiffHunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        lines: Vec::new(),
    })
}

/// Parse a `start,count` or bare `start` range; a missing count defaults to 1.
fn parse_range(value: &str) -> (usize, usize) {
    match value.split_once(',') {
        Some((start, count)) => (start.parse().unwrap_or(0), count.parse().unwrap_or(0)),
        None => (value.parse().unwrap_or(0), 1),
    }
}

// --- redaction --------------------------------------------------------------

/// Strip credentials from a remote/upstream string.
///
/// Handles two credential-bearing forms while leaving the common short
/// `remote/branch` form untouched:
/// - URL userinfo: `https://user:token@host/path` -> `https://host/path`
///   (also `ssh://user@host/...` and any `scheme://user[:pw]@...`).
/// - scp-like: `user@host:path` -> `host:path` (only when it is not a plain
///   path and not a `remote/branch` short ref).
pub fn redact_remote(value: &str) -> String {
    // scheme://[userinfo@]host/...
    if let Some(scheme_end) = value.find("://") {
        let (scheme, rest) = value.split_at(scheme_end + 3);
        // userinfo, if present, ends at the first '@' before the next '/'.
        let authority_end = rest.find('/').unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(authority_end);
        if let Some(at) = authority.rfind('@') {
            let host = &authority[at + 1..];
            return format!("{scheme}{host}{tail}");
        }
        return value.to_string();
    }
    // scp-like user@host:path — redact only when there is a ':' separating a
    // host from a path (so we never mangle a short "origin/main" ref, which has
    // no '@').
    if let Some(at) = value.find('@') {
        if value[at..].contains(':') {
            return value[at + 1..].to_string();
        }
    }
    value.to_string()
}

// --- shell helper -----------------------------------------------------------

/// Run a read-only git command in `cwd`. All call sites in this crate pass only
/// inspection verbs (`status`, `branch`, `worktree list`, `diff`, `rev-parse`).
fn git(cwd: &Path, args: &[&str]) -> Result<String, GitServiceError> {
    let output = Command::new("git").args(args).current_dir(cwd).output()?;
    if !output.status.success() {
        return Err(GitServiceError::GitCommand(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A throwaway temp dir, unique per process+call so parallel tests never
    /// collide and we never touch the real opensks repo.
    fn temp_dir(name: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "opensks-git-service-{name}-{}-{n}",
            std::process::id()
        ));
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
        assert!(status.success(), "git {args:?} failed");
    }

    fn init_repo(name: &str) -> PathBuf {
        let dir = temp_dir(name);
        run(&dir, &["init"]);
        run(&dir, &["config", "user.email", "opensks@example.test"]);
        run(&dir, &["config", "user.name", "OpenSKS Test"]);
        run(&dir, &["config", "commit.gpgsign", "false"]);
        // Stabilize the initial branch name across git versions.
        run(&dir, &["checkout", "-B", "main"]);
        dir
    }

    fn commit_file(dir: &Path, path: &str, contents: &str, message: &str) {
        let full = dir.join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&full, contents).expect("write");
        run(dir, &["add", path]);
        run(dir, &["commit", "-m", message]);
    }

    fn find<'a>(status: &'a GitStatus, path: &str) -> &'a GitStatusEntry {
        status
            .entries
            .iter()
            .find(|entry| entry.path == path || entry.orig_path.as_deref() == Some(path))
            .unwrap_or_else(|| panic!("no status entry for {path}: {:?}", status.entries))
    }

    // --- porcelain corpus: every status kind ---------------------------------

    #[test]
    fn status_corpus_classifies_every_kind() {
        let dir = init_repo("corpus");
        // Seed several tracked files in one commit.
        let full = |p: &str| dir.join(p);
        for (p, c) in [
            ("modified.txt", "v1\n"),
            ("deleted.txt", "gone\n"),
            ("renamed.txt", "rename me\n"),
        ] {
            fs::write(full(p), c).expect("seed write");
            run(&dir, &["add", p]);
        }
        run(&dir, &["commit", "-m", "seed"]);

        // modified: change a tracked file in the worktree.
        fs::write(full("modified.txt"), "v2\n").expect("modify");
        // deleted: remove a tracked file.
        fs::remove_file(full("deleted.txt")).expect("delete");
        // renamed: git mv (staged rename).
        run(&dir, &["mv", "renamed.txt", "renamed-new.txt"]);
        // staged add: a brand-new file added to the index.
        fs::write(full("staged-add.txt"), "new\n").expect("add write");
        run(&dir, &["add", "staged-add.txt"]);
        // untracked: a file never added.
        fs::write(full("untracked.txt"), "loose\n").expect("untracked write");

        let status = status(&dir).expect("status");
        assert!(status.in_repo);
        assert!(status.is_dirty);
        assert_eq!(status.branch.as_deref(), Some("main"));

        assert_eq!(find(&status, "modified.txt").kind, GitStatusKind::Modified);
        assert_eq!(find(&status, "deleted.txt").kind, GitStatusKind::Deleted);
        assert_eq!(find(&status, "staged-add.txt").kind, GitStatusKind::Added);
        assert_eq!(
            find(&status, "untracked.txt").kind,
            GitStatusKind::Untracked
        );

        let renamed = find(&status, "renamed-new.txt");
        assert_eq!(renamed.kind, GitStatusKind::Renamed);
        assert_eq!(renamed.orig_path.as_deref(), Some("renamed.txt"));
    }

    #[test]
    fn status_detects_merge_conflict() {
        let dir = init_repo("conflict");
        commit_file(&dir, "conflict.txt", "base\n", "base");
        // Branch A.
        run(&dir, &["checkout", "-b", "branch-a"]);
        fs::write(dir.join("conflict.txt"), "from-a\n").expect("write a");
        run(&dir, &["commit", "-am", "a"]);
        // Branch B from main with a divergent change.
        run(&dir, &["checkout", "main"]);
        run(&dir, &["checkout", "-b", "branch-b"]);
        fs::write(dir.join("conflict.txt"), "from-b\n").expect("write b");
        run(&dir, &["commit", "-am", "b"]);
        // Merge A into B -> conflict (merge exits nonzero, so don't assert).
        let _ = Command::new("git")
            .args(["merge", "branch-a"])
            .current_dir(&dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("merge");

        let status = status(&dir).expect("status");
        let entry = find(&status, "conflict.txt");
        assert_eq!(
            entry.kind,
            GitStatusKind::Conflicted,
            "unmerged path is classified as conflicted: {entry:?}"
        );
    }

    #[test]
    fn status_clean_repo_is_not_dirty() {
        let dir = init_repo("clean");
        commit_file(&dir, "file.txt", "stable\n", "init");
        let status = status(&dir).expect("status");
        assert!(status.in_repo);
        assert!(!status.is_dirty);
        assert!(status.entries.is_empty());
        assert_eq!(status.branch.as_deref(), Some("main"));
    }

    #[test]
    fn status_outside_repo_is_minimal() {
        let dir = temp_dir("no-repo");
        fs::write(dir.join("plain.txt"), "hi\n").expect("write");
        let status = status(&dir).expect("status");
        assert!(!status.in_repo);
        assert!(status.branch.is_none());
        assert!(status.entries.is_empty());
    }

    // --- branch state --------------------------------------------------------

    #[test]
    fn branches_report_current_branch() {
        let dir = init_repo("branches");
        commit_file(&dir, "file.txt", "x\n", "init");
        run(&dir, &["branch", "feature"]);
        let branches = branches(&dir).expect("branches");
        assert_eq!(branches.current.as_deref(), Some("main"));
        let names: Vec<&str> = branches.branches.iter().map(|b| b.name.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"feature"));
        let current = branches
            .branches
            .iter()
            .find(|b| b.name == "main")
            .expect("main branch");
        assert!(current.is_current);
        assert!(!current.checked_out_elsewhere);
    }

    #[test]
    fn branches_flag_worktree_occupancy() {
        let dir = init_repo("worktree-occupancy");
        commit_file(&dir, "file.txt", "x\n", "init");
        run(&dir, &["branch", "feature"]);
        // Check `feature` out in a sibling worktree.
        let wt = temp_dir("sibling-worktree");
        // worktree add needs a path that does not yet exist as a repo dir.
        let wt_path = wt.join("wt");
        run(
            &dir,
            &["worktree", "add", wt_path.to_str().unwrap(), "feature"],
        );
        let branches = branches(&dir).expect("branches");
        let feature = branches
            .branches
            .iter()
            .find(|b| b.name == "feature")
            .expect("feature branch");
        assert!(
            feature.checked_out_elsewhere,
            "feature is checked out in another worktree: {feature:?}"
        );
        assert!(feature.worktree_path.is_some());
    }

    // --- diff ----------------------------------------------------------------

    #[test]
    fn diff_reports_added_and_removed_lines() {
        let dir = init_repo("diff-text");
        commit_file(&dir, "file.txt", "line1\nline2\nline3\n", "init");
        fs::write(dir.join("file.txt"), "line1\nCHANGED\nline3\nline4\n").expect("modify");
        let diff = diff(&dir, &DiffOptions::default()).expect("diff");
        let file = diff
            .files
            .iter()
            .find(|f| f.path == "file.txt")
            .expect("file in diff");
        assert!(!file.is_binary);
        assert!(!file.hunks.is_empty());
        let has_added = file
            .hunks
            .iter()
            .any(|h| h.lines.iter().any(|l| l.starts_with('+')));
        let has_removed = file
            .hunks
            .iter()
            .any(|h| h.lines.iter().any(|l| l.starts_with('-')));
        assert!(has_added, "diff carries added lines");
        assert!(has_removed, "diff carries removed lines");
    }

    #[test]
    fn diff_flags_binary_file() {
        let dir = init_repo("diff-binary");
        // Commit a binary blob, then mutate it.
        fs::write(dir.join("blob.bin"), [0u8, 159, 146, 150, 0, 1, 2]).expect("seed binary");
        run(&dir, &["add", "blob.bin"]);
        run(&dir, &["commit", "-m", "binary"]);
        fs::write(dir.join("blob.bin"), [0u8, 1, 2, 3, 159, 146, 150]).expect("mutate binary");
        let diff = diff(&dir, &DiffOptions::default()).expect("diff");
        let file = diff
            .files
            .iter()
            .find(|f| f.path == "blob.bin")
            .expect("binary file in diff");
        assert!(file.is_binary, "binary file is flagged: {file:?}");
        assert!(file.hunks.is_empty());
    }

    #[test]
    fn diff_staged_only_sees_index() {
        let dir = init_repo("diff-staged");
        commit_file(&dir, "file.txt", "a\n", "init");
        // Stage one change, leave another unstaged.
        fs::write(dir.join("file.txt"), "a-staged\n").expect("stage write");
        run(&dir, &["add", "file.txt"]);
        let staged = diff(
            &dir,
            &DiffOptions {
                path: None,
                staged: true,
            },
        )
        .expect("staged diff");
        assert!(
            staged.files.iter().any(|f| f.path == "file.txt"),
            "staged diff sees the staged change"
        );
    }

    #[test]
    fn diff_scoped_to_single_path() {
        let dir = init_repo("diff-path");
        commit_file(&dir, "a.txt", "a\n", "init-a");
        commit_file(&dir, "b.txt", "b\n", "init-b");
        fs::write(dir.join("a.txt"), "a2\n").expect("modify a");
        fs::write(dir.join("b.txt"), "b2\n").expect("modify b");
        let diff = diff(
            &dir,
            &DiffOptions {
                path: Some("a.txt".to_string()),
                staged: false,
            },
        )
        .expect("scoped diff");
        let paths: Vec<&str> = diff.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt"], "diff is scoped to the requested path");
    }

    #[test]
    fn diff_outside_repo_is_empty() {
        let dir = temp_dir("diff-no-repo");
        fs::write(dir.join("plain.txt"), "hi\n").expect("write");
        let diff = diff(&dir, &DiffOptions::default()).expect("diff");
        assert!(diff.files.is_empty());
    }

    // --- redaction -----------------------------------------------------------

    #[test]
    fn redact_strips_https_credentials() {
        assert_eq!(
            redact_remote("https://alice:s3cr3t@github.com/acme/repo.git"),
            "https://github.com/acme/repo.git"
        );
        assert_eq!(
            redact_remote("https://token@github.com/acme/repo.git"),
            "https://github.com/acme/repo.git"
        );
        assert_eq!(
            redact_remote("ssh://git@github.com/acme/repo.git"),
            "ssh://github.com/acme/repo.git"
        );
    }

    #[test]
    fn redact_strips_scp_userinfo_but_keeps_short_refs() {
        assert_eq!(
            redact_remote("git@github.com:acme/repo.git"),
            "github.com:acme/repo.git"
        );
        // A short upstream ref has no credentials and must be left intact.
        assert_eq!(redact_remote("origin/main"), "origin/main");
        assert_eq!(
            redact_remote("upstream/release-1.0"),
            "upstream/release-1.0"
        );
    }

    #[test]
    fn status_redacts_remote_credentials_in_upstream() {
        let dir = init_repo("redact-status");
        commit_file(&dir, "file.txt", "x\n", "init");
        // Configure a credential-bearing remote and an upstream tracking it.
        run(
            &dir,
            &[
                "remote",
                "add",
                "origin",
                "https://alice:s3cr3t@example.test/acme/repo.git",
            ],
        );
        // Create a remote-tracking ref and set upstream without a network fetch.
        run(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        run(&dir, &["branch", "--set-upstream-to=origin/main", "main"]);

        let status = status(&dir).expect("status");
        if let Some(upstream) = &status.upstream {
            assert!(
                !upstream.contains("s3cr3t") && !upstream.contains("alice:"),
                "status upstream must not leak credentials: {upstream}"
            );
        }
        let branches = branches(&dir).expect("branches");
        for branch in &branches.branches {
            if let Some(upstream) = &branch.upstream {
                assert!(
                    !upstream.contains("s3cr3t") && !upstream.contains("alice:"),
                    "branch upstream must not leak credentials: {upstream}"
                );
            }
        }
    }

    // --- read-only invariant -------------------------------------------------

    #[test]
    fn service_source_contains_no_mutating_git_verbs() {
        // Guard: the service must never assemble a mutating git invocation.
        // We scan our own source for write verbs passed to `git(...)`.
        let source = include_str!("lib.rs");
        // Only inspect code above the test module to avoid matching test setup
        // (which legitimately creates fixture repos).
        let cutoff = source
            .find("#[cfg(test)]")
            .expect("test module marker present");
        let production = &source[..cutoff];
        for forbidden in [
            "\"commit\"",
            "\"push\"",
            "\"add\"",
            "\"checkout\"",
            "\"switch\"",
            "\"reset\"",
            "\"merge\"",
            "\"rebase\"",
            "\"stash\"",
            "\"apply\"",
        ] {
            assert!(
                !production.contains(forbidden),
                "read-only service must not reference mutating git verb {forbidden}"
            );
        }
    }
}
