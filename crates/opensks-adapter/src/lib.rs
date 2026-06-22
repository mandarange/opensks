//! Agent adapter abstraction + a real local test adapter (recovery release §6.4).
//!
//! The headline defect of the baseline is that a conversation turn dispatches a
//! `DeterministicWorker` that returns `ok: true` without doing any work. This
//! crate replaces that pattern with a real boundary:
//!
//! * [`AgentAdapter`] — what the engine drives. An adapter streams typed
//!   [`AgentEventEnvelope`]s and returns an [`AgentRunOutcome`].
//! * [`LocalTestAdapter`] — a *deterministic but genuinely real* adapter. Unlike
//!   the forbidden no-op worker, it performs actual file IO inside the request
//!   workspace, produces a real [`PatchProposal`]/[`PatchApplyResult`] with
//!   correct pre/post-image hashes, and reports an honest terminal state. It
//!   needs no model credentials, so the Chat → real-code-edit vertical is
//!   provable in tests.
//!
//! No model adapter lives here — that is the credentialed work deferred to the
//! release gate. The local test adapter is what keeps the vertical honest and
//! exercised in CI.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use opensks_contracts::projection::RunProjectionState;
use opensks_contracts::{
    AGENT_ADAPTER_DESCRIPTOR_SCHEMA, AGENT_EVENT_ENVELOPE_SCHEMA, AgentAdapterDescriptor,
    AgentAdapterKind, AgentEventEnvelope, AgentEventKind, FileOperation, FilePatch,
    PATCH_APPLY_RESULT_SCHEMA, PATCH_PROPOSAL_SCHEMA, PatchApplyResult, PatchProposal, RiskLevel,
    Sensitivity,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum AgentAdapterError {
    #[error("io error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("path `{0}` escapes the workspace or is not workspace-relative")]
    PathEscape(String),
    #[error("invalid instruction: {0}")]
    InvalidInstruction(String),
}

/// Everything an adapter needs to perform one turn. Timestamps are passed in so
/// runs are deterministic and testable.
#[derive(Debug, Clone)]
pub struct AgentRunRequest {
    pub workspace: PathBuf,
    pub project_id: String,
    pub conversation_id: String,
    pub turn_id: String,
    pub run_id: String,
    pub stream_id: String,
    pub now_ms: u64,
    /// The user prompt. For [`LocalTestAdapter`] this may carry a structured
    /// instruction (see [`LocalTestInstruction`]).
    pub prompt: String,
}

/// Receives the typed events an adapter emits. Implementations dedup/persist by
/// `sequence`. A run never completes by silence — completion is signalled by an
/// explicit terminal event + the returned outcome.
pub trait AgentEventSink {
    fn emit(&self, event: AgentEventEnvelope);
}

/// A sink that records every event (for tests and synchronous consumers).
#[derive(Debug, Default)]
pub struct CollectingSink {
    events: std::sync::Mutex<Vec<AgentEventEnvelope>>,
}

impl CollectingSink {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn events(&self) -> Vec<AgentEventEnvelope> {
        self.events.lock().expect("sink lock").clone()
    }
    pub fn kinds(&self) -> Vec<AgentEventKind> {
        self.events().into_iter().map(|e| e.kind).collect()
    }
}

impl AgentEventSink for CollectingSink {
    fn emit(&self, event: AgentEventEnvelope) {
        self.events.lock().expect("sink lock").push(event);
    }
}

/// The honest result of a run.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRunOutcome {
    pub assistant_text: String,
    pub patches: Vec<PatchProposal>,
    pub apply_results: Vec<PatchApplyResult>,
    pub final_state: RunProjectionState,
}

/// The engine-facing adapter contract.
pub trait AgentAdapter {
    fn descriptor(&self) -> AgentAdapterDescriptor;

    /// Run one turn, streaming events into `sink`, and return the outcome.
    fn run(
        &self,
        request: &AgentRunRequest,
        sink: &dyn AgentEventSink,
    ) -> Result<AgentRunOutcome, AgentAdapterError>;
}

