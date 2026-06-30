use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub use opensks_contracts::{OutboxApproval, OutboxDispatchReport};

pub mod push;
pub use push::{
    PUSH_OUTBOX_DB_RELATIVE_PATH, PUSH_OUTBOX_MIGRATION_VERSION, PushOutbox, is_protected_ref,
    redact_remote,
};

use opensks_contracts::{
    GIT_ISOLATION_SCHEMA, GateResult, GateStatus, GitIsolationReport, IsolationMode,
    OUTBOX_DISPATCH_REPORT_SCHEMA, OUTBOX_ITEM_SCHEMA, OutboxAction, OutboxItem,
    PATCH_ENVELOPE_SCHEMA, PatchEnvelope, ProducerRef, WORKTREE_ISOLATION_INVENTORY_RECEIPT_SCHEMA,
    WORKTREE_ISOLATION_RECOVERY_RECEIPT_SCHEMA, WorktreeIsolationInventoryEntry,
    WorktreeIsolationInventoryReceipt, WorktreeIsolationRecoveryReceipt,
    WorktreeIsolationRecoveryTarget,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("git command failed: {0}")]
    GitCommand(String),
    #[error("path escapes workspace: {0}")]
    PathEscape(String),
    #[error("target path has user dirty changes: {0}")]
    DirtyPath(String),
    #[error("before hash mismatch for {path}: expected {expected}, actual {actual}")]
    BeforeHashMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("patch verification failed and rollback was applied")]
    VerificationFailedRolledBack,
    #[error("git repository is required for unified diff apply")]
    GitRequired,
    #[error("secret-looking path cannot be staged: {0}")]
    SecretStageBlocked(String),
    #[error("protected branch requires approval: {0}")]
    ProtectedBranch(String),
    #[error("duplicate outbox idempotency key: {0}")]
    DuplicateOutboxWrite(String),
    #[error("outbox has no dispatchable item")]
    EmptyOutbox,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryInfo {
    pub root: PathBuf,
    pub head: String,
    pub submodule_detected: bool,
    pub lfs_detected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationCleanupResult {
    pub existed: bool,
    pub removed: bool,
    pub reason_code: String,
}

pub fn discover_repository(workspace: &Path) -> Option<RepositoryInfo> {
    let root_output = git(workspace, ["rev-parse", "--show-toplevel"]).ok()?;
    let root = PathBuf::from(root_output.trim());
    let head_output = git(&root, ["rev-parse", "HEAD"]).ok()?;
    let submodule_detected = root.join(".gitmodules").exists();
    let lfs_detected = root.join(".lfsconfig").exists()
        || git(&root, ["lfs", "env"])
            .map(|output| !output.trim().is_empty())
            .unwrap_or(false);
    Some(RepositoryInfo {
        root,
        head: head_output.trim().to_string(),
        submodule_detected,
        lfs_detected,
    })
}

pub fn create_isolation(
    workspace: &Path,
    run_id: &str,
    worker_id: &str,
) -> Result<GitIsolationReport, GitError> {
    let worktree_path = isolation_path(workspace, run_id, worker_id);
    if let Some(repo) = discover_repository(workspace) {
        if worktree_path.exists() {
            fs::remove_dir_all(&worktree_path)?;
        }
        if let Some(parent) = worktree_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let worktree_path_str = path_str(&worktree_path)?;
        let mut reason_code = "git_worktree_created";
        let add_result = run_git(
            &repo.root,
            ["worktree", "add", "--detach", worktree_path_str, &repo.head],
        );
        if let Err(error) = add_result {
            if is_stale_registered_worktree_error(&error) {
                run_git(&repo.root, ["worktree", "prune"])?;
                run_git(
                    &repo.root,
                    ["worktree", "add", "--detach", worktree_path_str, &repo.head],
                )?;
                reason_code = "git_worktree_created_after_prune";
            } else {
                return Err(error);
            }
        }
        return Ok(GitIsolationReport {
            schema: GIT_ISOLATION_SCHEMA.to_string(),
            id: format!("isolation-{run_id}-{worker_id}"),
            mode: IsolationMode::GitWorktree,
            repository_root: Some(repo.root.display().to_string()),
            base_commit: Some(repo.head),
            worktree_path: worktree_path.display().to_string(),
            git_available: true,
            reason_code: reason_code.to_string(),
            submodule_detected: repo.submodule_detected,
            lfs_detected: repo.lfs_detected,
        });
    }

    copy_snapshot(workspace, &worktree_path)?;
    Ok(GitIsolationReport {
        schema: GIT_ISOLATION_SCHEMA.to_string(),
        id: format!("isolation-{run_id}-{worker_id}"),
        mode: IsolationMode::Snapshot,
        repository_root: None,
        base_commit: None,
        worktree_path: worktree_path.display().to_string(),
        git_available: false,
        reason_code: "snapshot_isolation_for_non_git_workspace".to_string(),
        submodule_detected: false,
        lfs_detected: false,
    })
}

pub fn cleanup_isolation(
    workspace: &Path,
    run_id: &str,
    worker_id: &str,
) -> Result<IsolationCleanupResult, GitError> {
    let worktree_path = isolation_path(workspace, run_id, worker_id);
    if !worktree_path.exists() {
        return Ok(IsolationCleanupResult {
            existed: false,
            removed: false,
            reason_code: "source_isolation_already_absent".to_string(),
        });
    }
    if let Some(repo) = discover_repository(workspace) {
        if worktree_path.join(".git").exists() {
            run_git(
                &repo.root,
                ["worktree", "remove", "--force", path_str(&worktree_path)?],
            )?;
        } else {
            fs::remove_dir_all(&worktree_path)?;
        }
    } else {
        fs::remove_dir_all(&worktree_path)?;
    }
    Ok(IsolationCleanupResult {
        existed: true,
        removed: !worktree_path.exists(),
        reason_code: "source_isolation_removed".to_string(),
    })
}

pub fn inventory_isolations(
    workspace: &Path,
    run_id: &str,
    generated_at_ms: u64,
) -> Result<WorktreeIsolationInventoryReceipt, GitError> {
    let root = isolation_run_path(workspace, run_id);
    let mut isolations = Vec::new();
    if root.exists() {
        let mut entries = fs::read_dir(&root)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let worker_id = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            let has_git_metadata = path.join(".git").exists();
            let mode = if has_git_metadata {
                IsolationMode::GitWorktree
            } else {
                IsolationMode::Snapshot
            };
            isolations.push(WorktreeIsolationInventoryEntry {
                isolation_id: format!("isolation-{run_id}-{worker_id}"),
                run_id: run_id.to_string(),
                worker_id,
                mode,
                artifact_ref: isolation_artifact_ref(run_id, entry.file_name().to_string_lossy()),
                exists: true,
                has_git_metadata,
                path_redacted: true,
                content_redacted: true,
                reason_code: "runtime_isolation_present".to_string(),
            });
        }
    }
    let isolation_count = isolations.len();
    let (state, reason_code) = if isolation_count == 0 {
        ("empty", "no_runtime_isolations_found")
    } else {
        ("present", "runtime_isolations_discovered")
    };
    Ok(WorktreeIsolationInventoryReceipt {
        schema: WORKTREE_ISOLATION_INVENTORY_RECEIPT_SCHEMA.to_string(),
        id: format!("worktree-inventory-{run_id}"),
        run_id: run_id.to_string(),
        state: state.to_string(),
        reason_code: reason_code.to_string(),
        inventory_ref: format!("artifact://.opensks/runtime/worktrees/{run_id}/inventory.json"),
        isolations,
        isolation_count,
        git_available: discover_repository(workspace).is_some(),
        path_redacted: true,
        content_redacted: true,
        evidence_refs: vec!["git:worktree-inventory".to_string()],
        generated_at_ms,
    })
}

