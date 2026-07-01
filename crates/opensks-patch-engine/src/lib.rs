//! Commercial patch engine foundation.
//!
//! This crate is the single owner for applying workspace file writes produced by
//! agent workers. It gives adapter/testkit callers a safer bridge while the full
//! isolated-worktree PatchEngine matures: canonical workspace anchoring,
//! symlink-aware containment, SHA-256 content identity, pre-image validation,
//! same-directory temp writes with fsync, stale temp scavenging, atomic rename,
//! directory fsync, and typed rollback receipts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use opensks_contracts::{FileOperation, PATCH_APPLY_RESULT_SCHEMA, PatchApplyResult};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

const PATCH_TRANSACTION_JOURNAL_SCHEMA: &str = "opensks.patch-transaction-journal.v1";
const STALE_PATCH_TEMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_JOURNAL_ATTEMPTS: u64 = 500;

#[derive(Debug, Error)]
pub enum PatchEngineError {
    #[error("workspace root is invalid: {0}")]
    InvalidWorkspace(String),
    #[error("path `{0}` escapes the workspace or is not workspace-relative")]
    PathEscape(String),
    #[error("symlink path component rejected: {0}")]
    SymlinkRejected(String),
    #[error("io error at `{path}` during {stage}: {source}")]
    Io {
        path: String,
        stage: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("read-back mismatch at `{path}`: expected {expected}, got {actual}")]
    ReadBackMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("pre-apply revalidation failed at `{path}`: expected {expected}, got {actual}")]
    PreApplyChanged {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("failed to serialize patch transaction journal: {0}")]
    JournalSerialize(String),
    #[error("journal attempt limit exceeded for proposal `{proposal_id}`: attempt {attempt_index}")]
    JournalAttemptLimitExceeded {
        proposal_id: String,
        attempt_index: u64,
    },
}