/// A structured, deterministic instruction the local test adapter can execute.
/// Carried as JSON in the prompt under a `local_test` key, e.g.
/// `{"local_test": {"op": "append_line", "path": "NOTES.md", "value": "hi"}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LocalTestInstruction {
    /// Create a new file (fails if it already exists).
    CreateFile { path: String, value: String },
    /// Replace the entire content of an existing file.
    ReplaceContent { path: String, value: String },
    /// Append a line to a file (creating it if absent).
    AppendLine { path: String, value: String },
}

#[derive(Debug, Deserialize)]
struct LocalTestEnvelope {
    local_test: LocalTestInstruction,
}

impl LocalTestInstruction {
    /// Parse a structured instruction from a prompt, if present.
    pub fn from_prompt(prompt: &str) -> Option<Self> {
        serde_json::from_str::<LocalTestEnvelope>(prompt)
            .ok()
            .map(|e| e.local_test)
    }

    fn path(&self) -> &str {
        match self {
            Self::CreateFile { path, .. }
            | Self::ReplaceContent { path, .. }
            | Self::AppendLine { path, .. } => path,
        }
    }
}

/// A deterministic adapter that performs real file edits. Use in tests and the
/// zero-model "simulation" lane — never presented to the user as a live model.
#[derive(Debug, Default, Clone)]
pub struct LocalTestAdapter;

impl LocalTestAdapter {
    pub fn new() -> Self {
        Self
    }

    fn emit(
        &self,
        sink: &dyn AgentEventSink,
        request: &AgentRunRequest,
        sequence: &mut u64,
        kind: AgentEventKind,
        payload: serde_json::Value,
    ) {
        let seq = *sequence;
        *sequence += 1;
        sink.emit(AgentEventEnvelope {
            schema: AGENT_EVENT_ENVELOPE_SCHEMA.to_string(),
            stream_id: request.stream_id.clone(),
            project_id: request.project_id.clone(),
            conversation_id: request.conversation_id.clone(),
            turn_id: request.turn_id.clone(),
            run_id: request.run_id.clone(),
            worker_id: Some("local-test".to_string()),
            node_id: None,
            sequence: seq,
            occurred_at_ms: request.now_ms,
            kind,
            payload,
            sensitivity: Sensitivity::Internal,
            evidence_refs: vec![],
        });
    }
}

impl AgentAdapter for LocalTestAdapter {
    fn descriptor(&self) -> AgentAdapterDescriptor {
        AgentAdapterDescriptor {
            schema: AGENT_ADAPTER_DESCRIPTOR_SCHEMA.to_string(),
            adapter_id: "local-test".to_string(),
            display_name: "Local test adapter".to_string(),
            kind: AgentAdapterKind::LocalTest,
            supports_streaming: true,
            supports_tools: true,
            supports_resume: false,
            supports_parallel_sessions: true,
            supported_reasoning_efforts: vec![],
        }
    }