pub fn recover_isolations(
    workspace: &Path,
    run_id: &str,
    generated_at_ms: u64,
) -> Result<WorktreeIsolationRecoveryReceipt, GitError> {
    let inventory = inventory_isolations(workspace, run_id, generated_at_ms)?;
    let mut targets = Vec::new();
    for isolation in &inventory.isolations {
        let cleanup_result = cleanup_isolation(workspace, run_id, &isolation.worker_id);
        let (existed, removed, reason_code) = match cleanup_result {
            Ok(result) => (result.existed, result.removed, result.reason_code),
            Err(_) => (true, false, "source_isolation_recovery_failed".to_string()),
        };
        targets.push(WorktreeIsolationRecoveryTarget {
            isolation_id: isolation.isolation_id.clone(),
            run_id: run_id.to_string(),
            worker_id: isolation.worker_id.clone(),
            mode: isolation.mode.clone(),
            artifact_ref: isolation.artifact_ref.clone(),
            existed,
            removed,
            reason_code,
        });
    }
    let (prune_attempted, prune_succeeded) = if let Some(repo) = discover_repository(workspace) {
        (true, run_git(&repo.root, ["worktree", "prune"]).is_ok())
    } else {
        (false, false)
    };
    let target_count = targets.len();
    let recovered_count = targets
        .iter()
        .filter(|target| target.removed || !target.existed)
        .count();
    let (state, reason_code) = if target_count == 0 {
        ("empty", "no_runtime_isolations_to_recover")
    } else if recovered_count == target_count && (!prune_attempted || prune_succeeded) {
        ("recovered", "runtime_isolations_recovered")
    } else {
        ("partial", "runtime_isolation_recovery_partial")
    };
    Ok(WorktreeIsolationRecoveryReceipt {
        schema: WORKTREE_ISOLATION_RECOVERY_RECEIPT_SCHEMA.to_string(),
        id: format!("worktree-recovery-{run_id}"),
        run_id: run_id.to_string(),
        state: state.to_string(),
        reason_code: reason_code.to_string(),
        inventory_ref: inventory.inventory_ref,
        recovery_ref: format!("artifact://.opensks/runtime/worktrees/{run_id}/recovery.json"),
        targets,
        target_count,
        recovered_count,
        prune_attempted,
        prune_succeeded,
        path_redacted: true,
        content_redacted: true,
        evidence_refs: vec![
            "git:worktree-inventory".to_string(),
            "git:worktree-recovery".to_string(),
        ],
        generated_at_ms,
    })
}

pub fn new_patch_envelope(
    id: impl Into<String>,
    work_item_id: impl Into<String>,
    lease_id: impl Into<String>,
    target_paths: Vec<String>,
) -> PatchEnvelope {
    PatchEnvelope {
        schema: PATCH_ENVELOPE_SCHEMA.to_string(),
        id: id.into(),
        work_item_id: work_item_id.into(),
        lease_id: lease_id.into(),
        base_commit: None,
        target_paths,
        before_hashes: BTreeMap::new(),
        after_hashes: BTreeMap::new(),
        unified_diff_ref: "artifact://pending-unified-diff".to_string(),
        rollback_ref: "artifact://pending-rollback".to_string(),
        requirement_ids: Vec::new(),
        producer: ProducerRef {
            kind: "opensks-worker".to_string(),
            id: "local".to_string(),
        },
        secret_scan: GateResult {
            status: GateStatus::Pending,
            reason_code: "secret_scan_not_yet_run".to_string(),
            evidence_refs: Vec::new(),
            secret_value_exposed: false,
        },
    }
}

