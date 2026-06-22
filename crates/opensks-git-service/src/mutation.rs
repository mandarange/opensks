//! Local Git **mutation** service (PR-035).
//!
//! This module is the mutating counterpart of the read-only inspection code in
//! [`crate`]. It exposes exactly the local, non-remote mutations the editor
//! needs — switch-preflight, create-branch, switch, stage, unstage,
//! commit-preview, and commit — and nothing else.
//!
//! # Local-only invariant
//!
//! There is deliberately **no** code path here that pushes, fetches, pulls, or
//! otherwise writes to a remote. The only git verbs assembled are local index,
//! branch, and commit operations (`add`, `reset`, `branch`, `switch`, `commit`,
//! plus read-only `status`/`diff`/`rev-parse`/`ls-files`). The grep guard test
//! in this crate asserts no `push`/`fetch`/`pull`/`remote` verb is ever
//! assembled in production code.
//!
//! # Secret and data-plane invariant
//!
//! Secret-looking paths (`id_rsa`, `.env`, `*.pem`, …) and local data-plane
//! paths (from [`opensks_policy::default_data_plane_manifest`]) are never staged
//! or committed. [`stage`] filters them into `rejected`; [`commit`] re-checks
//! the live staged set and refuses outright if a restricted path is present.
//!
//! # Index-hash staleness
//!
//! [`commit_preview`] returns a stable `index_hash` derived from the staged path
//! list and each path's staged blob oid (via `git diff --cached --raw -z`). Any
//! change to the index — adding a path, removing one, or restaging different
//! content — changes the set of (oid, path) pairs and therefore the hash.
//! [`commit`] recomputes the live hash and refuses with `index_changed` when it
//! does not match the caller's `expected_index_hash`, so a commit can never be
//! built from a stale preview.

use std::path::Path;
use std::process::Command;

use opensks_contracts::{
    GitCommit, GitCommitPreview, GitCreateBranch, GitMutationError, GitStageRejectReason,
    GitStageRejection, GitStageResult, GitSwitch, GitSwitchBlocker, GitSwitchBlockerKind,
    GitSwitchPreflight, GitUnstageResult,
};

use crate::GitServiceError;

/// The outcome of a mutation that may be refused with a typed error. The `Err`
/// variant carries a [`GitMutationError`] so callers can serialize the
/// `opensks.git-error.v1` contract and exit nonzero.
pub type MutationOutcome<T> = Result<Result<T, GitMutationError>, GitServiceError>;

// --- switch preflight -------------------------------------------------------

/// Read-only check of whether a branch switch can proceed. Reports a
/// `dirty_worktree` blocker for any tracked path with staged or unstaged
/// changes, and a `conflict` blocker for any unmerged path. A clean worktree
/// yields `can_switch: true` with no blockers.
pub fn switch_preflight(workspace: &Path) -> Result<GitSwitchPreflight, GitServiceError> {
    let blockers = collect_switch_blockers(workspace)?;
    Ok(GitSwitchPreflight::new(blockers))
}

/// Gather dirty/conflict blockers from `git status --porcelain=v2 -z`. Untracked
/// files do not block a switch (git carries them across), so only tracked
/// changes and unmerged entries are reported.
fn collect_switch_blockers(workspace: &Path) -> Result<Vec<GitSwitchBlocker>, GitServiceError> {
    let raw = git(workspace, &["status", "--porcelain=v2", "-z", "--"])?;
    let mut dirty_paths = Vec::new();
    let mut conflict_paths = Vec::new();
    let mut records = raw.split('\0');
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        match record.as_bytes()[0] {
            b'1' => {
                // `1 XY ... <path>` — ordinary change to a tracked path.
                if let Some(path) = record.splitn(9, ' ').nth(8) {
                    dirty_paths.push(path.to_string());
                }
            }
            b'2' => {
                // `2 XY ... <path>` plus the orig path in the next record.
                if let Some(path) = record.splitn(10, ' ').nth(9) {
                    dirty_paths.push(path.to_string());
                }
                // Consume the rename source record so it is not misparsed.
                records.next();
            }
            b'u' => {
                // `u XY ... <path>` — an unmerged (conflicted) path.
                if let Some(path) = record.splitn(11, ' ').nth(10) {
                    conflict_paths.push(path.to_string());
                }
            }
            // `?` untracked and `!` ignored never block a switch.
            _ => {}
        }
    }
    let mut blockers = Vec::new();
    if !conflict_paths.is_empty() {
        blockers.push(GitSwitchBlocker {
            kind: GitSwitchBlockerKind::Conflict,
            paths: conflict_paths,
        });
    }
    if !dirty_paths.is_empty() {
        blockers.push(GitSwitchBlocker {
            kind: GitSwitchBlockerKind::DirtyWorktree,
            paths: dirty_paths,
        });
    }
    Ok(blockers)
}