#[derive(Debug, Clone)]
pub struct PlannedPatchWrite {
    pub path: String,
    pub expected_before_hash: String,
    pub after_content: String,
    pub operation: FileOperation,
    pub rename_to: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PatchApplyContext {
    pub lease_id: String,
    pub fence_token: String,
    pub leased_paths: Vec<String>,
}

impl PatchApplyContext {
    pub fn new(
        lease_id: impl Into<String>,
        fence_token: impl Into<String>,
        leased_paths: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            lease_id: lease_id.into(),
            fence_token: fence_token.into(),
            leased_paths: leased_paths.into_iter().collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchRecoveryAttempt {
    pub attempt_index: u64,
    pub attempt_id: String,
    pub state: String,
    pub reason_code: String,
    pub rolled_back: bool,
    pub applied_files: Vec<String>,
    pub conflict_paths: Vec<String>,
    pub journal_events: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchRecoveryReport {
    pub proposal_id: String,
    pub journal_ref: String,
    pub attempts: Vec<PatchRecoveryAttempt>,
    pub latest_attempt: Option<PatchRecoveryAttempt>,
    pub requires_operator_review: bool,
    pub reason_code: String,
}

#[derive(Debug, Clone)]
pub struct PatchEngine {
    root: PathBuf,
    #[cfg(test)]
    temp_scavenge_now: Option<SystemTime>,
}

struct PatchJournalEvent<'a> {
    proposal_id: &'a str,
    attempt_index: u64,
    sequence: u64,
    state: &'a str,
    writes: &'a [PlannedPatchWrite],
    applied_files: &'a [String],
    conflict_paths: &'a [String],
    rolled_back: bool,
    reason_code: &'a str,
    context: Option<&'a PatchApplyContext>,
}

#[derive(Debug, Clone)]
struct PreparedPatchWrite {
    path: String,
    target: PathBuf,
    before: Option<String>,
    operation: FileOperation,
    rename_to: Option<(String, PathBuf)>,
}

#[cfg(test)]
type PreApplyTestHook = Box<dyn Fn(&PlannedPatchWrite) + Send + Sync + 'static>;

#[cfg(test)]
static PRE_APPLY_TEST_HOOK: std::sync::Mutex<Option<PreApplyTestHook>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
type ApplyFaultTestHook =
    Box<dyn Fn(&PlannedPatchWrite) -> Option<PatchEngineError> + Send + Sync + 'static>;

#[cfg(test)]
static APPLY_FAULT_TEST_HOOK: std::sync::Mutex<Option<ApplyFaultTestHook>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn run_pre_apply_test_hook(write: &PlannedPatchWrite) {
    if let Some(hook) = PRE_APPLY_TEST_HOOK
        .lock()
        .expect("pre-apply hook lock")
        .as_ref()
    {
        hook(write);
    }
}

#[cfg(test)]
fn run_apply_fault_test_hook(write: &PlannedPatchWrite) -> Option<PatchEngineError> {
    APPLY_FAULT_TEST_HOOK
        .lock()
        .expect("apply fault hook lock")
        .as_ref()
        .and_then(|hook| hook(write))
}

/// Process-wide registry of per-journal-file locks. Serializes attempt-index
/// allocation and journal appends for a given proposal journal across all
/// `PatchEngine` instances (even freshly opened ones) within this process, so
/// concurrent `apply` calls for the same proposal cannot race on
/// `next_journal_attempt_index` and produce duplicate attempt indices.
static JOURNAL_LOCKS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, std::sync::Arc<std::sync::Mutex<()>>>>,
> = std::sync::OnceLock::new();

fn journal_lock(path: &Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    let registry =
        JOURNAL_LOCKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut guard = registry.lock().expect("journal lock registry");
    guard
        .entry(path.to_path_buf())
        .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(())))
        .clone()
}

impl PatchEngine {
    pub fn open(workspace: impl AsRef<Path>) -> Result<Self, PatchEngineError> {
        let raw = workspace.as_ref();
        let root = raw
            .canonicalize()
            .map_err(|_| PatchEngineError::InvalidWorkspace(raw.display().to_string()))?;
        if !root.is_dir() {
            return Err(PatchEngineError::InvalidWorkspace(
                raw.display().to_string(),
            ));
        }
        Ok(Self {
            root,
            #[cfg(test)]
            temp_scavenge_now: None,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn read_to_string(&self, relative: &str) -> Result<Option<String>, PatchEngineError> {
        let target = self.resolve(relative)?;
        match fs::read_to_string(&target) {
            Ok(content) => Ok(Some(content)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(PatchEngineError::Io {
                path: relative.to_string(),
                stage: "read",
                source,
            }),
        }
    }

    pub fn apply(
        &self,
        proposal_id: &str,
        writes: &[PlannedPatchWrite],
    ) -> Result<PatchApplyResult, PatchEngineError> {
        self.apply_inner(proposal_id, writes, None)
    }

    pub fn apply_with_context(
        &self,
        proposal_id: &str,
        writes: &[PlannedPatchWrite],
        context: &PatchApplyContext,
    ) -> Result<PatchApplyResult, PatchEngineError> {
        self.apply_inner(proposal_id, writes, Some(context))
    }

    fn apply_inner(
        &self,
        proposal_id: &str,
        writes: &[PlannedPatchWrite],
        context: Option<&PatchApplyContext>,
    ) -> Result<PatchApplyResult, PatchEngineError> {
        let journal_path = self.root.join(journal_relative_path(proposal_id));
        let journal_mutex = journal_lock(&journal_path);
        let _journal_guard = journal_mutex.lock().expect("journal lock");
        let attempt_index = self.next_journal_attempt_index(proposal_id)?;
        if attempt_index > MAX_JOURNAL_ATTEMPTS {
            return Err(PatchEngineError::JournalAttemptLimitExceeded {
                proposal_id: proposal_id.to_string(),
                attempt_index,
            });
        }
        let journal_ref = format!("patch-engine:journal:{proposal_id}");
        let result = |applied: bool,
                      applied_files: Vec<String>,
                      conflict_paths: Vec<String>,
                      rolled_back: bool,
                      reason: &str| {
            let mut evidence_refs =
                vec!["patch-engine:atomic-apply".to_string(), journal_ref.clone()];
            if applied {
                evidence_refs.push("patch-engine:pre-apply-revalidated".to_string());
                if context.is_some() {
                    evidence_refs.push("patch-engine:path-lease-bound".to_string());
                    evidence_refs.push("patch-engine:fence-token-bound".to_string());
                }
                evidence_refs.push("patch-engine:read-back-verified".to_string());
            }
            if rolled_back {
                evidence_refs.push("patch-engine:rollback-attempted".to_string());
            }
            PatchApplyResult {
                schema: PATCH_APPLY_RESULT_SCHEMA.to_string(),
                proposal_id: proposal_id.to_string(),
                applied,
                applied_files,
                conflict_paths,
                rolled_back,
                reason_code: reason.to_string(),
                evidence_refs,
            }
        };

        let mut targets = Vec::with_capacity(writes.len());
        let mut conflicts = Vec::new();
        let mut operation_conflicts = Vec::new();
        let mut touched_paths = BTreeSet::new();
        for write in writes {
            let target = self.resolve(&write.path)?;
            if !touched_paths.insert(write.path.clone()) {
                operation_conflicts.push(write.path.clone());
            }
            let rename_to = match write.operation {
                FileOperation::Rename => match write.rename_to.as_deref() {
                    Some(dest) => {
                        let dest_target = self.resolve(dest)?;
                        if !touched_paths.insert(dest.to_string()) {
                            operation_conflicts.push(dest.to_string());
                        }
                        Some((dest.to_string(), dest_target))
                    }
                    None => {
                        operation_conflicts.push(write.path.clone());
                        None
                    }
                },
                _ => None,
            };
            let current = self.read_target_text(&write.path, &target, "preflight_read")?;
            match (write.operation, current.is_some()) {
                (FileOperation::Create, true) => operation_conflicts.push(write.path.clone()),
                (FileOperation::Modify, false) => operation_conflicts.push(write.path.clone()),
                (FileOperation::Delete | FileOperation::Rename, false) => {
                    operation_conflicts.push(write.path.clone())
                }
                (FileOperation::Rename, true) => {
                    if let Some((dest, dest_target)) = &rename_to
                        && dest_target.exists()
                    {
                        operation_conflicts.push(dest.clone());
                    }
                }
                _ => {}
            }
            let current_hash = content_hash(current.as_deref().unwrap_or(""));
            if current_hash != write.expected_before_hash {
                conflicts.push(write.path.clone());
            }
            targets.push(PreparedPatchWrite {
                path: write.path.clone(),
                target,
                before: current,
                operation: write.operation,
                rename_to,
            });
        }
        if let Some(context) = context
            && let Some(missing) = missing_leased_paths(context, &touched_paths)
        {
            self.append_journal_event(PatchJournalEvent {
                proposal_id,
                attempt_index,
                sequence: 0,
                state: "rejected",
                writes,
                applied_files: &[],
                conflict_paths: &missing,
                rolled_back: false,
                reason_code: "path_lease_missing",
                context: Some(context),
            })?;
            return Ok(result(false, vec![], missing, false, "path_lease_missing"));
        }
        if !operation_conflicts.is_empty() {
            self.append_journal_event(PatchJournalEvent {
                proposal_id,
                attempt_index,
                sequence: 0,
                state: "rejected",
                writes,
                applied_files: &[],
                conflict_paths: &operation_conflicts,
                rolled_back: false,
                reason_code: "operation_precondition",
                context,
            })?;
            return Ok(result(
                false,
                vec![],
                operation_conflicts,
                false,
                "operation_precondition",
            ));
        }
        if !conflicts.is_empty() {
            self.append_journal_event(PatchJournalEvent {
                proposal_id,
                attempt_index,
                sequence: 0,
                state: "rejected",
                writes,
                applied_files: &[],
                conflict_paths: &conflicts,
                rolled_back: false,
                reason_code: "stale_precondition",
                context,
            })?;
            return Ok(result(
                false,
                vec![],
                conflicts,
                false,
                "stale_precondition",
            ));
        }

        self.append_journal_event(PatchJournalEvent {
            proposal_id,
            attempt_index,
            sequence: 0,
            state: "planned",
            writes,
            applied_files: &[],
            conflict_paths: &[],
            rolled_back: false,
            reason_code: "preflight_ok",
            context,
        })?;
        let mut applied_files = Vec::new();
        for (idx, write) in writes.iter().enumerate() {
            let prepared = &targets[idx];
            #[cfg(test)]
            run_pre_apply_test_hook(write);
            if let Err(error) = self.revalidate_before_apply(write, prepared) {
                let conflict_paths = error_paths(&error);
                let reason_code = pre_apply_reason_code(&error);
                let had_applied = idx > 0;
                let rolled_back = had_applied && self.rollback(&targets[..idx]).is_ok();
                self.append_journal_event(PatchJournalEvent {
                    proposal_id,
                    attempt_index,
                    sequence: 1,
                    state: if had_applied {
                        if rolled_back {
                            "rolled_back"
                        } else {
                            "rollback_failed_critical"
                        }
                    } else {
                        "rejected"
                    },
                    writes,
                    applied_files: &applied_files,
                    conflict_paths: &conflict_paths,
                    rolled_back,
                    reason_code: if had_applied && !rolled_back {
                        "rollback_failed_critical"
                    } else {
                        reason_code
                    },
                    context,
                })?;
                return if had_applied && !rolled_back {
                    Ok(result(
                        false,
                        applied_files,
                        conflict_paths,
                        true,
                        "rollback_failed_critical",
                    ))
                } else {
                    Ok(result(
                        false,
                        applied_files,
                        conflict_paths,
                        rolled_back,
                        reason_code,
                    ))
                };
            }
            #[cfg(test)]
            let apply_result = run_apply_fault_test_hook(write)
                .map_or_else(|| self.apply_prepared_write(write, prepared), Err);
            #[cfg(not(test))]
            let apply_result = self.apply_prepared_write(write, prepared);
            if apply_result.is_err() {
                let rolled_back = self.rollback(&targets[..idx]).is_ok();
                self.append_journal_event(PatchJournalEvent {
                    proposal_id,
                    attempt_index,
                    sequence: 1,
                    state: if rolled_back {
                        "rolled_back"
                    } else {
                        "rollback_failed_critical"
                    },
                    writes,
                    applied_files: &applied_files,
                    conflict_paths: &[],
                    rolled_back,
                    reason_code: if rolled_back {
                        "io_rolled_back"
                    } else {
                        "rollback_failed_critical"
                    },
                    context,
                })?;
                return if rolled_back {
                    Ok(result(false, applied_files, vec![], true, "io_rolled_back"))
                } else {
                    Ok(result(
                        false,
                        applied_files,
                        vec![],
                        true,
                        "rollback_failed_critical",
                    ))
                };
            }
            if let Err(error) = self.verify_applied_write(write, prepared) {
                let applied_paths = applied_paths(prepared);
                let reason_code = match error {
                    PatchEngineError::ReadBackMismatch { .. } => "read_back_mismatch",
                    _ => "read_back_error",
                };
                let rolled_back = self.rollback(&targets[..=idx]).is_ok();
                self.append_journal_event(PatchJournalEvent {
                    proposal_id,
                    attempt_index,
                    sequence: 1,
                    state: if rolled_back {
                        "rolled_back"
                    } else {
                        "rollback_failed_critical"
                    },
                    writes,
                    applied_files: &applied_paths,
                    conflict_paths: &applied_paths,
                    rolled_back,
                    reason_code: if rolled_back {
                        reason_code
                    } else {
                        "rollback_failed_critical"
                    },
                    context,
                })?;
                return if rolled_back {
                    Ok(result(false, vec![], applied_paths, true, reason_code))
                } else {
                    Ok(result(
                        false,
                        vec![],
                        applied_paths,
                        true,
                        "rollback_failed_critical",
                    ))
                };
            }
            applied_files.extend(applied_paths(prepared));
        }

        // The real target files are already atomically written and read-back
        // verified at this point. A failure to append the terminal "applied"
        // journal event (e.g. ENOSPC, transient EIO) must not be surfaced as
        // an Err, since that would tell the caller the patch was rejected
        // when it was in fact fully and correctly applied.
        let journal_write_failed = self
            .append_journal_event(PatchJournalEvent {
                proposal_id,
                attempt_index,
                sequence: 1,
                state: "applied",
                writes,
                applied_files: &applied_files,
                conflict_paths: &[],
                rolled_back: false,
                reason_code: "applied",
                context,
            })
            .is_err();
        Ok(result(
            true,
            applied_files,
            vec![],
            false,
            if journal_write_failed {
                "applied_journal_write_degraded"
            } else {
                "applied"
            },
        ))
    }

    pub fn recovery_report(
        &self,
        proposal_id: &str,
    ) -> Result<PatchRecoveryReport, PatchEngineError> {
        let journal_ref = format!("patch-engine:journal:{proposal_id}");
        let relative = journal_relative_path(proposal_id);
        let path = self.root.join(&relative);
        self.guard_no_symlink(&relative, &path)?;
        self.verify_containment(&relative, &path)?;

        let file = match File::open(&path) {
            Ok(file) => file,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PatchRecoveryReport {
                    proposal_id: proposal_id.to_string(),
                    journal_ref,
                    attempts: Vec::new(),
                    latest_attempt: None,
                    requires_operator_review: false,
                    reason_code: "journal_missing".to_string(),
                });
            }
            Err(source) => {
                return Err(PatchEngineError::Io {
                    path: relative,
                    stage: "recovery_read_journal",
                    source,
                });
            }
        };

        let mut attempts = BTreeMap::<u64, PatchRecoveryAttempt>::new();
        let mut requires_operator_review = false;
        let mut inferred_attempt_index = 0;

        for line in std::io::BufRead::lines(std::io::BufReader::new(file)) {
            let Ok(line) = line else {
                requires_operator_review = true;
                continue;
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_str::<Value>(line) else {
                requires_operator_review = true;
                continue;
            };
            let state = json_string(&event, "state").unwrap_or_else(|| {
                requires_operator_review = true;
                "unknown".to_string()
            });
            if event.get("attempt_index").and_then(Value::as_u64).is_none()
                && (state == "planned" || inferred_attempt_index == 0)
            {
                inferred_attempt_index += 1;
            }
            let attempt_index = event
                .get("attempt_index")
                .and_then(Value::as_u64)
                .unwrap_or(inferred_attempt_index.max(1));
            let attempt_id = json_string(&event, "attempt_id")
                .unwrap_or_else(|| journal_attempt_id(attempt_index));
            let reason_code = json_string(&event, "reason_code").unwrap_or_else(|| {
                requires_operator_review = true;
                "missing_reason_code".to_string()
            });
            let rolled_back = event
                .get("rolled_back")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let applied_files = json_string_array(&event, "applied_files");
            let conflict_paths = json_string_array(&event, "conflict_paths");

            let attempt = attempts
                .entry(attempt_index)
                .or_insert_with(|| PatchRecoveryAttempt {
                    attempt_index,
                    attempt_id: attempt_id.clone(),
                    state: "unknown".to_string(),
                    reason_code: "missing_reason_code".to_string(),
                    rolled_back: false,
                    applied_files: Vec::new(),
                    conflict_paths: Vec::new(),
                    journal_events: 0,
                });
            attempt.attempt_id = attempt_id;
            attempt.state = state;
            attempt.reason_code = reason_code;
            attempt.rolled_back = rolled_back;
            attempt.applied_files = applied_files;
            attempt.conflict_paths = conflict_paths;
            attempt.journal_events += 1;
        }

        let attempts = attempts.into_values().collect::<Vec<_>>();
        let latest_attempt = attempts.last().cloned();
        if let Some(latest) = &latest_attempt
            && matches!(
                latest.state.as_str(),
                "planned" | "rollback_failed_critical" | "unknown"
            )
        {
            requires_operator_review = true;
        }
        let reason_code = if requires_operator_review {
            "operator_review_required".to_string()
        } else {
            latest_attempt
                .as_ref()
                .map(|attempt| attempt.reason_code.clone())
                .unwrap_or_else(|| "journal_missing".to_string())
        };

        Ok(PatchRecoveryReport {
            proposal_id: proposal_id.to_string(),
            journal_ref,
            attempts,
            latest_attempt,
            requires_operator_review,
            reason_code,
        })
    }

    pub fn resolve(&self, relative: &str) -> Result<PathBuf, PatchEngineError> {
        let rel = Path::new(relative);
        if relative.is_empty() || rel.is_absolute() {
            return Err(PatchEngineError::PathEscape(relative.to_string()));
        }
        let mut normalized = PathBuf::new();
        for component in rel.components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(PatchEngineError::PathEscape(relative.to_string()));
                }
            }
        }
        if normalized.as_os_str().is_empty() {
            return Err(PatchEngineError::PathEscape(relative.to_string()));
        }
        let candidate = self.root.join(normalized);
        self.guard_no_symlink(relative, &candidate)?;
        self.verify_containment(relative, &candidate)?;
        Ok(candidate)
    }