fn isolation_path(workspace: &Path, run_id: &str, worker_id: &str) -> PathBuf {
    isolation_run_path(workspace, run_id).join(worker_id)
}

fn isolation_run_path(workspace: &Path, run_id: &str) -> PathBuf {
    workspace
        .join(".opensks")
        .join("runtime")
        .join("worktrees")
        .join(run_id)
}

fn isolation_artifact_ref(run_id: &str, worker_id: impl AsRef<str>) -> String {
    format!(
        "artifact://.opensks/runtime/worktrees/{}/{}",
        run_id,
        worker_id.as_ref()
    )
}

pub fn check_patch_envelope(workspace: &Path, envelope: &PatchEnvelope) -> Result<(), GitError> {
    for target in &envelope.target_paths {
        let path = resolve_repo_path(workspace, target)?;
        if is_dirty_path(workspace, target)? {
            return Err(GitError::DirtyPath(target.clone()));
        }
        if let Some(expected) = envelope.before_hashes.get(target) {
            let actual = if path.exists() {
                stable_hash(&fs::read(&path)?)
            } else {
                "missing".to_string()
            };
            if &actual != expected {
                return Err(GitError::BeforeHashMismatch {
                    path: target.clone(),
                    expected: expected.clone(),
                    actual,
                });
            }
        }
    }
    Ok(())
}

pub fn check_unified_diff_apply(
    workspace: &Path,
    envelope: &PatchEnvelope,
    unified_diff: &str,
) -> Result<(), GitError> {
    if discover_repository(workspace).is_none() {
        return Err(GitError::GitRequired);
    }
    check_patch_envelope(workspace, envelope)?;
    run_git_with_stdin(workspace, ["apply", "--check"], unified_diff)
}

pub fn check_unified_diff_reverse_apply(
    workspace: &Path,
    unified_diff: &str,
) -> Result<(), GitError> {
    if discover_repository(workspace).is_none() {
        return Err(GitError::GitRequired);
    }
    run_git_with_stdin(workspace, ["apply", "--reverse", "--check"], unified_diff)
}

pub fn apply_unified_diff_with_rollback<F>(
    workspace: &Path,
    envelope: &PatchEnvelope,
    unified_diff: &str,
    verifier: F,
) -> Result<(), GitError>
where
    F: FnOnce() -> bool,
{
    if discover_repository(workspace).is_none() {
        return Err(GitError::GitRequired);
    }
    check_patch_envelope(workspace, envelope)?;
    let rollback = capture_rollback(workspace, &envelope.target_paths)?;
    run_git_with_stdin(workspace, ["apply", "--check"], unified_diff)?;
    run_git_with_stdin(workspace, ["apply"], unified_diff)?;
    if verifier() {
        return Ok(());
    }
    restore_rollback(workspace, rollback)?;
    Err(GitError::VerificationFailedRolledBack)
}