// --- create branch ----------------------------------------------------------

/// Create a local branch `name` (optionally from `from`, defaulting to the
/// current HEAD) without checking it out. Returns the new branch's head commit.
pub fn create_branch(
    workspace: &Path,
    name: &str,
    from: Option<&str>,
) -> Result<GitCreateBranch, GitServiceError> {
    let mut args = vec!["branch", "--", name];
    if let Some(from) = from {
        args.push(from);
    }
    git(workspace, &args)?;
    let head = rev_parse(workspace, name)?;
    Ok(GitCreateBranch::new(name, head))
}

// --- switch -----------------------------------------------------------------

/// Switch to local branch `target`. When the worktree is dirty or conflicted and
/// `force` is false, the switch is refused with a `switch_blocked` error that
/// carries the blockers. With `force`, the switch is attempted regardless (using
/// `git switch --force`, which discards local changes in the same way the editor
/// has confirmed).
pub fn switch(workspace: &Path, target: &str, force: bool) -> MutationOutcome<GitSwitch> {
    if !force {
        let blockers = collect_switch_blockers(workspace)?;
        if !blockers.is_empty() {
            return Ok(Err(GitMutationError::switch_blocked(blockers)));
        }
    }
    let mut args = vec!["switch"];
    if force {
        args.push("--force");
    }
    args.push("--");
    args.push(target);
    git(workspace, &args)?;
    Ok(Ok(GitSwitch::new(target)))
}

// --- stage / unstage --------------------------------------------------------

/// Stage `paths`, filtering out any secret-looking or data-plane path into
/// `rejected` (those are never added to the index). The remaining paths are
/// `git add`-ed individually so one bad path cannot poison the rest.
pub fn stage(workspace: &Path, paths: &[String]) -> Result<GitStageResult, GitServiceError> {
    let mut staged = Vec::new();
    let mut rejected = Vec::new();
    for path in paths {
        if let Some(reason) = restricted_reason(path) {
            rejected.push(GitStageRejection {
                path: path.clone(),
                reason,
            });
            continue;
        }
        // Stage exactly this path; `--` guards against a path that looks like a
        // flag.
        git(workspace, &["add", "--", path])?;
        staged.push(path.clone());
    }
    Ok(GitStageResult::new(staged, rejected))
}

/// Unstage `paths` from the index (`git reset -- <path>`), leaving worktree
/// content untouched.
pub fn unstage(workspace: &Path, paths: &[String]) -> Result<GitUnstageResult, GitServiceError> {
    let mut unstaged = Vec::new();
    for path in paths {
        // `git reset -- <path>` removes the path from the index without touching
        // the worktree. It is a local index operation, never a remote write.
        git(workspace, &["reset", "--quiet", "--", path])?;
        unstaged.push(path.clone());
    }
    Ok(GitUnstageResult::new(unstaged))
}

// --- commit preview / commit ------------------------------------------------

/// Preview the current index: the staged path list and a stable `index_hash`
/// over the (staged blob oid, path) pairs. Callers pass `index_hash` back to
/// [`commit`] to detect a stale preview.
pub fn commit_preview(workspace: &Path) -> Result<GitCommitPreview, GitServiceError> {
    let (index_hash, staged_paths) = index_state(workspace)?;
    Ok(GitCommitPreview::new(index_hash, staged_paths))
}

