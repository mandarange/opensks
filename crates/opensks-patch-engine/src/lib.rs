//! Commercial patch engine foundation.
//!
//! This crate is the single owner for applying workspace file writes produced by
//! agent workers. It gives adapter/testkit callers a safer bridge while the full
//! isolated-worktree PatchEngine matures: canonical workspace anchoring,
//! symlink-aware containment, SHA-256 content identity, pre-image validation,
//! same-directory temp writes with fsync, atomic rename, directory fsync, and
//! typed rollback receipts.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use opensks_contracts::{FileOperation, PATCH_APPLY_RESULT_SCHEMA, PatchApplyResult};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;

const PATCH_TRANSACTION_JOURNAL_SCHEMA: &str = "opensks.patch-transaction-journal.v1";

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
    #[error("failed to serialize patch transaction journal: {0}")]
    JournalSerialize(String),
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
pub struct PatchEngine {
    root: PathBuf,
}

struct PatchJournalEvent<'a> {
    proposal_id: &'a str,
    sequence: u64,
    state: &'a str,
    writes: &'a [PlannedPatchWrite],
    applied_files: &'a [String],
    conflict_paths: &'a [String],
    rolled_back: bool,
    reason_code: &'a str,
}

#[derive(Debug, Clone)]
struct PreparedPatchWrite {
    path: String,
    target: PathBuf,
    before: Option<String>,
    operation: FileOperation,
    rename_to: Option<(String, PathBuf)>,
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
        Ok(Self { root })
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
        let journal_ref = format!("patch-engine:journal:{proposal_id}");
        let result = |applied: bool,
                      applied_files: Vec<String>,
                      conflict_paths: Vec<String>,
                      rolled_back: bool,
                      reason: &str| PatchApplyResult {
            schema: PATCH_APPLY_RESULT_SCHEMA.to_string(),
            proposal_id: proposal_id.to_string(),
            applied,
            applied_files,
            conflict_paths,
            rolled_back,
            reason_code: reason.to_string(),
            evidence_refs: vec!["patch-engine:atomic-apply".to_string(), journal_ref.clone()],
        };

        let mut targets = Vec::with_capacity(writes.len());
        let mut conflicts = Vec::new();
        let mut operation_conflicts = Vec::new();
        let mut touched_paths = std::collections::BTreeSet::new();
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
            let current = fs::read_to_string(&target).ok();
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
        if !operation_conflicts.is_empty() {
            self.append_journal_event(PatchJournalEvent {
                proposal_id,
                sequence: 0,
                state: "rejected",
                writes,
                applied_files: &[],
                conflict_paths: &operation_conflicts,
                rolled_back: false,
                reason_code: "operation_precondition",
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
                sequence: 0,
                state: "rejected",
                writes,
                applied_files: &[],
                conflict_paths: &conflicts,
                rolled_back: false,
                reason_code: "stale_precondition",
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
            sequence: 0,
            state: "planned",
            writes,
            applied_files: &[],
            conflict_paths: &[],
            rolled_back: false,
            reason_code: "preflight_ok",
        })?;
        let mut applied_files = Vec::new();
        for (idx, write) in writes.iter().enumerate() {
            let prepared = &targets[idx];
            let apply_result = match write.operation {
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
            };
            if apply_result.is_err() {
                let rolled_back = self.rollback(&targets[..idx]).is_ok();
                self.append_journal_event(PatchJournalEvent {
                    proposal_id,
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
                })?;
                return if rolled_back {
                    Ok(result(false, vec![], vec![], true, "io_rolled_back"))
                } else {
                    Ok(result(
                        false,
                        vec![],
                        vec![],
                        true,
                        "rollback_failed_critical",
                    ))
                };
            }
            applied_files.extend(applied_paths(prepared));
        }

        self.append_journal_event(PatchJournalEvent {
            proposal_id,
            sequence: 1,
            state: "applied",
            writes,
            applied_files: &applied_files,
            conflict_paths: &[],
            rolled_back: false,
            reason_code: "applied",
        })?;
        Ok(result(true, applied_files, vec![], false, "applied"))
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
        let event = json!({
            "schema": PATCH_TRANSACTION_JOURNAL_SCHEMA,
            "proposal_id": event.proposal_id,
            "sequence": event.sequence,
            "state": event.state,
            "reason_code": event.reason_code,
            "rolled_back": event.rolled_back,
            "files": files,
            "applied_files": event.applied_files,
            "conflict_paths": event.conflict_paths,
            "content_redacted": true,
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