pub fn working_tree_diff(workspace: &Path, target_paths: &[String]) -> Result<String, GitError> {
    if discover_repository(workspace).is_none() {
        return Err(GitError::GitRequired);
    }
    for target in target_paths {
        let _ = resolve_repo_path(workspace, target)?;
    }
    let output = Command::new("git")
        .arg("diff")
        .arg("--")
        .args(target_paths)
        .current_dir(workspace)
        .output()?;
    if !output.status.success() {
        return Err(GitError::GitCommand(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    let mut diff = String::from_utf8_lossy(&output.stdout).to_string();
    for target in target_paths {
        let path = resolve_repo_path(workspace, target)?;
        if path.is_file() && !is_tracked_path(workspace, target)? {
            let untracked_diff = untracked_file_diff(workspace, target)?;
            if !untracked_diff.trim().is_empty() {
                if !diff.ends_with('\n') && !diff.is_empty() {
                    diff.push('\n');
                }
                diff.push_str(&untracked_diff);
                if !diff.ends_with('\n') {
                    diff.push('\n');
                }
            }
        }
    }
    Ok(diff)
}

pub fn target_paths_changed_since_base(
    workspace: &Path,
    base_commit: &str,
    target_paths: &[String],
) -> Result<bool, GitError> {
    if target_paths.is_empty() {
        return Ok(false);
    }
    if discover_repository(workspace).is_none() {
        return Err(GitError::GitRequired);
    }
    for target in target_paths {
        let _ = resolve_repo_path(workspace, target)?;
    }
    let output = Command::new("git")
        .arg("diff")
        .arg("--quiet")
        .arg("--exit-code")
        .arg(base_commit)
        .arg("HEAD")
        .arg("--")
        .args(target_paths)
        .current_dir(workspace)
        .output()?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(GitError::GitCommand(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        )),
    }
}

pub fn content_hash(path: &Path) -> Result<String, GitError> {
    Ok(stable_hash(&fs::read(path)?))
}

/// The git blob object id of `relative` at HEAD, or `None` when the path is not
/// tracked at HEAD (newly added, ignored, or deleted there). Used to label a
/// branch-switch working-tree conflict in the editor UI.
///
/// `relative` is workspace-relative; it is passed to git via `HEAD:<path>` which
/// git resolves against the repository root, so callers do not need to rebase
/// the path themselves.
pub fn head_blob_hash(workspace: &Path, relative: &str) -> Result<Option<String>, GitError> {
    if discover_repository(workspace).is_none() {
        return Ok(None);
    }
    let spec = format!("HEAD:{relative}");
    match git(workspace, ["rev-parse", "--verify", "--quiet", &spec]) {
        Ok(output) => {
            let trimmed = output.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        // `rev-parse --verify --quiet` exits nonzero (empty stderr) when the
        // object does not exist at HEAD; treat that as "not tracked", not error.
        Err(GitError::GitCommand(message)) if message.is_empty() => Ok(None),
        Err(error) => Err(error),
    }
}

#[derive(Debug, Clone, Default)]
pub struct Outbox {
    items: Vec<OutboxItem>,
}

impl Outbox {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn enqueue_commit(
        &mut self,
        id: impl Into<String>,
        target_paths: &[String],
    ) -> Result<OutboxItem, GitError> {
        for path in target_paths {
            if looks_secret_path(path) {
                return Err(GitError::SecretStageBlocked(path.clone()));
            }
        }
        let id = id.into();
        let item = OutboxItem {
            schema: OUTBOX_ITEM_SCHEMA.to_string(),
            id: id.clone(),
            action: OutboxAction::Commit,
            target: target_paths.join(","),
            approval_required: false,
            approval_id: None,
            protected_branch: false,
            idempotency_key: format!("commit:{id}:{}", target_paths.join("|")),
            state: "queued".to_string(),
            attempt_count: 0,
            last_reason_code: None,
            evidence_refs: vec!["opensks-git:outbox-commit".to_string()],
        };
        self.push_unique(item)
    }

    pub fn enqueue_push(
        &mut self,
        branch: impl Into<String>,
        approved: bool,
    ) -> Result<OutboxItem, GitError> {
        let branch = branch.into();
        let protected = matches!(branch.as_str(), "main" | "master" | "trunk");
        let approval_id = format!("approval-push-{branch}");
        let item = OutboxItem {
            schema: OUTBOX_ITEM_SCHEMA.to_string(),
            id: format!("push-{branch}"),
            action: OutboxAction::Push,
            target: branch.clone(),
            approval_required: true,
            approval_id: Some(approval_id),
            protected_branch: protected,
            idempotency_key: format!("push:{branch}"),
            state: if approved {
                "queued"
            } else {
                "awaiting_approval"
            }
            .to_string(),
            attempt_count: 0,
            last_reason_code: (!approved).then(|| "approval_required".to_string()),
            evidence_refs: vec![
                "opensks-git:outbox-push".to_string(),
                "opensks-git:approval-required".to_string(),
            ],
        };
        self.push_unique(item)
    }

    pub fn items(&self) -> &[OutboxItem] {
        &self.items
    }

    pub fn dispatch_next<F>(
        &mut self,
        approvals: &[OutboxApproval],
        mut execute: F,
    ) -> Result<OutboxDispatchReport, GitError>
    where
        F: FnMut(&OutboxItem) -> Result<(), GitError>,
    {
        let item = self
            .items
            .iter_mut()
            .find(|item| item.state != "executed")
            .ok_or(GitError::EmptyOutbox)?;

        if item.state == "executed" {
            return Ok(dispatch_report(
                item,
                false,
                "executed".to_string(),
                "duplicate_remote_write_blocked",
                "opensks-git:duplicate-write-blocked",
            ));
        }

        if item.approval_required && !approval_matches(item, approvals) {
            item.state = "awaiting_approval".to_string();
            item.last_reason_code = Some("approval_required".to_string());
            item.evidence_refs
                .push("opensks-git:dispatch-blocked-without-approval".to_string());
            return Ok(dispatch_report(
                item,
                false,
                "awaiting_approval".to_string(),
                "approval_required",
                "opensks-git:dispatch-blocked-without-approval",
            ));
        }

        item.attempt_count += 1;
        match execute(item) {
            Ok(()) => {
                item.state = "executed".to_string();
                item.last_reason_code = Some("executed_after_approval".to_string());
                item.evidence_refs
                    .push("opensks-git:dispatch-executed-after-approval".to_string());
                Ok(dispatch_report(
                    item,
                    true,
                    "executed".to_string(),
                    "executed_after_approval",
                    "opensks-git:dispatch-executed-after-approval",
                ))
            }
            Err(error) => {
                item.state = "retry_wait".to_string();
                item.last_reason_code = Some("push_failed".to_string());
                item.evidence_refs
                    .push("opensks-git:dispatch-retry-wait".to_string());
                let mut report = dispatch_report(
                    item,
                    false,
                    "retry_wait".to_string(),
                    "push_failed",
                    "opensks-git:dispatch-retry-wait",
                );
                report
                    .evidence_refs
                    .push(format!("opensks-git:dispatch-error:{error}"));
                Ok(report)
            }
        }
    }

    fn push_unique(&mut self, item: OutboxItem) -> Result<OutboxItem, GitError> {
        if self
            .items
            .iter()
            .any(|existing| existing.idempotency_key == item.idempotency_key)
        {
            return Err(GitError::DuplicateOutboxWrite(item.idempotency_key));
        }
        self.items.push(item.clone());
        Ok(item)
    }
}

fn approval_matches(item: &OutboxItem, approvals: &[OutboxApproval]) -> bool {
    approvals.iter().any(|approval| {
        approval.approved
            && approval.scope == action_scope(&item.action)
            && approval.target == item.target
            && item
                .approval_id
                .as_ref()
                .map(|id| id == &approval.approval_id)
                .unwrap_or(true)
    })
}

fn action_scope(action: &OutboxAction) -> &'static str {
    match action {
        OutboxAction::Commit => "git_commit",
        OutboxAction::Push => "git_push",
        OutboxAction::PullRequest => "git_pull_request",
        OutboxAction::ExternalSend => "external_send",
    }
}

fn dispatch_report(
    item: &OutboxItem,
    executed: bool,
    state: String,
    reason_code: &str,
    evidence_ref: &str,
) -> OutboxDispatchReport {
    let mut evidence_refs = item.evidence_refs.clone();
    if !evidence_refs
        .iter()
        .any(|existing| existing == evidence_ref)
    {
        evidence_refs.push(evidence_ref.to_string());
    }
    OutboxDispatchReport {
        schema: OUTBOX_DISPATCH_REPORT_SCHEMA.to_string(),
        item_id: item.id.clone(),
        action: item.action.clone(),
        target: item.target.clone(),
        approval_id: item.approval_id.clone(),
        executed,
        state,
        reason_code: reason_code.to_string(),
        attempt_count: item.attempt_count,
        evidence_refs,
    }
}

fn capture_rollback(
    workspace: &Path,
    target_paths: &[String],
) -> Result<BTreeMap<String, Option<Vec<u8>>>, GitError> {
    let mut rollback = BTreeMap::new();
    for target in target_paths {
        let path = resolve_repo_path(workspace, target)?;
        let bytes = if path.exists() {
            Some(fs::read(path)?)
        } else {
            None
        };
        rollback.insert(target.clone(), bytes);
    }
    Ok(rollback)
}

fn restore_rollback(
    workspace: &Path,
    rollback: BTreeMap<String, Option<Vec<u8>>>,
) -> Result<(), GitError> {
    for (target, bytes) in rollback {
        let path = resolve_repo_path(workspace, &target)?;
        match bytes {
            Some(bytes) => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(path, bytes)?;
            }
            None if path.exists() => fs::remove_file(path)?,
            None => {}
        }
    }
    Ok(())
}

fn resolve_repo_path(workspace: &Path, target: &str) -> Result<PathBuf, GitError> {
    let rel = Path::new(target);
    if rel.is_absolute() || target.split('/').any(|part| part == "..") {
        return Err(GitError::PathEscape(target.to_string()));
    }
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let path = workspace.join(rel);
    let parent = path.parent().unwrap_or(&workspace);
    let canonical_parent = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    if !canonical_parent.starts_with(&workspace) {
        return Err(GitError::PathEscape(target.to_string()));
    }
    Ok(path)
}

fn is_dirty_path(workspace: &Path, target: &str) -> Result<bool, GitError> {
    if discover_repository(workspace).is_none() {
        return Ok(false);
    }
    let output = git(workspace, ["status", "--porcelain", "--", target])?;
    Ok(!output.trim().is_empty())
}

fn is_tracked_path(workspace: &Path, target: &str) -> Result<bool, GitError> {
    let output = Command::new("git")
        .arg("ls-files")
        .arg("--error-unmatch")
        .arg("--")
        .arg(target)
        .current_dir(workspace)
        .output()?;
    Ok(output.status.success())
}

fn untracked_file_diff(workspace: &Path, target: &str) -> Result<String, GitError> {
    let output = Command::new("git")
        .arg("diff")
        .arg("--no-index")
        .arg("--")
        .arg("/dev/null")
        .arg(target)
        .current_dir(workspace)
        .output()?;
    if output.status.success() || output.status.code() == Some(1) {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    Err(GitError::GitCommand(
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    ))
}

fn copy_snapshot(source: &Path, target: &Path) -> Result<(), GitError> {
    if target.exists() {
        fs::remove_dir_all(target)?;
    }
    fs::create_dir_all(target)?;
    copy_dir(source, target, source)
}

fn copy_dir(source: &Path, target: &Path, root: &Path) -> Result<(), GitError> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        if file_name == ".git" {
            continue;
        }
        if path.starts_with(root.join(".opensks").join("runtime")) {
            continue;
        }
        let dest = target.join(file_name);
        if path.is_dir() {
            fs::create_dir_all(&dest)?;
            copy_dir(&path, &dest, root)?;
        } else if path.is_file() {
            fs::copy(&path, &dest)?;
        }
    }
    Ok(())
}