    fn run(
        &self,
        request: &AgentRunRequest,
        sink: &dyn AgentEventSink,
    ) -> Result<AgentRunOutcome, AgentAdapterError> {
        let mut sequence: u64 = 0;

        let Some(instruction) = LocalTestInstruction::from_prompt(&request.prompt) else {
            // No actionable instruction: answer honestly, edit nothing.
            self.emit(
                sink,
                request,
                &mut sequence,
                AgentEventKind::AssistantTextCompleted,
                serde_json::json!({
                    "text": "No structured local-test instruction was provided; nothing was changed."
                }),
            );
            return Ok(AgentRunOutcome {
                assistant_text:
                    "No structured local-test instruction was provided; nothing was changed."
                        .to_string(),
                patches: vec![],
                apply_results: vec![],
                final_state: RunProjectionState::Completed,
            });
        };

        let rel = instruction.path().to_string();
        let abs = resolve_in_workspace(&request.workspace, &rel)?;

        self.emit(
            sink,
            request,
            &mut sequence,
            AgentEventKind::PlanUpdated,
            serde_json::json!({ "steps": [format!("edit {rel}")] }),
        );

        // Read pre-image.
        let before = std::fs::read_to_string(&abs).unwrap_or_default();
        self.emit(
            sink,
            request,
            &mut sequence,
            AgentEventKind::ToolCallStarted,
            serde_json::json!({ "tool": "files.read", "path": rel }),
        );

        let (after, operation) = match &instruction {
            LocalTestInstruction::CreateFile { value, .. } => {
                if abs.exists() {
                    return Err(AgentAdapterError::InvalidInstruction(format!(
                        "create_file: `{rel}` already exists"
                    )));
                }
                (value.clone(), FileOperation::Create)
            }
            LocalTestInstruction::ReplaceContent { value, .. } => {
                (value.clone(), FileOperation::Modify)
            }
            LocalTestInstruction::AppendLine { value, .. } => {
                let mut next = before.clone();
                if !next.is_empty() && !next.ends_with('\n') {
                    next.push('\n');
                }
                next.push_str(value);
                next.push('\n');
                let op = if before.is_empty() && !abs.exists() {
                    FileOperation::Create
                } else {
                    FileOperation::Modify
                };
                (next, op)
            }
        };

        let before_hash = content_hash(&before);
        let after_hash = content_hash(&after);

        let proposal = PatchProposal {
            schema: PATCH_PROPOSAL_SCHEMA.to_string(),
            proposal_id: format!("pp-{}", request.run_id),
            run_id: request.run_id.clone(),
            worker_id: "local-test".to_string(),
            base_commit: None,
            base_tree_hash: before_hash.clone(),
            files: vec![FilePatch {
                path: rel.clone(),
                before_hash: before_hash.clone(),
                after_hash: after_hash.clone(),
                unified_diff: minimal_unified_diff(&rel, &before, &after),
                operation,
            }],
            requirement_ids: vec![],
            rationale_summary: format!("Local test edit of {rel}"),
            risk_level: RiskLevel::Low,
            evidence_refs: vec![format!("adapter:local-test:{}", request.run_id)],
        };

        self.emit(
            sink,
            request,
            &mut sequence,
            AgentEventKind::FilePatchProposed,
            serde_json::to_value(&proposal).unwrap_or(serde_json::Value::Null),
        );

        // Apply through the transactional writer: it re-validates the on-disk
        // pre-image against before_hash (no silent overwrite of a file that
        // changed since it was read — §10.5) and rolls back on any IO failure.
        let apply = apply_file_writes(
            &request.workspace,
            &proposal.proposal_id,
            &[PlannedWrite {
                path: rel.clone(),
                expected_before_hash: before_hash.clone(),
                after_content: after.clone(),
                operation,
            }],
        )?;

        self.emit(
            sink,
            request,
            &mut sequence,
            AgentEventKind::FilePatchApplied,
            serde_json::to_value(&apply).unwrap_or(serde_json::Value::Null),
        );

        let (assistant_text, final_state) = if apply.applied {
            (
                format!("Edited `{rel}` (1 file changed)."),
                RunProjectionState::Completed,
            )
        } else {
            (
                format!("Could not edit `{rel}`: it changed on disk since it was read."),
                RunProjectionState::Failed,
            )
        };
        self.emit(
            sink,
            request,
            &mut sequence,
            AgentEventKind::AssistantTextCompleted,
            serde_json::json!({ "text": assistant_text }),
        );

        Ok(AgentRunOutcome {
            assistant_text,
            patches: vec![proposal],
            apply_results: vec![apply],
            final_state,
        })
    }
}

/// A single planned file write with the pre-image hash it expects on disk.
#[derive(Debug, Clone)]
pub struct PlannedWrite {
    pub path: String,
    pub expected_before_hash: String,
    pub after_content: String,
    pub operation: FileOperation,
}