/// Commit the current index with `message`, gated on `expected_index_hash`.
///
/// Refusal cases (each returns `Ok(Err(..))` with a typed error and a nonzero
/// exit upstream):
/// - `nothing_staged` when the index is empty,
/// - `index_changed` when the live index hash differs from `expected_index_hash`
///   (the preview is stale),
/// - `secret_restricted` when any staged path is secret-looking or a data-plane
///   path (the commit is refused rather than publishing restricted content).
pub fn commit(
    workspace: &Path,
    message: &str,
    expected_index_hash: &str,
) -> MutationOutcome<GitCommit> {
    let (index_hash, staged_paths) = index_state(workspace)?;
    if staged_paths.is_empty() {
        return Ok(Err(GitMutationError::nothing_staged()));
    }
    if index_hash != expected_index_hash {
        return Ok(Err(GitMutationError::index_changed()));
    }
    // Defense in depth: even though `stage` filters restricted paths, re-check
    // the live staged set so a path staged out-of-band (or by an older client)
    // can never be committed.
    let restricted: Vec<String> = staged_paths
        .iter()
        .filter(|path| restricted_reason(path).is_some())
        .cloned()
        .collect();
    if !restricted.is_empty() {
        return Ok(Err(GitMutationError::secret_restricted(restricted)));
    }
    // `git commit -m <message>` commits exactly the current index — no `-a`, so
    // only reviewed/staged paths are included.
    git(workspace, &["commit", "--message", message])?;
    let sha = rev_parse(workspace, "HEAD")?;
    Ok(Ok(GitCommit::new(sha, staged_paths)))
}

// --- restricted-path policy -------------------------------------------------

/// Classify a workspace-relative path as restricted (secret-looking or a local
/// data-plane path), or `None` when it may be staged.
fn restricted_reason(path: &str) -> Option<GitStageRejectReason> {
    if looks_secret_path(path) {
        return Some(GitStageRejectReason::SecretRestricted);
    }
    if is_data_plane_path(path) {
        return Some(GitStageRejectReason::DataPlane);
    }
    None
}

/// Secret-looking-name policy, aligned with `opensks-file-service` and
/// `opensks-git`. A name containing any of these tokens is never staged.
fn looks_secret_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains(".env")
        || lower.contains("secret")
        || lower.contains("credential")
        || lower.contains("id_rsa")
        || lower.contains(".pem")
}

/// True when `path` falls under a local (ignored / never-tracked) rule in the
/// default data-plane manifest. Shared (tracked) rules do not restrict staging.
fn is_data_plane_path(path: &str) -> bool {
    let normalized = path.trim_start_matches("./");
    opensks_policy::default_data_plane_manifest()
        .local_paths
        .iter()
        .any(|rule| data_plane_rule_matches(&rule.path, normalized))
}

/// Match a manifest rule path against a candidate. A trailing `/` denotes a
/// directory prefix; a `*` segment matches one path component; otherwise the
/// match is exact or a directory-prefix match.
fn data_plane_rule_matches(rule: &str, candidate: &str) -> bool {
    if let Some(prefix) = rule.strip_suffix('/') {
        // Directory rule: the candidate is inside this directory.
        return candidate == prefix
            || candidate.starts_with(&format!("{prefix}/"))
            || candidate.starts_with(prefix);
    }
    if rule.contains('*') {
        return glob_segments_match(rule, candidate);
    }
    candidate == rule || candidate.starts_with(&format!("{rule}/"))
}

/// Component-wise glob match where each `*` segment matches exactly one path
/// component. Used for manifest rules like `.opensks/history/runs/*/proof.json`.
fn glob_segments_match(rule: &str, candidate: &str) -> bool {
    let rule_parts: Vec<&str> = rule.split('/').collect();
    let cand_parts: Vec<&str> = candidate.split('/').collect();
    if rule_parts.len() != cand_parts.len() {
        return false;
    }
    rule_parts
        .iter()
        .zip(cand_parts.iter())
        .all(|(r, c)| *r == "*" || r == c)
}

// --- index state ------------------------------------------------------------