fn git<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<String, GitError> {
    let output = Command::new("git").args(args).current_dir(cwd).output()?;
    if !output.status.success() {
        return Err(GitError::GitCommand(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<(), GitError> {
    git(cwd, args).map(|_| ())
}

fn is_stale_registered_worktree_error(error: &GitError) -> bool {
    let GitError::GitCommand(message) = error else {
        return false;
    };
    message.contains("is a missing but already registered worktree")
}

fn run_git_with_stdin<const N: usize>(
    cwd: &Path,
    args: [&str; N],
    stdin_text: &str,
) -> Result<(), GitError> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .as_mut()
        .expect("git stdin")
        .write_all(stdin_text.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(GitError::GitCommand(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(())
}

fn path_str(path: &Path) -> Result<&str, GitError> {
    path.to_str()
        .ok_or_else(|| GitError::PathEscape(path.display().to_string()))
}

fn looks_secret_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains(".env")
        || lower.contains("secret")
        || lower.contains("credential")
        || lower.contains("id_rsa")
        || lower.contains(".pem")
}

fn stable_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("opensks-git-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn init_repo(name: &str) -> PathBuf {
        let dir = temp_dir(name);
        run_git(&dir, ["init"]).expect("git init");
        run_git(&dir, ["config", "user.email", "opensks@example.test"]).expect("git email");
        run_git(&dir, ["config", "user.name", "OpenSKS Test"]).expect("git user");
        fs::write(dir.join("file.txt"), "before\n").expect("write file");
        run_git(&dir, ["add", "file.txt"]).expect("git add");
        run_git(&dir, ["commit", "-m", "initial"]).expect("git commit");
        dir
    }

    #[test]
    fn creates_actual_git_worktree_for_repo_fixture() {
        let repo = init_repo("worktree");
        let report = create_isolation(&repo, "run1", "worker1").expect("isolation");
        assert_eq!(report.mode, IsolationMode::GitWorktree);
        assert!(Path::new(&report.worktree_path).join("file.txt").exists());
    }

    #[test]
    fn falls_back_to_snapshot_for_non_git_workspace() {
        let root = temp_dir("snapshot");
        fs::write(root.join("plain.txt"), "hello").expect("write file");
        let report = create_isolation(&root, "run1", "worker1").expect("isolation");
        assert_eq!(report.mode, IsolationMode::Snapshot);
        assert!(Path::new(&report.worktree_path).join("plain.txt").exists());
    }

    #[test]
    fn cleanup_isolation_removes_snapshot_workspace() {
        let root = temp_dir("snapshot-cleanup");
        fs::write(root.join("plain.txt"), "hello").expect("write file");
        let report = create_isolation(&root, "run1", "worker1").expect("isolation");
        assert!(Path::new(&report.worktree_path).exists());

        let cleanup = cleanup_isolation(&root, "run1", "worker1").expect("cleanup");

        assert!(cleanup.existed);
        assert!(cleanup.removed);
        assert_eq!(cleanup.reason_code, "source_isolation_removed");
        assert!(!Path::new(&report.worktree_path).exists());
    }

    #[test]
    fn cleanup_isolation_removes_git_worktree() {
        let repo = init_repo("worktree-cleanup");
        let report = create_isolation(&repo, "run1", "worker1").expect("isolation");
        assert_eq!(report.mode, IsolationMode::GitWorktree);
        assert!(Path::new(&report.worktree_path).join(".git").exists());

        let cleanup = cleanup_isolation(&repo, "run1", "worker1").expect("cleanup");

        assert!(cleanup.existed);
        assert!(cleanup.removed);
        assert_eq!(cleanup.reason_code, "source_isolation_removed");
        assert!(!Path::new(&report.worktree_path).exists());
        let worktree_list = git(&repo, ["worktree", "list"]).expect("worktree list");
        assert!(!worktree_list.contains("worker1"));
    }

    #[test]
    fn create_isolation_prunes_missing_registered_worktree_and_retries() {
        let repo = init_repo("worktree-stale-registration");
        let first = create_isolation(&repo, "run1", "worker1").expect("first isolation");
        let stale_path = PathBuf::from(&first.worktree_path);
        fs::remove_dir_all(&stale_path).expect("simulate externally removed worktree path");
        let stale_list = git(&repo, ["worktree", "list", "--porcelain"]).expect("stale list");
        assert!(
            stale_list.contains(stale_path.to_string_lossy().as_ref()),
            "git metadata should still reference the missing worktree before prune"
        );

        let recovered = create_isolation(&repo, "run1", "worker1").expect("recovered isolation");

        assert_eq!(recovered.mode, IsolationMode::GitWorktree);
        assert_eq!(recovered.reason_code, "git_worktree_created_after_prune");
        assert!(Path::new(&recovered.worktree_path).join(".git").exists());
        let recovered_list =
            git(&repo, ["worktree", "list", "--porcelain"]).expect("recovered list");
        assert!(recovered_list.contains(&recovered.worktree_path));
    }

    #[test]
    fn cleanup_isolation_reports_already_absent() {
        let root = temp_dir("cleanup-absent");

        let cleanup = cleanup_isolation(&root, "run1", "worker1").expect("cleanup absent");

        assert!(!cleanup.existed);
        assert!(!cleanup.removed);
        assert_eq!(cleanup.reason_code, "source_isolation_already_absent");
    }

    #[test]
    fn inventory_isolations_reports_git_worktrees_without_absolute_paths() {
        let repo = init_repo("worktree-inventory");
        let report = create_isolation(&repo, "run1", "worker1").expect("isolation");

        let inventory = inventory_isolations(&repo, "run1", 1_000).expect("inventory");
        let json = serde_json::to_string(&inventory).expect("inventory json");

        assert_eq!(
            inventory.schema,
            opensks_contracts::WORKTREE_ISOLATION_INVENTORY_RECEIPT_SCHEMA
        );
        assert_eq!(inventory.state, "present");
        assert_eq!(inventory.isolation_count, 1);
        assert!(inventory.git_available);
        assert_eq!(inventory.isolations[0].worker_id, "worker1");
        assert_eq!(inventory.isolations[0].mode, IsolationMode::GitWorktree);
        assert!(inventory.isolations[0].has_git_metadata);
        assert_eq!(
            inventory.isolations[0].artifact_ref,
            "artifact://.opensks/runtime/worktrees/run1/worker1"
        );
        assert!(!json.contains(repo.to_string_lossy().as_ref()));
        assert!(!json.contains(&report.worktree_path));
    }

    #[test]
    fn inventory_isolations_reports_snapshot_workspaces() {
        let root = temp_dir("snapshot-inventory");
        fs::write(root.join("plain.txt"), "hello").expect("write file");
        create_isolation(&root, "run1", "worker1").expect("isolation");

        let inventory = inventory_isolations(&root, "run1", 1_000).expect("inventory");

        assert_eq!(inventory.state, "present");
        assert_eq!(inventory.isolation_count, 1);
        assert!(!inventory.git_available);
        assert_eq!(inventory.isolations[0].mode, IsolationMode::Snapshot);
        assert!(!inventory.isolations[0].has_git_metadata);
        assert!(inventory.path_redacted);
        assert!(inventory.content_redacted);
    }

    #[test]
    fn recover_isolations_removes_git_worktrees_and_prunes_metadata() {
        let repo = init_repo("worktree-recovery");
        let first = create_isolation(&repo, "run1", "worker1").expect("first isolation");
        let second = create_isolation(&repo, "run1", "worker2").expect("second isolation");

        let recovery = recover_isolations(&repo, "run1", 1_000).expect("recovery");

        assert_eq!(
            recovery.schema,
            opensks_contracts::WORKTREE_ISOLATION_RECOVERY_RECEIPT_SCHEMA
        );
        assert_eq!(recovery.state, "recovered");
        assert_eq!(recovery.target_count, 2);
        assert_eq!(recovery.recovered_count, 2);
        assert!(recovery.prune_attempted);
        assert!(recovery.prune_succeeded);
        assert!(!Path::new(&first.worktree_path).exists());
        assert!(!Path::new(&second.worktree_path).exists());
        let worktree_list = git(&repo, ["worktree", "list"]).expect("worktree list");
        assert!(!worktree_list.contains("worker1"));
        assert!(!worktree_list.contains("worker2"));
        let json = serde_json::to_string(&recovery).expect("recovery json");
        assert!(!json.contains(repo.to_string_lossy().as_ref()));
    }

    #[test]
    fn recover_isolations_removes_snapshot_workspaces() {
        let root = temp_dir("snapshot-recovery");
        fs::write(root.join("plain.txt"), "hello").expect("write file");
        let report = create_isolation(&root, "run1", "worker1").expect("isolation");

        let recovery = recover_isolations(&root, "run1", 1_000).expect("recovery");

        assert_eq!(recovery.state, "recovered");
        assert_eq!(recovery.target_count, 1);
        assert_eq!(recovery.recovered_count, 1);
        assert!(!recovery.prune_attempted);
        assert!(!Path::new(&report.worktree_path).exists());
        assert!(recovery.path_redacted);
        assert!(recovery.content_redacted);
    }

    #[test]
    fn dirty_path_guard_preserves_user_change() {
        let repo = init_repo("dirty");
        fs::write(repo.join("file.txt"), "user dirty\n").expect("dirty file");
        let mut envelope =
            new_patch_envelope("patch1", "work1", "lease1", vec!["file.txt".to_string()]);
        envelope
            .before_hashes
            .insert("file.txt".to_string(), "irrelevant".to_string());
        let error = check_patch_envelope(&repo, &envelope).expect_err("dirty path");
        assert!(matches!(error, GitError::DirtyPath(path) if path == "file.txt"));
        assert_eq!(
            fs::read_to_string(repo.join("file.txt")).expect("file"),
            "user dirty\n"
        );
    }

    #[test]
    fn failed_verifier_rolls_patch_back() {
        let repo = init_repo("rollback");
        let before = content_hash(&repo.join("file.txt")).expect("hash");
        let mut envelope =
            new_patch_envelope("patch2", "work2", "lease2", vec!["file.txt".to_string()]);
        envelope
            .before_hashes
            .insert("file.txt".to_string(), before);
        let diff = "\
diff --git a/file.txt b/file.txt
index 3b18e51..a5df3c0 100644
--- a/file.txt
+++ b/file.txt
@@ -1 +1 @@
-before
+after
";
        let error =
            apply_unified_diff_with_rollback(&repo, &envelope, diff, || false).expect_err("fail");
        assert!(matches!(error, GitError::VerificationFailedRolledBack));
        assert_eq!(
            fs::read_to_string(repo.join("file.txt")).expect("file"),
            "before\n"
        );
    }

    #[test]
    fn working_tree_diff_returns_path_limited_patch() {
        let repo = init_repo("working-tree-diff");
        fs::write(repo.join("file.txt"), "after\n").expect("modify file");
        fs::write(repo.join("other.txt"), "other\n").expect("write other");

        let diff = working_tree_diff(&repo, &["file.txt".to_string()]).expect("working tree diff");

        assert!(diff.contains("diff --git a/file.txt b/file.txt"));
        assert!(diff.contains("-before"));
        assert!(diff.contains("+after"));
        assert!(!diff.contains("other.txt"));
    }

    #[test]
    fn working_tree_diff_includes_untracked_target_file() {
        let repo = init_repo("working-tree-diff-untracked");
        fs::write(repo.join("new.txt"), "created\n").expect("write untracked file");
        fs::write(repo.join("other.txt"), "other\n").expect("write other");

        let diff = working_tree_diff(&repo, &["new.txt".to_string()]).expect("working tree diff");

        assert!(diff.contains("diff --git a/new.txt b/new.txt"));
        assert!(diff.contains("new file mode"));
        assert!(diff.contains("+created"));
        assert!(!diff.contains("other.txt"));
    }

    #[test]
    fn target_paths_changed_since_base_detects_committed_target_drift() {
        let repo = init_repo("target-drift");
        let base = git(&repo, ["rev-parse", "HEAD"])
            .expect("base commit")
            .trim()
            .to_string();
        fs::write(repo.join("file.txt"), "after\n").expect("modify target");
        run_git(&repo, ["add", "file.txt"]).expect("git add target");
        run_git(&repo, ["commit", "-m", "target drift"]).expect("git commit target");

        let changed = target_paths_changed_since_base(&repo, &base, &["file.txt".to_string()])
            .expect("target drift check");

        assert!(changed);
    }

    #[test]
    fn target_paths_changed_since_base_ignores_unrelated_commits() {
        let repo = init_repo("unrelated-drift");
        let base = git(&repo, ["rev-parse", "HEAD"])
            .expect("base commit")
            .trim()
            .to_string();
        fs::write(repo.join("other.txt"), "other\n").expect("write unrelated");
        run_git(&repo, ["add", "other.txt"]).expect("git add unrelated");
        run_git(&repo, ["commit", "-m", "unrelated drift"]).expect("git commit unrelated");

        let changed = target_paths_changed_since_base(&repo, &base, &["file.txt".to_string()])
            .expect("target drift check");
        let empty_changed =
            target_paths_changed_since_base(&repo, &base, &[]).expect("empty target drift check");

        assert!(!changed);
        assert!(!empty_changed);
    }

    #[test]
    fn head_blob_hash_resolves_for_tracked_file_and_none_otherwise() {
        let repo = init_repo("head-blob");
        let tracked = head_blob_hash(&repo, "file.txt").expect("head blob");
        assert!(
            tracked.is_some(),
            "committed file resolves a blob id at HEAD"
        );
        let missing = head_blob_hash(&repo, "never-committed.txt").expect("head blob missing");
        assert!(missing.is_none(), "untracked path has no HEAD blob");
    }

    #[test]
    fn head_blob_hash_returns_none_outside_repo() {
        let root = temp_dir("no-repo");
        fs::write(root.join("plain.txt"), "hi").expect("write");
        assert!(head_blob_hash(&root, "plain.txt").expect("none").is_none());
    }

    #[test]
    fn outbox_blocks_secret_stage_and_queues_protected_push_for_approval() {
        let mut outbox = Outbox::new();
        let secret = outbox
            .enqueue_commit("commit-secret", &[".env".to_string()])
            .expect_err("secret stage");
        assert!(matches!(secret, GitError::SecretStageBlocked(path) if path == ".env"));

        let protected = outbox
            .enqueue_push("main", false)
            .expect("protected push is preserved for approval");
        assert_eq!(protected.state, "awaiting_approval");
        assert!(protected.approval_required);
        assert!(protected.protected_branch);
    }

    #[test]
    fn outbox_idempotency_blocks_duplicate_remote_write() {
        let mut outbox = Outbox::new();
        outbox.enqueue_push("feature", true).expect("first push");
        let duplicate = outbox.enqueue_push("feature", true).expect_err("duplicate");
        assert!(matches!(duplicate, GitError::DuplicateOutboxWrite(key) if key == "push:feature"));
    }

    #[test]
    fn outbox_dispatch_never_executes_push_without_matching_approval() {
        let mut outbox = Outbox::new();
        outbox.enqueue_push("main", false).expect("push item");
        let mut executed = false;

        let report = outbox
            .dispatch_next(&[], |_| {
                executed = true;
                Ok(())
            })
            .expect("dispatch report");

        assert!(!executed);
        assert!(!report.executed);
        assert_eq!(report.state, "awaiting_approval");
        assert_eq!(report.reason_code, "approval_required");
        assert_eq!(outbox.items()[0].attempt_count, 0);
    }

    #[test]
    fn outbox_dispatch_executes_once_after_matching_approval() {
        let mut outbox = Outbox::new();
        let item = outbox.enqueue_push("main", false).expect("push item");
        let approvals = vec![OutboxApproval {
            approval_id: item.approval_id.expect("approval id"),
            scope: "git_push".to_string(),
            target: "main".to_string(),
            approved: true,
        }];
        let mut execution_count = 0;

        let report = outbox
            .dispatch_next(&approvals, |_| {
                execution_count += 1;
                Ok(())
            })
            .expect("dispatch report");

        assert_eq!(execution_count, 1);
        assert!(report.executed);
        assert_eq!(report.state, "executed");
        assert_eq!(report.reason_code, "executed_after_approval");
        assert_eq!(outbox.items()[0].attempt_count, 1);
        assert!(matches!(
            outbox.dispatch_next(&approvals, |_| Ok(())),
            Err(GitError::EmptyOutbox)
        ));
    }

    #[test]
    fn outbox_dispatch_records_retry_wait_then_succeeds() {
        let mut outbox = Outbox::new();
        let item = outbox.enqueue_push("feature", true).expect("push item");
        let approvals = vec![OutboxApproval {
            approval_id: item.approval_id.expect("approval id"),
            scope: "git_push".to_string(),
            target: "feature".to_string(),
            approved: true,
        }];

        let retry = outbox
            .dispatch_next(&approvals, |_| {
                Err(GitError::GitCommand(
                    "network down after local commit".to_string(),
                ))
            })
            .expect("retry report");
        assert!(!retry.executed);
        assert_eq!(retry.state, "retry_wait");
        assert_eq!(retry.attempt_count, 1);

        let success = outbox
            .dispatch_next(&approvals, |_| Ok(()))
            .expect("success report");
        assert!(success.executed);
        assert_eq!(success.state, "executed");
        assert_eq!(success.attempt_count, 2);
    }
}