/// Apply a set of file writes as a transaction (recovery directive §10.4/§10.5):
///
/// 1. **Validate** every target's on-disk pre-image against
///    `expected_before_hash`. If any file changed since it was read, nothing is
///    written and the conflicting paths are returned — no silent overwrite.
/// 2. **Apply with rollback**: snapshot each target's original content, then
///    write all of them. Any IO failure restores every target to its pre-image
///    so a multi-file patch never lands half-applied.
///
/// A path that escapes the workspace is a hard error (`PathEscape`).
pub fn apply_file_writes(
    workspace: &Path,
    proposal_id: &str,
    writes: &[PlannedWrite],
) -> Result<PatchApplyResult, AgentAdapterError> {
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
        evidence_refs: vec![],
    };

    // Phase 1: resolve + validate pre-images.
    let mut targets: Vec<(String, PathBuf, Option<String>)> = Vec::with_capacity(writes.len());
    let mut conflicts = Vec::new();
    for w in writes {
        let abs = resolve_in_workspace(workspace, &w.path)?;
        let current = std::fs::read_to_string(&abs).ok();
        let current_hash = content_hash(current.as_deref().unwrap_or(""));
        if current_hash != w.expected_before_hash {
            conflicts.push(w.path.clone());
        }
        targets.push((w.path.clone(), abs, current));
    }
    if !conflicts.is_empty() {
        return Ok(result(
            false,
            vec![],
            conflicts,
            false,
            "stale_precondition",
        ));
    }

    // Phase 2: apply, rolling every target back on the first failure.
    let mut applied_files = Vec::new();
    for (index, w) in writes.iter().enumerate() {
        let (_, abs, _) = &targets[index];
        let attempt = (|| -> std::io::Result<()> {
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(abs, w.after_content.as_bytes())
        })();
        if attempt.is_err() {
            rollback(&targets[..=index]);
            return Ok(result(false, vec![], vec![], true, "io_rolled_back"));
        }
        applied_files.push(w.path.clone());
    }
    Ok(result(true, applied_files, vec![], false, "applied"))
}

/// Restore each target to its pre-image: rewrite the snapshot, or remove the
/// file if it did not exist before.
fn rollback(targets: &[(String, PathBuf, Option<String>)]) {
    for (_, abs, snapshot) in targets {
        match snapshot {
            Some(original) => {
                let _ = std::fs::write(abs, original.as_bytes());
            }
            None => {
                let _ = std::fs::remove_file(abs);
            }
        }
    }
}

/// Resolve a workspace-relative path, rejecting absolute paths and any `..`
/// traversal (recovery directive §19.6 — no path escape).
fn resolve_in_workspace(workspace: &Path, rel: &str) -> Result<PathBuf, AgentAdapterError> {
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        return Err(AgentAdapterError::PathEscape(rel.to_string()));
    }
    for component in candidate.components() {
        use std::path::Component;
        match component {
            Component::Normal(_) | Component::CurDir => {}
            _ => return Err(AgentAdapterError::PathEscape(rel.to_string())),
        }
    }
    Ok(workspace.join(candidate))
}

/// A stable, non-cryptographic content hash. Sufficient for the test adapter's
/// change detection; the production file service uses a real digest.
fn content_hash(content: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    format!("h{:016x}", hasher.finish())
}