/// Compute the stable index hash and the sorted staged path list.
///
/// The hash is an FNV-1a 64 digest over each staged entry's `dst_oid` and path,
/// taken from `git diff --cached --raw -z` (which compares the index to HEAD).
/// Entries are sorted by path first so the hash is independent of git's output
/// order. Any added/removed path or any change to a staged blob's oid flips the
/// digest.
fn index_state(workspace: &Path) -> Result<(String, Vec<String>), GitServiceError> {
    let raw = git(
        workspace,
        &["diff", "--cached", "--raw", "--no-color", "-z"],
    )?;
    let mut entries = parse_raw_cached(&raw)?;
    entries.sort_by(|a, b| a.1.cmp(&b.1));
    let mut hash: u64 = 0xcbf29ce484222325;
    for (oid, path) in &entries {
        for byte in oid.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0x1f; // unit separator between oid and path
        hash = hash.wrapping_mul(0x100000001b3);
        for byte in path.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0x1e; // record separator between entries
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let paths: Vec<String> = entries.into_iter().map(|(_, path)| path).collect();
    Ok((format!("fnv1a64:{hash:016x}"), paths))
}

/// Parse `git diff --cached --raw -z` records into `(dst_oid, path)` pairs.
///
/// Each record is `:<srcMode> <dstMode> <srcOid> <dstOid> <status>\0<path>`
/// (renames/copies carry a second NUL-separated path). The metadata line and
/// the path(s) are separate NUL fields, so we alternate: a field beginning with
/// `:` is metadata, and the following NUL field(s) are its path(s).
fn parse_raw_cached(raw: &str) -> Result<Vec<(String, String)>, GitServiceError> {
    let mut entries = Vec::new();
    let mut fields = raw.split('\0');
    while let Some(meta) = fields.next() {
        if meta.is_empty() {
            continue;
        }
        if !meta.starts_with(':') {
            // Defensive: a stray path field with no preceding metadata.
            continue;
        }
        let parts: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        // parts: [srcMode, dstMode, srcOid, dstOid, status]
        let dst_oid = parts
            .get(3)
            .ok_or_else(|| GitServiceError::Parse(format!("raw cached missing dst oid: {meta:?}")))?
            .to_string();
        let status = parts.get(4).copied().unwrap_or("");
        let dst_path = fields
            .next()
            .ok_or_else(|| GitServiceError::Parse(format!("raw cached missing path: {meta:?}")))?;
        // Renames/copies (R/C) carry a destination path in a second field.
        let path = if status.starts_with('R') || status.starts_with('C') {
            fields
                .next()
                .ok_or_else(|| {
                    GitServiceError::Parse(format!("raw cached missing rename dest: {meta:?}"))
                })?
                .to_string()
        } else {
            dst_path.to_string()
        };
        entries.push((dst_oid, path));
    }
    Ok(entries)
}

// --- shell helpers ----------------------------------------------------------

/// `git rev-parse <rev>` → the resolved object name (trimmed).
fn rev_parse(workspace: &Path, rev: &str) -> Result<String, GitServiceError> {
    let out = git(workspace, &["rev-parse", rev])?;
    Ok(out.trim().to_string())
}

/// Run a **local** git command in `workspace`. Every call site in this module
/// passes a local index/branch/commit verb or a read-only inspection verb; no
/// `push`/`fetch`/`pull`/`remote` verb is ever assembled here.
fn git(workspace: &Path, args: &[&str]) -> Result<String, GitServiceError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()?;
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
    use opensks_contracts::{GitMutationErrorCode, GitStageRejectReason};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A throwaway temp repo, unique per process+call so parallel tests never
    /// collide and we never touch the real opensks repo. EVERY mutation test
    /// operates only on a fresh repo created here under `std::env::temp_dir()`.
    fn init_repo(name: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "opensks-git-mutation-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        let dir = dir.canonicalize().expect("canonicalize temp dir");
        run(&dir, &["init"]);
        run(&dir, &["config", "user.email", "opensks@example.test"]);
        run(&dir, &["config", "user.name", "OpenSKS Test"]);
        run(&dir, &["config", "commit.gpgsign", "false"]);
        run(&dir, &["checkout", "-B", "main"]);
        dir
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

    fn write(dir: &Path, path: &str, contents: &str) {
        let full = dir.join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(full, contents).expect("write");
    }

    fn commit_file(dir: &Path, path: &str, contents: &str, message: &str) {
        write(dir, path, contents);
        run(dir, &["add", path]);
        run(dir, &["commit", "-m", message]);
    }

    fn staged_paths(dir: &Path) -> Vec<String> {
        let raw = git(dir, &["diff", "--cached", "--name-only", "-z"]).expect("name-only");
        raw.split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    // --- switch preflight / switch ------------------------------------------

    #[test]
    fn dirty_worktree_blocks_switch_and_force_clean_switches() {
        let dir = init_repo("switch");
        commit_file(&dir, "file.txt", "v1\n", "init");
        run(&dir, &["branch", "feature"]);
        // Make the worktree dirty (tracked change).
        write(&dir, "file.txt", "v2\n");

        // Preflight reports a dirty_worktree blocker.
        let preflight = switch_preflight(&dir).expect("preflight");
        assert!(!preflight.can_switch);
        assert!(
            preflight
                .blockers
                .iter()
                .any(|b| b.kind == GitSwitchBlockerKind::DirtyWorktree
                    && b.paths.iter().any(|p| p == "file.txt")),
            "expected dirty_worktree blocker: {preflight:?}"
        );

        // Switch without --force is refused with switch_blocked.
        let blocked = switch(&dir, "feature", false).expect("switch result");
        let error = blocked.expect_err("switch should be blocked");
        assert_eq!(error.error.code, GitMutationErrorCode::SwitchBlocked);
        assert!(!error.error.blockers.is_empty());
        // We are still on main (the switch did not happen).
        let head = git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).expect("head");
        assert_eq!(head.trim(), "main");

        // On a clean worktree, a forced switch succeeds.
        run(&dir, &["checkout", "--", "file.txt"]); // discard dirty change
        let preflight_clean = switch_preflight(&dir).expect("preflight clean");
        assert!(preflight_clean.can_switch, "clean repo can switch");
        let ok = switch(&dir, "feature", true)
            .expect("switch result")
            .expect("forced switch should succeed");
        assert!(ok.switched);
        assert_eq!(ok.branch, "feature");
        let head = git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).expect("head");
        assert_eq!(head.trim(), "feature");
    }

    // --- staging rejects secret + data-plane paths --------------------------

    #[test]
    fn secret_and_data_plane_paths_are_rejected_normal_path_stages() {
        let dir = init_repo("stage-reject");
        commit_file(&dir, "seed.txt", "seed\n", "init");
        // Create three worktree files: a secret (by name), a data-plane file
        // (an ephemeral-local manifest path whose name is NOT secret-looking),
        // and a normal one.
        write(&dir, "id_rsa", "PRIVATE KEY\n");
        write(&dir, ".opensks/cache/blob.bin", "cached\n");
        write(&dir, "normal.rs", "fn main() {}\n");

        let result = stage(
            &dir,
            &[
                "id_rsa".to_string(),
                ".opensks/cache/blob.bin".to_string(),
                "normal.rs".to_string(),
            ],
        )
        .expect("stage");

        // Only the normal path is staged.
        assert_eq!(result.staged, vec!["normal.rs".to_string()]);
        // Both restricted paths are rejected with the right reason.
        let secret = result
            .rejected
            .iter()
            .find(|r| r.path == "id_rsa")
            .expect("id_rsa rejected");
        assert_eq!(secret.reason, GitStageRejectReason::SecretRestricted);
        let data_plane = result
            .rejected
            .iter()
            .find(|r| r.path == ".opensks/cache/blob.bin")
            .expect("data-plane rejected");
        assert_eq!(data_plane.reason, GitStageRejectReason::DataPlane);

        // The index contains ONLY the normal path afterward.
        let staged = staged_paths(&dir);
        assert_eq!(staged, vec!["normal.rs".to_string()]);
        assert!(!staged.iter().any(|p| p == "id_rsa"));
        assert!(!staged.iter().any(|p| p == ".opensks/cache/blob.bin"));
    }

    #[test]
    fn dotenv_secret_path_is_rejected() {
        let dir = init_repo("stage-dotenv");
        commit_file(&dir, "seed.txt", "seed\n", "init");
        write(&dir, ".env", "API_KEY=abc\n");
        let result = stage(&dir, &[".env".to_string()]).expect("stage");
        assert!(result.staged.is_empty());
        assert_eq!(
            result.rejected.first().map(|r| r.reason),
            Some(GitStageRejectReason::SecretRestricted)
        );
        assert!(staged_paths(&dir).is_empty());
    }

    // --- commit preview / staleness / commit --------------------------------

    #[test]
    fn commit_preview_staleness_and_commit_includes_only_reviewed_paths() {
        let dir = init_repo("commit");
        commit_file(&dir, "seed.txt", "seed\n", "init");

        // Stage one file, take a preview.
        write(&dir, "a.rs", "fn a() {}\n");
        let staged = stage(&dir, &["a.rs".to_string()]).expect("stage a");
        assert_eq!(staged.staged, vec!["a.rs".to_string()]);
        let preview = commit_preview(&dir).expect("preview");
        assert!(preview.has_staged);
        assert_eq!(preview.staged_paths, vec!["a.rs".to_string()]);
        let old_hash = preview.index_hash.clone();

        // Staging another file changes the index hash.
        write(&dir, "b.rs", "fn b() {}\n");
        stage(&dir, &["b.rs".to_string()]).expect("stage b");
        let preview2 = commit_preview(&dir).expect("preview2");
        assert_ne!(
            preview2.index_hash, old_hash,
            "staging another file must change the index hash"
        );

        // Commit with the OLD (stale) hash is refused with index_changed.
        let stale = commit(&dir, "stale commit", &old_hash).expect("commit result");
        let error = stale.expect_err("stale commit should be refused");
        assert_eq!(error.error.code, GitMutationErrorCode::IndexChanged);

        // Commit with the CURRENT hash succeeds.
        let ok = commit(&dir, "good commit", &preview2.index_hash)
            .expect("commit result")
            .expect("fresh commit should succeed");
        assert!(ok.committed);
        assert!(!ok.commit.is_empty());
        let mut returned = ok.paths.clone();
        returned.sort();
        assert_eq!(returned, vec!["a.rs".to_string(), "b.rs".to_string()]);

        // The commit contains ONLY the staged/reviewed paths.
        let names = git(&dir, &["show", "--name-only", "--pretty=format:", "HEAD"])
            .expect("show")
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        let mut names_sorted = names.clone();
        names_sorted.sort();
        assert_eq!(
            names_sorted,
            vec!["a.rs".to_string(), "b.rs".to_string()],
            "commit must contain only the reviewed paths: {names:?}"
        );
    }

    #[test]
    fn commit_with_nothing_staged_is_refused() {
        let dir = init_repo("commit-empty");
        commit_file(&dir, "seed.txt", "seed\n", "init");
        let preview = commit_preview(&dir).expect("preview");
        assert!(!preview.has_staged);
        let result = commit(&dir, "empty", &preview.index_hash).expect("commit result");
        let error = result.expect_err("empty commit should be refused");
        assert_eq!(error.error.code, GitMutationErrorCode::NothingStaged);
    }

    #[test]
    fn commit_refuses_when_a_secret_path_is_staged_out_of_band() {
        let dir = init_repo("commit-secret");
        commit_file(&dir, "seed.txt", "seed\n", "init");
        // Stage a secret path directly with git (bypassing `stage`'s filter).
        write(&dir, "id_rsa", "PRIVATE\n");
        run(&dir, &["add", "id_rsa"]);
        let preview = commit_preview(&dir).expect("preview");
        let result = commit(&dir, "leak", &preview.index_hash).expect("commit result");
        let error = result.expect_err("secret commit should be refused");
        assert_eq!(error.error.code, GitMutationErrorCode::SecretRestricted);
        assert!(error.error.paths.iter().any(|p| p == "id_rsa"));
    }

    // --- create branch ------------------------------------------------------

    #[test]
    fn create_branch_is_visible_with_real_head() {
        let dir = init_repo("create-branch");
        commit_file(&dir, "seed.txt", "seed\n", "init");
        let created = create_branch(&dir, "feature", None).expect("create");
        assert!(created.created);
        assert_eq!(created.branch, "feature");
        // The head is a real 40-hex sha.
        assert_eq!(created.head.len(), 40);
        assert!(created.head.chars().all(|c| c.is_ascii_hexdigit()));
        // The branch is visible to git.
        let list = git(&dir, &["branch", "--list", "--format=%(refname:short)"]).expect("list");
        assert!(
            list.lines().any(|l| l.trim() == "feature"),
            "feature branch is visible: {list}"
        );
    }

    #[test]
    fn unstage_removes_path_from_index() {
        let dir = init_repo("unstage");
        commit_file(&dir, "seed.txt", "seed\n", "init");
        write(&dir, "a.rs", "fn a() {}\n");
        stage(&dir, &["a.rs".to_string()]).expect("stage");
        assert_eq!(staged_paths(&dir), vec!["a.rs".to_string()]);
        let result = unstage(&dir, &["a.rs".to_string()]).expect("unstage");
        assert_eq!(result.unstaged, vec!["a.rs".to_string()]);
        assert!(staged_paths(&dir).is_empty());
    }

    // --- local-only invariant (grep guard) ----------------------------------

    #[test]
    fn mutation_source_contains_no_remote_write_verbs() {
        // Guard: the mutation service must never assemble a remote-write git
        // invocation. We scan our own production source (above the test module).
        let source = include_str!("mutation.rs");
        let cutoff = source
            .find("#[cfg(test)]")
            .expect("test module marker present");
        let production = &source[..cutoff];
        for forbidden in ["\"push\"", "\"fetch\"", "\"pull\"", "\"remote\""] {
            assert!(
                !production.contains(forbidden),
                "local-only mutation service must not reference remote verb {forbidden}"
            );
        }
    }
}