    fn verify_containment(&self, relative: &str, candidate: &Path) -> Result<(), PatchEngineError> {
        let existing = deepest_existing_ancestor(candidate);
        let canonical = existing
            .canonicalize()
            .map_err(|source| PatchEngineError::Io {
                path: relative.to_string(),
                stage: "canonicalize",
                source,
            })?;
        if canonical.starts_with(&self.root) {
            Ok(())
        } else {
            Err(PatchEngineError::PathEscape(relative.to_string()))
        }
    }

    fn guard_no_symlink(&self, relative: &str, candidate: &Path) -> Result<(), PatchEngineError> {
        let suffix = candidate.strip_prefix(&self.root).unwrap_or(candidate);
        let mut current = self.root.clone();
        for component in suffix.components() {
            if let Component::Normal(part) = component {
                current.push(part);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(PatchEngineError::SymlinkRejected(relative.to_string()));
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        }
        Ok(())
    }

    fn atomic_replace(
        &self,
        relative: &str,
        target: &Path,
        bytes: &[u8],
    ) -> Result<(), PatchEngineError> {
        let parent = target
            .parent()
            .ok_or_else(|| PatchEngineError::PathEscape(relative.to_string()))?;
        fs::create_dir_all(parent).map_err(|source| PatchEngineError::Io {
            path: relative.to_string(),
            stage: "create_parent",
            source,
        })?;
        self.guard_no_symlink(relative, target)?;
        self.verify_containment(relative, target)?;

        let target_mode = fs::symlink_metadata(target)
            .ok()
            .map(|metadata| metadata.permissions().mode())
            .unwrap_or(0o644);
        self.scavenge_stale_temp_siblings(relative, target)?;
        let temp = temp_sibling(target);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|source| PatchEngineError::Io {
                path: relative.to_string(),
                stage: "create_temp",
                source,
            })?;
        if let Err(source) = file.write_all(bytes) {
            let _ = fs::remove_file(&temp);
            return Err(PatchEngineError::Io {
                path: relative.to_string(),
                stage: "write_temp",
                source,
            });
        }
        if let Err(source) = file.sync_all() {
            let _ = fs::remove_file(&temp);
            return Err(PatchEngineError::Io {
                path: relative.to_string(),
                stage: "fsync_temp",
                source,
            });
        }
        if let Err(source) = fs::set_permissions(&temp, fs::Permissions::from_mode(target_mode)) {
            let _ = fs::remove_file(&temp);
            return Err(PatchEngineError::Io {
                path: relative.to_string(),
                stage: "set_permissions",
                source,
            });
        }
        drop(file);
        if let Err(source) = fs::rename(&temp, target) {
            let _ = fs::remove_file(&temp);
            return Err(PatchEngineError::Io {
                path: relative.to_string(),
                stage: "rename",
                source,
            });
        }
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    fn atomic_remove(&self, relative: &str, target: &Path) -> Result<(), PatchEngineError> {
        self.guard_no_symlink(relative, target)?;
        self.verify_containment(relative, target)?;
        fs::remove_file(target).map_err(|source| PatchEngineError::Io {
            path: relative.to_string(),
            stage: "remove_file",
            source,
        })?;
        if let Some(parent) = target.parent()
            && let Ok(dir) = File::open(parent)
        {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    fn atomic_rename(
        &self,
        source_relative: &str,
        source: &Path,
        dest_relative: &str,
        dest: &Path,
    ) -> Result<(), PatchEngineError> {
        let parent = dest
            .parent()
            .ok_or_else(|| PatchEngineError::PathEscape(dest_relative.to_string()))?;
        fs::create_dir_all(parent).map_err(|source| PatchEngineError::Io {
            path: dest_relative.to_string(),
            stage: "create_rename_parent",
            source,
        })?;
        self.guard_no_symlink(source_relative, source)?;
        self.guard_no_symlink(dest_relative, dest)?;
        self.verify_containment(source_relative, source)?;
        self.verify_containment(dest_relative, dest)?;
        fs::rename(source, dest).map_err(|source| PatchEngineError::Io {
            path: format!("{source_relative}->{dest_relative}"),
            stage: "rename_file",
            source,
        })?;
        if let Some(parent) = source.parent()
            && let Ok(dir) = File::open(parent)
        {
            let _ = dir.sync_all();
        }
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    fn scavenge_stale_temp_siblings(
        &self,
        relative: &str,
        target: &Path,
    ) -> Result<(), PatchEngineError> {
        let parent = target
            .parent()
            .ok_or_else(|| PatchEngineError::PathEscape(relative.to_string()))?;
        let Some(file_name) = target
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
        else {
            return Ok(());
        };
        let now = self.temp_scavenge_now();
        let mut removed_any = false;
        let entries = fs::read_dir(parent).map_err(|source| PatchEngineError::Io {
            path: relative.to_string(),
            stage: "scavenge_temp_dir",
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| PatchEngineError::Io {
                path: relative.to_string(),
                stage: "scavenge_temp_entry",
                source,
            })?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !is_patch_temp_sibling_name(&name, &file_name) {
                continue;
            }
            let file_type = entry.file_type().map_err(|source| PatchEngineError::Io {
                path: relative.to_string(),
                stage: "scavenge_temp_type",
                source,
            })?;
            if !file_type.is_file() && !file_type.is_symlink() {
                continue;
            }
            let metadata =
                fs::symlink_metadata(entry.path()).map_err(|source| PatchEngineError::Io {
                    path: relative.to_string(),
                    stage: "scavenge_temp_metadata",
                    source,
                })?;
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            let Ok(age) = now.duration_since(modified) else {
                continue;
            };
            if age < STALE_PATCH_TEMP_MAX_AGE {
                continue;
            }
            fs::remove_file(entry.path()).map_err(|source| PatchEngineError::Io {
                path: relative.to_string(),
                stage: "scavenge_temp_remove",
                source,
            })?;
            removed_any = true;
        }
        if removed_any && let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    fn temp_scavenge_now(&self) -> SystemTime {
        #[cfg(test)]
        if let Some(now) = self.temp_scavenge_now {
            return now;
        }
        SystemTime::now()
    }

    fn read_target_text(
        &self,
        relative: &str,
        target: &Path,
        stage: &'static str,
    ) -> Result<Option<String>, PatchEngineError> {
        match fs::read_to_string(target) {
            Ok(content) => Ok(Some(content)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(PatchEngineError::Io {
                path: relative.to_string(),
                stage,
                source,
            }),
        }
    }

    fn verify_applied_write(
        &self,
        write: &PlannedPatchWrite,
        prepared: &PreparedPatchWrite,
    ) -> Result<(), PatchEngineError> {
        match write.operation {
            FileOperation::Create | FileOperation::Modify => {
                let actual = self
                    .read_target_text(&write.path, &prepared.target, "read_back")?
                    .map(|content| content_hash(&content))
                    .unwrap_or_else(|| "missing".to_string());
                let expected = content_hash(&write.after_content);
                if actual != expected {
                    return Err(PatchEngineError::ReadBackMismatch {
                        path: write.path.clone(),
                        expected,
                        actual,
                    });
                }
            }
            FileOperation::Delete => {
                if prepared.target.exists() {
                    return Err(PatchEngineError::ReadBackMismatch {
                        path: write.path.clone(),
                        expected: "missing".to_string(),
                        actual: "present".to_string(),
                    });
                }
            }
            FileOperation::Rename => {
                if prepared.target.exists() {
                    return Err(PatchEngineError::ReadBackMismatch {
                        path: write.path.clone(),
                        expected: "missing".to_string(),
                        actual: "present".to_string(),
                    });
                }
                let Some((dest_relative, dest_target)) = &prepared.rename_to else {
                    return Err(PatchEngineError::ReadBackMismatch {
                        path: write.path.clone(),
                        expected: "rename_destination".to_string(),
                        actual: "missing_rename_destination".to_string(),
                    });
                };
                let actual = self
                    .read_target_text(dest_relative, dest_target, "read_back")?
                    .map(|content| content_hash(&content))
                    .unwrap_or_else(|| "missing".to_string());
                if actual != write.expected_before_hash {
                    return Err(PatchEngineError::ReadBackMismatch {
                        path: dest_relative.clone(),
                        expected: write.expected_before_hash.clone(),
                        actual,
                    });
                }
            }
        }
        Ok(())
    }

    fn revalidate_before_apply(
        &self,
        write: &PlannedPatchWrite,
        prepared: &PreparedPatchWrite,
    ) -> Result<(), PatchEngineError> {
        self.guard_no_symlink(&write.path, &prepared.target)?;
        self.verify_containment(&write.path, &prepared.target)?;

        let current = self.read_target_text(&write.path, &prepared.target, "pre_apply_read")?;
        let current_hash = current.as_deref().map(content_hash);
        match write.operation {
            FileOperation::Create => {
                if let Some(actual) = current_hash {
                    return Err(PatchEngineError::PreApplyChanged {
                        path: write.path.clone(),
                        expected: "missing".to_string(),
                        actual,
                    });
                }
            }
            FileOperation::Modify | FileOperation::Delete | FileOperation::Rename => {
                let actual = current_hash.unwrap_or_else(|| "missing".to_string());
                if actual != write.expected_before_hash {
                    return Err(PatchEngineError::PreApplyChanged {
                        path: write.path.clone(),
                        expected: write.expected_before_hash.clone(),
                        actual,
                    });
                }
            }
        }

        if write.operation == FileOperation::Rename {
            let Some((dest_relative, dest_target)) = &prepared.rename_to else {
                return Err(PatchEngineError::PreApplyChanged {
                    path: write.path.clone(),
                    expected: "rename_destination".to_string(),
                    actual: "missing_rename_destination".to_string(),
                });
            };
            self.guard_no_symlink(dest_relative, dest_target)?;
            self.verify_containment(dest_relative, dest_target)?;
            let dest_current =
                self.read_target_text(dest_relative, dest_target, "pre_apply_read")?;
            if let Some(content) = dest_current {
                return Err(PatchEngineError::PreApplyChanged {
                    path: dest_relative.clone(),
                    expected: "missing".to_string(),
                    actual: content_hash(&content),
                });
            }
        }

        Ok(())
    }

    fn apply_prepared_write(
        &self,
        write: &PlannedPatchWrite,
        prepared: &PreparedPatchWrite,
    ) -> Result<(), PatchEngineError> {
        match write.operation {
            FileOperation::Create | FileOperation::Modify => self.atomic_replace(
                &write.path,
                &prepared.target,
                write.after_content.as_bytes(),
            ),
            FileOperation::Delete => self.atomic_remove(&write.path, &prepared.target),
            FileOperation::Rename => {
                let Some((dest, dest_target)) = &prepared.rename_to else {
                    unreachable!("rename preflight requires destination");
                };
                self.atomic_rename(&write.path, &prepared.target, dest, dest_target)
            }
        }
    }

    fn rollback(&self, targets: &[PreparedPatchWrite]) -> Result<(), PatchEngineError> {
        for target in targets.iter().rev() {
            match target.operation {
                FileOperation::Create => {
                    if target.target.exists() {
                        self.atomic_remove(&target.path, &target.target)?;
                    }
                }
                FileOperation::Modify | FileOperation::Delete => {
                    if let Some(original) = &target.before {
                        self.atomic_replace(&target.path, &target.target, original.as_bytes())?;
                    } else if target.target.exists() {
                        self.atomic_remove(&target.path, &target.target)?;
                    }
                }
                FileOperation::Rename => {
                    if let Some((dest_relative, dest)) = &target.rename_to
                        && dest.exists()
                    {
                        self.atomic_rename(dest_relative, dest, &target.path, &target.target)?;
                    }
                    if let Some(original) = &target.before
                        && !target.target.exists()
                    {
                        self.atomic_replace(&target.path, &target.target, original.as_bytes())?;
                    }
                }
            }
        }
        Ok(())
    }

    fn append_journal_event(&self, event: PatchJournalEvent<'_>) -> Result<(), PatchEngineError> {
        let relative = journal_relative_path(event.proposal_id);
        let path = self.root.join(&relative);
        let parent = path
            .parent()
            .ok_or_else(|| PatchEngineError::PathEscape(relative.clone()))?;
        fs::create_dir_all(parent).map_err(|source| PatchEngineError::Io {
            path: relative.clone(),
            stage: "create_journal_parent",
            source,
        })?;
        self.guard_no_symlink(&relative, &path)?;
        self.verify_containment(&relative, &path)?;

        let files = event
            .writes
            .iter()
            .map(|write| {
                json!({
                    "path": write.path,
                    "rename_to": write.rename_to,
                    "operation": write.operation,
                    "expected_before_hash": write.expected_before_hash,
                    "after_hash": content_hash(&write.after_content),
                })
            })
            .collect::<Vec<_>>();
        let context = event.context.map(|context| {
            json!({
                "lease_id_hash": content_hash(&context.lease_id),
                "fence_token_hash": content_hash(&context.fence_token),
                "leased_path_count": context.leased_paths.len(),
                "raw_tokens_redacted": true,
            })
        });
        let event = json!({
            "schema": PATCH_TRANSACTION_JOURNAL_SCHEMA,
            "proposal_id": event.proposal_id,
            "attempt_index": event.attempt_index,
            "attempt_id": journal_attempt_id(event.attempt_index),
            "sequence": event.sequence,
            "state": event.state,
            "reason_code": event.reason_code,
            "rolled_back": event.rolled_back,
            "files": files,
            "applied_files": event.applied_files,
            "conflict_paths": event.conflict_paths,
            "content_redacted": true,
            "context": context,
        });
        let line = serde_json::to_string(&event)
            .map_err(|error| PatchEngineError::JournalSerialize(error.to_string()))?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)
            .map_err(|source| PatchEngineError::Io {
                path: relative.clone(),
                stage: "open_journal",
                source,
            })?;
        file.write_all(line.as_bytes())
            .and_then(|_| file.write_all(b"\n"))
            .map_err(|source| PatchEngineError::Io {
                path: relative.clone(),
                stage: "write_journal",
                source,
            })?;
        file.sync_all().map_err(|source| PatchEngineError::Io {
            path: relative.clone(),
            stage: "fsync_journal",
            source,
        })?;
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    fn next_journal_attempt_index(&self, proposal_id: &str) -> Result<u64, PatchEngineError> {
        let relative = journal_relative_path(proposal_id);
        let path = self.root.join(&relative);
        self.guard_no_symlink(&relative, &path)?;
        self.verify_containment(&relative, &path)?;
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(1),
            Err(source) => {
                return Err(PatchEngineError::Io {
                    path: relative,
                    stage: "read_journal_attempts",
                    source,
                });
            }
        };
        let mut max_attempt = 0;
        let mut legacy_attempts = 0;
        for line in std::io::BufRead::lines(std::io::BufReader::new(file)) {
            let line = line.map_err(|source| PatchEngineError::Io {
                path: relative.clone(),
                stage: "read_journal_attempts",
                source,
            })?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(attempt_index) = event.get("attempt_index").and_then(Value::as_u64) {
                max_attempt = max_attempt.max(attempt_index);
            } else if event.get("state").and_then(Value::as_str) == Some("planned")
                || legacy_attempts == 0
            {
                legacy_attempts += 1;
            }
        }
        Ok(if max_attempt > 0 {
            max_attempt + 1
        } else {
            legacy_attempts + 1
        })
    }
}

fn error_paths(error: &PatchEngineError) -> Vec<String> {
    match error {
        PatchEngineError::Io { path, .. }
        | PatchEngineError::ReadBackMismatch { path, .. }
        | PatchEngineError::PreApplyChanged { path, .. } => vec![path.clone()],
        PatchEngineError::InvalidWorkspace(_)
        | PatchEngineError::PathEscape(_)
        | PatchEngineError::SymlinkRejected(_)
        | PatchEngineError::JournalSerialize(_)
        | PatchEngineError::JournalAttemptLimitExceeded { .. } => Vec::new(),
    }
}

fn pre_apply_reason_code(error: &PatchEngineError) -> &'static str {
    match error {
        PatchEngineError::PreApplyChanged { .. } => "toctou_precondition",
        PatchEngineError::Io { stage, .. } if *stage == "pre_apply_read" => "pre_apply_read_error",
        _ => "pre_apply_revalidation_error",
    }
}

fn missing_leased_paths(
    context: &PatchApplyContext,
    touched_paths: &BTreeSet<String>,
) -> Option<Vec<String>> {
    if context.lease_id.trim().is_empty() || context.fence_token.trim().is_empty() {
        return Some(touched_paths.iter().cloned().collect());
    }
    let leased = context
        .leased_paths
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let missing = touched_paths
        .iter()
        .filter(|path| !leased.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        None
    } else {
        Some(missing)
    }
}

fn journal_attempt_id(attempt_index: u64) -> String {
    format!("attempt-{attempt_index}")
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn json_string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

pub fn content_hash(content: &str) -> String {
    format!("sha256:v1:{}", sha256_hex(content.as_bytes()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

pub fn unified_diff(path: &str, before: &str, after: &str) -> String {
    if before == after {
        return format!("--- a/{path}\n+++ b/{path}\n");
    }
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let mut out = format!(
        "--- a/{path}\n+++ b/{path}\n@@ -1,{} +1,{} @@\n",
        before_lines.len(),
        after_lines.len()
    );
    for line in before_lines {
        out.push('-');
        out.push_str(line);
        out.push('\n');
    }
    for line in after_lines {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn deepest_existing_ancestor(candidate: &Path) -> PathBuf {
    let mut current = candidate;
    loop {
        if current.exists() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return current.to_path_buf(),
        }
    }
}

fn temp_sibling(target: &Path) -> PathBuf {
    let file_name = target
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    target
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{file_name}.{}.{stamp}.tmp", std::process::id()))
}

fn is_patch_temp_sibling_name(name: &str, target_file_name: &str) -> bool {
    let prefix = format!(".{target_file_name}.");
    let Some(suffix) = name.strip_prefix(&prefix) else {
        return false;
    };
    let Some(stem) = suffix.strip_suffix(".tmp") else {
        return false;
    };
    let mut parts = stem.split('.');
    let Some(pid) = parts.next() else {
        return false;
    };
    let Some(stamp) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && !pid.is_empty()
        && !stamp.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && stamp.bytes().all(|byte| byte.is_ascii_digit())
}

fn applied_paths(target: &PreparedPatchWrite) -> Vec<String> {
    match &target.rename_to {
        Some((dest, _)) => vec![target.path.clone(), dest.clone()],
        None => vec![target.path.clone()],
    }
}

fn journal_relative_path(proposal_id: &str) -> String {
    let mut component = proposal_id
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '_',
        })
        .take(96)
        .collect::<String>();
    if component.is_empty() {
        component.push_str("proposal");
    }
    let digest = sha256_hex(proposal_id.as_bytes());
    format!(
        ".opensks/patch-engine/transactions/{component}-{}.jsonl",
        &digest[..12]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "opensks-patch-engine-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    }

    struct PreApplyHookGuard;

    impl PreApplyHookGuard {
        fn install(hook: PreApplyTestHook) -> Self {
            *PRE_APPLY_TEST_HOOK.lock().expect("pre-apply hook lock") = Some(hook);
            Self
        }
    }

    impl Drop for PreApplyHookGuard {
        fn drop(&mut self) {
            *PRE_APPLY_TEST_HOOK.lock().expect("pre-apply hook lock") = None;
        }
    }

    struct ApplyFaultHookGuard;

    impl ApplyFaultHookGuard {
        fn install(hook: ApplyFaultTestHook) -> Self {
            *APPLY_FAULT_TEST_HOOK.lock().expect("apply fault hook lock") = Some(hook);
            Self
        }
    }

    impl Drop for ApplyFaultHookGuard {
        fn drop(&mut self) {
            *APPLY_FAULT_TEST_HOOK.lock().expect("apply fault hook lock") = None;
        }
    }

    #[test]
    fn digest_is_canonical_sha256_v1() {
        assert_eq!(
            content_hash("abc"),
            "sha256:v1:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn applies_atomically_and_preserves_mode() {
        let root = workspace("apply");
        let engine = PatchEngine::open(&root).unwrap();
        fs::write(root.join("a.txt"), "one\n").unwrap();
        fs::set_permissions(root.join("a.txt"), fs::Permissions::from_mode(0o755)).unwrap();
        let result = engine
            .apply(
                "pp-1",
                &[PlannedPatchWrite {
                    path: "a.txt".to_string(),
                    expected_before_hash: content_hash("one\n"),
                    after_content: "two\n".to_string(),
                    operation: FileOperation::Modify,
                    rename_to: None,
                }],
            )
            .unwrap();
        assert!(result.applied);
        assert!(
            result
                .evidence_refs
                .contains(&"patch-engine:read-back-verified".to_string())
        );
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "two\n");
        assert_eq!(
            fs::symlink_metadata(root.join("a.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_replace_scavenges_only_stale_matching_temp_siblings() {
        let root = workspace("temp-scavenger");
        let mut engine = PatchEngine::open(&root).unwrap();
        engine.temp_scavenge_now =
            Some(SystemTime::now() + STALE_PATCH_TEMP_MAX_AGE + Duration::from_secs(1));
        fs::write(root.join("a.txt"), "one\n").unwrap();
        fs::write(root.join(".a.txt.7.8.tmp"), "stale patch temp\n").unwrap();
        fs::write(root.join(".b.txt.7.8.tmp"), "other target\n").unwrap();
        fs::write(root.join(".a.txt.bad.8.tmp"), "malformed\n").unwrap();

        let result = engine
            .apply(
                "pp-temp-scavenge",
                &[PlannedPatchWrite {
                    path: "a.txt".to_string(),
                    expected_before_hash: content_hash("one\n"),
                    after_content: "two\n".to_string(),
                    operation: FileOperation::Modify,
                    rename_to: None,
                }],
            )
            .unwrap();

        assert!(result.applied);
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "two\n");
        assert!(!root.join(".a.txt.7.8.tmp").exists());
        assert!(root.join(".b.txt.7.8.tmp").exists());
        assert!(root.join(".a.txt.bad.8.tmp").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_replace_preserves_fresh_matching_temp_siblings() {
        let root = workspace("fresh-temp-preserved");
        let engine = PatchEngine::open(&root).unwrap();
        fs::write(root.join("a.txt"), "one\n").unwrap();
        fs::write(root.join(".a.txt.7.8.tmp"), "fresh patch temp\n").unwrap();

        let result = engine
            .apply(
                "pp-fresh-temp",
                &[PlannedPatchWrite {
                    path: "a.txt".to_string(),
                    expected_before_hash: content_hash("one\n"),
                    after_content: "two\n".to_string(),
                    operation: FileOperation::Modify,
                    rename_to: None,
                }],
            )
            .unwrap();

        assert!(result.applied);
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "two\n");
        assert!(root.join(".a.txt.7.8.tmp").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn context_bound_apply_records_path_lease_and_redacted_fence() {
        let root = workspace("path-lease-bound");
        let engine = PatchEngine::open(&root).unwrap();
        fs::write(root.join("a.txt"), "one\n").unwrap();
        let context =
            PatchApplyContext::new("lease-run-1", "secret-fence-token", ["a.txt".to_string()]);

        let result = engine
            .apply_with_context(
                "pp-path-lease",
                &[PlannedPatchWrite {
                    path: "a.txt".to_string(),
                    expected_before_hash: content_hash("one\n"),
                    after_content: "two\n".to_string(),
                    operation: FileOperation::Modify,
                    rename_to: None,
                }],
                &context,
            )
            .unwrap();

        assert!(result.applied);
        assert!(
            result
                .evidence_refs
                .contains(&"patch-engine:path-lease-bound".to_string())
        );
        assert!(
            result
                .evidence_refs
                .contains(&"patch-engine:fence-token-bound".to_string())
        );
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "two\n");
        let journal =
            fs::read_to_string(root.join(journal_relative_path("pp-path-lease"))).expect("journal");
        assert!(journal.contains("\"raw_tokens_redacted\":true"));
        assert!(journal.contains("sha256:v1:"));
        assert!(!journal.contains("secret-fence-token"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn context_bound_apply_rejects_unleased_paths_without_writing() {
        let root = workspace("path-lease-missing");
        let engine = PatchEngine::open(&root).unwrap();
        fs::write(root.join("a.txt"), "one\n").unwrap();
        let context =
            PatchApplyContext::new("lease-run-2", "fence-token-2", ["other.txt".to_string()]);

        let result = engine
            .apply_with_context(
                "pp-path-lease-missing",
                &[PlannedPatchWrite {
                    path: "a.txt".to_string(),
                    expected_before_hash: content_hash("one\n"),
                    after_content: "two\n".to_string(),
                    operation: FileOperation::Modify,
                    rename_to: None,
                }],
                &context,
            )
            .unwrap();

        assert!(!result.applied);
        assert_eq!(result.reason_code, "path_lease_missing");
        assert_eq!(result.conflict_paths, vec!["a.txt".to_string()]);
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one\n");
        let journal = fs::read_to_string(root.join(journal_relative_path("pp-path-lease-missing")))
            .expect("journal");
        let rejected: serde_json::Value = serde_json::from_str(journal.trim()).unwrap();
        assert_eq!(rejected["state"], "rejected");
        assert_eq!(rejected["reason_code"], "path_lease_missing");
        assert_eq!(rejected["conflict_paths"][0], "a.txt");
        assert_eq!(rejected["context"]["leased_path_count"], 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn apply_writes_redacted_transaction_journal() {
        let root = workspace("journal-apply");
        let engine = PatchEngine::open(&root).unwrap();
        fs::write(root.join("secret.txt"), "old\n").unwrap();
        let result = engine
            .apply(
                "pp/journal secret",
                &[PlannedPatchWrite {
                    path: "secret.txt".to_string(),
                    expected_before_hash: content_hash("old\n"),
                    after_content: "new secret token\n".to_string(),
                    operation: FileOperation::Modify,
                    rename_to: None,
                }],
            )
            .unwrap();
        assert!(result.applied);
        assert!(
            result
                .evidence_refs
                .contains(&"patch-engine:journal:pp/journal secret".to_string())
        );

        let journal = fs::read_to_string(root.join(journal_relative_path("pp/journal secret")))
            .expect("journal");
        let lines = journal.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        let planned: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let applied: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(planned["schema"], PATCH_TRANSACTION_JOURNAL_SCHEMA);
        assert_eq!(planned["state"], "planned");
        assert_eq!(applied["state"], "applied");
        assert_eq!(applied["applied_files"][0], "secret.txt");
        assert!(journal.contains("sha256:v1:"));
        assert!(!journal.contains("new secret token"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_preimage_blocks_all_writes() {
        let root = workspace("stale");
        let engine = PatchEngine::open(&root).unwrap();
        fs::write(root.join("a.txt"), "new\n").unwrap();
        let result = engine
            .apply(
                "pp-stale",
                &[PlannedPatchWrite {
                    path: "a.txt".to_string(),
                    expected_before_hash: content_hash("old\n"),
                    after_content: "after\n".to_string(),
                    operation: FileOperation::Modify,
                    rename_to: None,
                }],
            )
            .unwrap();
        assert!(!result.applied);
        assert_eq!(result.reason_code, "stale_precondition");
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "new\n");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pre_apply_revalidation_blocks_toctou_and_rolls_back_prior_writes() {
        let root = workspace("toctou-revalidation");
        let engine = PatchEngine::open(&root).unwrap();
        fs::write(root.join("first.txt"), "first before\n").unwrap();
        fs::write(root.join("race.txt"), "race before\n").unwrap();

        let race_target = root.join("race.txt");
        let _hook = PreApplyHookGuard::install(Box::new(move |write| {
            if write.path == "race.txt" {
                fs::write(&race_target, "intruder\n").unwrap();
            }
        }));

        let result = engine
            .apply(
                "pp-toctou-revalidate",
                &[
                    PlannedPatchWrite {
                        path: "first.txt".to_string(),
                        expected_before_hash: content_hash("first before\n"),
                        after_content: "first after\n".to_string(),
                        operation: FileOperation::Modify,
                        rename_to: None,
                    },
                    PlannedPatchWrite {
                        path: "race.txt".to_string(),
                        expected_before_hash: content_hash("race before\n"),
                        after_content: "race after\n".to_string(),
                        operation: FileOperation::Modify,
                        rename_to: None,
                    },
                ],
            )
            .unwrap();

        assert!(!result.applied);
        assert_eq!(result.reason_code, "toctou_precondition");
        assert_eq!(result.conflict_paths, vec!["race.txt".to_string()]);
        assert!(result.rolled_back);
        assert_eq!(
            fs::read_to_string(root.join("first.txt")).unwrap(),
            "first before\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("race.txt")).unwrap(),
            "intruder\n"
        );

        let journal =
            fs::read_to_string(root.join(journal_relative_path("pp-toctou-revalidate"))).unwrap();
        let lines = journal.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        let rolled_back: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(rolled_back["state"], "rolled_back");
        assert_eq!(rolled_back["reason_code"], "toctou_precondition");
        assert_eq!(rolled_back["conflict_paths"][0], "race.txt");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn injected_apply_io_fault_rolls_back_prior_write_and_journals() {
        let root = workspace("apply-fault-rollback");
        let engine = PatchEngine::open(&root).unwrap();
        fs::write(root.join("first.txt"), "first before\n").unwrap();
        fs::write(root.join("second.txt"), "second before\n").unwrap();

        let _hook = ApplyFaultHookGuard::install(Box::new(|write| {
            if write.path == "second.txt" {
                Some(PatchEngineError::Io {
                    path: write.path.clone(),
                    stage: "test_apply_fault",
                    source: std::io::Error::other("injected apply fault"),
                })
            } else {
                None
            }
        }));

        let result = engine
            .apply(
                "pp-apply-fault-rollback",
                &[
                    PlannedPatchWrite {
                        path: "first.txt".to_string(),
                        expected_before_hash: content_hash("first before\n"),
                        after_content: "first after\n".to_string(),
                        operation: FileOperation::Modify,
                        rename_to: None,
                    },
                    PlannedPatchWrite {
                        path: "second.txt".to_string(),
                        expected_before_hash: content_hash("second before\n"),
                        after_content: "second after\n".to_string(),
                        operation: FileOperation::Modify,
                        rename_to: None,
                    },
                ],
            )
            .unwrap();

        assert!(!result.applied);
        assert!(result.rolled_back);
        assert_eq!(result.reason_code, "io_rolled_back");
        assert!(
            result
                .evidence_refs
                .contains(&"patch-engine:rollback-attempted".to_string())
        );
        assert_eq!(
            fs::read_to_string(root.join("first.txt")).unwrap(),
            "first before\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("second.txt")).unwrap(),
            "second before\n"
        );

        let journal =
            fs::read_to_string(root.join(journal_relative_path("pp-apply-fault-rollback")))
                .expect("journal");
        let lines = journal.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        let rolled_back: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(rolled_back["state"], "rolled_back");
        assert_eq!(rolled_back["reason_code"], "io_rolled_back");
        assert_eq!(rolled_back["rolled_back"], true);
        assert_eq!(rolled_back["applied_files"][0], "first.txt");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovery_report_tracks_repeated_attempts_and_latest_terminal_state() {
        let root = workspace("attempt-recovery");
        let engine = PatchEngine::open(&root).unwrap();
        fs::write(root.join("a.txt"), "current\n").unwrap();

        let rejected = engine
            .apply(
                "pp-attempt-recovery",
                &[PlannedPatchWrite {
                    path: "a.txt".to_string(),
                    expected_before_hash: content_hash("old\n"),
                    after_content: "ignored\n".to_string(),
                    operation: FileOperation::Modify,
                    rename_to: None,
                }],
            )
            .unwrap();
        assert!(!rejected.applied);
        assert_eq!(rejected.reason_code, "stale_precondition");

        let applied = engine
            .apply(
                "pp-attempt-recovery",
                &[PlannedPatchWrite {
                    path: "a.txt".to_string(),
                    expected_before_hash: content_hash("current\n"),
                    after_content: "after\n".to_string(),
                    operation: FileOperation::Modify,
                    rename_to: None,
                }],
            )
            .unwrap();
        assert!(applied.applied);

        let report = engine.recovery_report("pp-attempt-recovery").unwrap();
        assert_eq!(report.reason_code, "applied");
        assert!(!report.requires_operator_review);
        assert_eq!(report.attempts.len(), 2);
        assert_eq!(report.attempts[0].attempt_index, 1);
        assert_eq!(report.attempts[0].attempt_id, "attempt-1");
        assert_eq!(report.attempts[0].state, "rejected");
        assert_eq!(report.attempts[0].reason_code, "stale_precondition");
        assert_eq!(report.attempts[0].journal_events, 1);
        assert_eq!(report.attempts[1].attempt_index, 2);
        assert_eq!(report.attempts[1].attempt_id, "attempt-2");
        assert_eq!(report.attempts[1].state, "applied");
        assert_eq!(report.attempts[1].reason_code, "applied");
        assert_eq!(report.attempts[1].applied_files, vec!["a.txt".to_string()]);
        assert_eq!(report.attempts[1].journal_events, 2);
        assert_eq!(
            report
                .latest_attempt
                .as_ref()
                .map(|attempt| attempt.attempt_index),
            Some(2)
        );

        let journal =
            fs::read_to_string(root.join(journal_relative_path("pp-attempt-recovery"))).unwrap();
        assert!(journal.contains("\"attempt_index\":1"));
        assert!(journal.contains("\"attempt_id\":\"attempt-2\""));
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "after\n");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejected_precondition_is_journaled_without_writing() {
        let root = workspace("journal-reject");
        let engine = PatchEngine::open(&root).unwrap();
        fs::write(root.join("a.txt"), "new\n").unwrap();
        let result = engine
            .apply(
                "pp-stale-journal",
                &[PlannedPatchWrite {
                    path: "a.txt".to_string(),
                    expected_before_hash: content_hash("old\n"),
                    after_content: "after\n".to_string(),
                    operation: FileOperation::Modify,
                    rename_to: None,
                }],
            )
            .unwrap();
        assert!(!result.applied);
        let journal =
            fs::read_to_string(root.join(journal_relative_path("pp-stale-journal"))).unwrap();
        let rejected: serde_json::Value = serde_json::from_str(journal.trim()).unwrap();
        assert_eq!(rejected["state"], "rejected");
        assert_eq!(rejected["reason_code"], "stale_precondition");
        assert_eq!(rejected["conflict_paths"][0], "a.txt");
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "new\n");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn create_does_not_overwrite_existing_file() {
        let root = workspace("create-existing");
        let engine = PatchEngine::open(&root).unwrap();
        fs::write(root.join("a.txt"), "already here\n").unwrap();
        let result = engine
            .apply(
                "pp-create-existing",
                &[PlannedPatchWrite {
                    path: "a.txt".to_string(),
                    expected_before_hash: content_hash("already here\n"),
                    after_content: "replacement\n".to_string(),
                    operation: FileOperation::Create,
                    rename_to: None,
                }],
            )
            .unwrap();
        assert!(!result.applied);
        assert_eq!(result.reason_code, "operation_precondition");
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "already here\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn modify_does_not_create_missing_file() {
        let root = workspace("modify-missing");
        let engine = PatchEngine::open(&root).unwrap();
        let result = engine
            .apply(
                "pp-modify-missing",
                &[PlannedPatchWrite {
                    path: "missing.txt".to_string(),
                    expected_before_hash: content_hash(""),
                    after_content: "new\n".to_string(),
                    operation: FileOperation::Modify,
                    rename_to: None,
                }],
            )
            .unwrap();
        assert!(!result.applied);
        assert_eq!(result.reason_code, "operation_precondition");
        assert!(!root.join("missing.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delete_and_rename_apply_transactionally() {
        let root = workspace("delete-rename");
        let engine = PatchEngine::open(&root).unwrap();
        fs::write(root.join("a.txt"), "one\n").unwrap();
        fs::write(root.join("old.txt"), "two\n").unwrap();
        let result = engine
            .apply(
                "pp-delete-rename",
                &[
                    PlannedPatchWrite {
                        path: "a.txt".to_string(),
                        expected_before_hash: content_hash("one\n"),
                        after_content: String::new(),
                        operation: FileOperation::Delete,
                        rename_to: None,
                    },
                    PlannedPatchWrite {
                        path: "old.txt".to_string(),
                        expected_before_hash: content_hash("two\n"),
                        after_content: String::new(),
                        operation: FileOperation::Rename,
                        rename_to: Some("nested/new.txt".to_string()),
                    },
                ],
            )
            .unwrap();
        assert!(result.applied);
        assert_eq!(result.reason_code, "applied");
        assert!(!root.join("a.txt").exists());
        assert!(!root.join("old.txt").exists());
        assert_eq!(
            fs::read_to_string(root.join("nested/new.txt")).unwrap(),
            "two\n"
        );
        assert!(result.applied_files.contains(&"a.txt".to_string()));
        assert!(result.applied_files.contains(&"old.txt".to_string()));
        assert!(result.applied_files.contains(&"nested/new.txt".to_string()));

        let journal =
            fs::read_to_string(root.join(journal_relative_path("pp-delete-rename"))).unwrap();
        assert!(journal.contains("\"operation\":\"delete\""));
        assert!(journal.contains("\"operation\":\"rename\""));
        assert!(journal.contains("\"rename_to\":\"nested/new.txt\""));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rename_requires_missing_destination() {
        let root = workspace("rename-dest-exists");
        let engine = PatchEngine::open(&root).unwrap();
        fs::write(root.join("old.txt"), "two\n").unwrap();
        fs::write(root.join("new.txt"), "already\n").unwrap();
        let result = engine
            .apply(
                "pp-rename-conflict",
                &[PlannedPatchWrite {
                    path: "old.txt".to_string(),
                    expected_before_hash: content_hash("two\n"),
                    after_content: String::new(),
                    operation: FileOperation::Rename,
                    rename_to: Some("new.txt".to_string()),
                }],
            )
            .unwrap();
        assert!(!result.applied);
        assert_eq!(result.reason_code, "operation_precondition");
        assert_eq!(fs::read_to_string(root.join("old.txt")).unwrap(), "two\n");
        assert_eq!(
            fs::read_to_string(root.join("new.txt")).unwrap(),
            "already\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn non_utf8_preflight_is_typed_error_and_never_overwritten_as_missing() {
        let root = workspace("non-utf8-preflight");
        fs::write(root.join("binary.bin"), [0xff, 0xfe, 0xfd]).unwrap();
        let engine = PatchEngine::open(&root).unwrap();
        let error = engine
            .apply(
                "pp-non-utf8",
                &[PlannedPatchWrite {
                    path: "binary.bin".to_string(),
                    expected_before_hash: content_hash(""),
                    after_content: "replacement\n".to_string(),
                    operation: FileOperation::Create,
                    rename_to: None,
                }],
            )
            .unwrap_err();
        assert!(matches!(
            error,
            PatchEngineError::Io {
                path,
                stage: "preflight_read",
                source: _
            } if path == "binary.bin"
        ));
        assert_eq!(
            fs::read(root.join("binary.bin")).unwrap(),
            [0xff, 0xfe, 0xfd]
        );
        assert!(!root.join(journal_relative_path("pp-non-utf8")).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        use std::os::unix::fs::symlink;
        let root = workspace("symlink");
        let outside =
            std::env::temp_dir().join(format!("opensks-patch-outside-{}", std::process::id()));
        fs::write(&outside, "outside").unwrap();
        symlink(&outside, root.join("link.txt")).unwrap();
        let engine = PatchEngine::open(&root).unwrap();
        let error = engine.resolve("link.txt").unwrap_err();
        assert!(matches!(error, PatchEngineError::SymlinkRejected(_)));
        let _ = fs::remove_file(outside);
        let _ = fs::remove_dir_all(root);
    }
}