/// A minimal unified-diff-ish body for review/display. Not a full diff engine —
/// the production path computes a real unified diff.
fn minimal_unified_diff(path: &str, before: &str, after: &str) -> String {
    format!(
        "--- a/{path}\n+++ b/{path}\n@@ before {} bytes @@\n@@ after {} bytes @@\n",
        before.len(),
        after.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("opensks-adapter-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn request(workspace: &Path, prompt: &str) -> AgentRunRequest {
        AgentRunRequest {
            workspace: workspace.to_path_buf(),
            project_id: "p1".to_string(),
            conversation_id: "c1".to_string(),
            turn_id: "t1".to_string(),
            run_id: "r1".to_string(),
            stream_id: "s1".to_string(),
            now_ms: 1000,
            prompt: prompt.to_string(),
        }
    }

    #[test]
    fn local_test_adapter_really_edits_a_file_on_disk() {
        let ws = temp_workspace("edit");
        let file = ws.join("NOTES.md");
        std::fs::write(&file, "first line\n").unwrap();

        let adapter = LocalTestAdapter::new();
        let sink = CollectingSink::new();
        let prompt =
            r#"{"local_test": {"op": "append_line", "path": "NOTES.md", "value": "second line"}}"#;
        let outcome = adapter.run(&request(&ws, prompt), &sink).unwrap();

        // The real side effect: the file on disk actually changed.
        let after = std::fs::read_to_string(&file).unwrap();
        assert_eq!(after, "first line\nsecond line\n");
        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        assert_eq!(outcome.patches.len(), 1);
        assert_eq!(outcome.apply_results.len(), 1);
        assert!(outcome.apply_results[0].applied);
        assert_ne!(
            outcome.patches[0].files[0].before_hash,
            outcome.patches[0].files[0].after_hash
        );

        // The stream carries explicit, ordered, terminal events.
        let kinds = sink.kinds();
        assert!(kinds.contains(&AgentEventKind::FilePatchApplied));
        assert_eq!(kinds.last(), Some(&AgentEventKind::AssistantTextCompleted));
        let seqs: Vec<u64> = sink.events().into_iter().map(|e| e.sequence).collect();
        assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<_>>());

        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn create_file_writes_new_content() {
        let ws = temp_workspace("create");
        let adapter = LocalTestAdapter::new();
        let sink = CollectingSink::new();
        let prompt =
            r#"{"local_test": {"op": "create_file", "path": "sub/new.txt", "value": "hello"}}"#;
        adapter.run(&request(&ws, prompt), &sink).unwrap();
        assert_eq!(
            std::fs::read_to_string(ws.join("sub/new.txt")).unwrap(),
            "hello"
        );
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn path_traversal_is_rejected() {
        let ws = temp_workspace("escape");
        let adapter = LocalTestAdapter::new();
        let sink = CollectingSink::new();
        let prompt =
            r#"{"local_test": {"op": "replace_content", "path": "../escape.txt", "value": "x"}}"#;
        let err = adapter.run(&request(&ws, prompt), &sink).unwrap_err();
        assert!(matches!(err, AgentAdapterError::PathEscape(_)));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn no_instruction_edits_nothing() {
        let ws = temp_workspace("noop");
        let adapter = LocalTestAdapter::new();
        let sink = CollectingSink::new();
        let outcome = adapter
            .run(&request(&ws, "just chat, no json"), &sink)
            .unwrap();
        assert!(outcome.patches.is_empty());
        assert!(outcome.apply_results.is_empty());
        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn transaction_rejects_stale_precondition_without_writing() {
        // §10.5: a file that changed since it was read is NOT silently
        // overwritten — the write is refused and the original is preserved.
        let ws = temp_workspace("tx-stale");
        let file = ws.join("f.txt");
        std::fs::write(&file, "original").unwrap();
        let writes = [PlannedWrite {
            path: "f.txt".to_string(),
            expected_before_hash: "h_does_not_match".to_string(),
            after_content: "new".to_string(),
            operation: FileOperation::Modify,
        }];
        let res = apply_file_writes(&ws, "pp-1", &writes).unwrap();
        assert!(!res.applied);
        assert_eq!(res.conflict_paths, vec!["f.txt".to_string()]);
        assert_eq!(res.reason_code, "stale_precondition");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "original",
            "file must be untouched on a precondition conflict"
        );
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn transaction_applies_multiple_files_when_preimages_match() {
        let ws = temp_workspace("tx-multi");
        std::fs::write(ws.join("a.txt"), "A").unwrap();
        let writes = [
            PlannedWrite {
                path: "a.txt".to_string(),
                expected_before_hash: content_hash("A"),
                after_content: "A2".to_string(),
                operation: FileOperation::Modify,
            },
            PlannedWrite {
                // b.txt is absent → its pre-image is the hash of "".
                path: "nested/b.txt".to_string(),
                expected_before_hash: content_hash(""),
                after_content: "B".to_string(),
                operation: FileOperation::Create,
            },
        ];
        let res = apply_file_writes(&ws, "pp-2", &writes).unwrap();
        assert!(res.applied);
        assert_eq!(res.applied_files.len(), 2);
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "A2");
        assert_eq!(
            std::fs::read_to_string(ws.join("nested/b.txt")).unwrap(),
            "B"
        );
        std::fs::remove_dir_all(&ws).ok();
    }
}
