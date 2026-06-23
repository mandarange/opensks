//! Agent adapter abstraction + a real local test adapter (recovery release §6.4).
//!
//! The headline defect of the baseline is that a conversation turn dispatches a
//! `DeterministicWorker` that returns `ok: true` without doing any work. This
//! crate replaces that pattern with a real boundary:
//!
//! * [`AgentAdapter`] — what the engine drives. An adapter streams typed
//!   [`AgentEventEnvelope`]s and returns an [`AgentRunOutcome`].
//! * `LocalTestAdapter` — an explicit deterministic simulation adapter. It
//!   performs real file IO only when a caller provides a structured local-test
//!   instruction; ordinary product chat must surface setup-required instead of
//!   silently falling back to this adapter. It is compiled only for tests or the
//!   explicit `simulation` feature.
//!
//! No model adapter lives here — that is the credentialed work deferred to the
//! release gate. The local test adapter is what keeps the vertical honest and
//! exercised in CI.

use std::path::{Path, PathBuf};

use opensks_contracts::projection::RunProjectionState;
#[cfg(any(test, feature = "simulation"))]
use opensks_contracts::{
    AGENT_ADAPTER_DESCRIPTOR_SCHEMA, AGENT_EVENT_ENVELOPE_SCHEMA, AgentAdapterKind, FilePatch,
    PATCH_PROPOSAL_SCHEMA, RiskLevel, Sensitivity,
};
use opensks_contracts::{
    AgentAdapterDescriptor, AgentEventEnvelope, AgentEventKind, FileOperation,
    PATCH_APPLY_RESULT_SCHEMA, PatchApplyResult, PatchProposal,
};
use opensks_patch_engine::{PatchEngine, PlannedPatchWrite};
use serde::{Deserialize, Serialize};

pub mod agentic;
pub mod openrouter;
pub use agentic::{
    AgentStep, AgenticConfig, FnDriver, SequenceDriver, ToolCall, ToolDriver, ToolResult,
    run_agentic_loop,
};
pub use openrouter::{
    ChatCompleter, NativeHttpChatCompleter, OpenRouterAdapter, OpenRouterToolDriver, parse_step,
    tool_definitions,
};

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
    #[error("missing API key in env var `{0}`")]
    MissingApiKey(String),
    #[error("provider call failed: {0}")]
    Provider(String),
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
    /// Run a MULTI-STEP agentic edit: each tool call is one loop step, executed
    /// against the workspace with its result fed back, applied transactionally
    /// (recovery release §6/§8). This reaches the real [`agentic::run_agentic_loop`]
    /// from a single conversation turn, so coordinated multi-file edits are real and
    /// reachable — not just a single-shot op.
    Steps { steps: Vec<agentic::ToolCall> },
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
            // A multi-step edit has no single path; it is handled by the agentic
            // loop, never by the single-file planner below.
            Self::Steps { .. } => "",
        }
    }
}

/// A deterministic adapter that performs real file edits. Use in tests and the
/// explicit "simulation" lane — never presented to the user as a live model.
#[cfg(any(test, feature = "simulation"))]
#[derive(Debug, Default, Clone)]
pub struct LocalTestAdapter;

#[cfg(any(test, feature = "simulation"))]
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

#[cfg(any(test, feature = "simulation"))]
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

        // A multi-step instruction drives the REAL agentic loop: each tool call is
        // one loop step, applied transactionally with its result fed back.
        if let LocalTestInstruction::Steps { steps } = &instruction {
            let groups: Vec<Vec<agentic::ToolCall>> =
                steps.iter().cloned().map(|call| vec![call]).collect();
            let count = groups.len();
            let mut driver =
                agentic::SequenceDriver::new(groups, format!("Completed {count} agentic step(s)."));
            return agentic::run_agentic_loop(
                request,
                &mut driver,
                &agentic::AgenticConfig::default(),
                sink,
            );
        }

        // Pure planning (no writes) — shared with the parallel runtime.
        let (rel, before, after, operation) = plan_local_edit(&request.workspace, &instruction)?;

        self.emit(
            sink,
            request,
            &mut sequence,
            AgentEventKind::PlanUpdated,
            serde_json::json!({ "steps": [format!("edit {rel}")] }),
        );
        self.emit(
            sink,
            request,
            &mut sequence,
            AgentEventKind::ToolCallStarted,
            serde_json::json!({ "tool": "files.read", "path": rel }),
        );

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
    let engine = PatchEngine::open(workspace).map_err(|error| match error {
        opensks_patch_engine::PatchEngineError::PathEscape(path)
        | opensks_patch_engine::PatchEngineError::SymlinkRejected(path) => {
            AgentAdapterError::PathEscape(path)
        }
        other => AgentAdapterError::InvalidInstruction(other.to_string()),
    })?;
    let planned: Vec<PlannedPatchWrite> = writes
        .iter()
        .map(|write| PlannedPatchWrite {
            path: write.path.clone(),
            expected_before_hash: write.expected_before_hash.clone(),
            after_content: write.after_content.clone(),
            operation: write.operation,
            rename_to: None,
        })
        .collect();
    engine
        .apply(proposal_id, &planned)
        .map_err(|error| match error {
            opensks_patch_engine::PatchEngineError::PathEscape(path)
            | opensks_patch_engine::PatchEngineError::SymlinkRejected(path) => {
                AgentAdapterError::PathEscape(path)
            }
            other => AgentAdapterError::InvalidInstruction(other.to_string()),
        })
}

/// Resolve a workspace-relative path through the PatchEngine path guard.
fn resolve_in_workspace(workspace: &Path, rel: &str) -> Result<PathBuf, AgentAdapterError> {
    let engine = PatchEngine::open(workspace)
        .map_err(|error| AgentAdapterError::InvalidInstruction(error.to_string()))?;
    engine.resolve(rel).map_err(|error| match error {
        opensks_patch_engine::PatchEngineError::PathEscape(path)
        | opensks_patch_engine::PatchEngineError::SymlinkRejected(path) => {
            AgentAdapterError::PathEscape(path)
        }
        other => AgentAdapterError::InvalidInstruction(other.to_string()),
    })
}

fn content_hash(content: &str) -> String {
    opensks_patch_engine::content_hash(content)
}

fn minimal_unified_diff(path: &str, before: &str, after: &str) -> String {
    opensks_patch_engine::unified_diff(path, before, after)
}

/// Pure planning for a local-test edit (no writes, no events): resolve the path,
/// read the pre-image, and compute the after-image + operation. Because it only
/// reads, it is safe to run concurrently across workers (the parallel runtime
/// relies on this). Returns `(rel_path, before, after, operation)`.
fn plan_local_edit(
    workspace: &Path,
    instruction: &LocalTestInstruction,
) -> Result<(String, String, String, FileOperation), AgentAdapterError> {
    let rel = instruction.path().to_string();
    let abs = resolve_in_workspace(workspace, &rel)?;
    let before = std::fs::read_to_string(&abs).unwrap_or_default();
    let (after, operation) = match instruction {
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
        // A multi-step edit is not a single-file plan; the agentic loop handles it.
        LocalTestInstruction::Steps { .. } => {
            return Err(AgentAdapterError::InvalidInstruction(
                "steps: handled by the agentic loop, not the single-file planner".to_string(),
            ));
        }
    };
    Ok((rel, before, after, operation))
}

/// One subcontracted unit of parallel work: a local-test instruction prompt
/// against a workspace (recovery directive §8 — parallel subcontracts).
#[derive(Debug, Clone)]
pub struct SubPacket {
    pub id: String,
    pub workspace: PathBuf,
    pub prompt: String,
}

/// The result of a parallel subcontract run.
#[derive(Debug, Clone)]
pub struct ParallelOutcome {
    /// The single atomic apply of the merged, non-conflicting patch set.
    pub apply: PatchApplyResult,
    /// Number of file writes each packet planned (by packet id, sorted).
    pub planned_per_packet: Vec<(String, usize)>,
    /// Per-packet planning errors (by packet id).
    pub errors: Vec<(String, String)>,
}

/// Plan every packet CONCURRENTLY (bounded by `max_concurrency`), then apply the
/// merged patch set through ONE atomic transaction (§8.4: workers produce
/// patches in parallel; an arbiter applies them). Two packets that target the
/// SAME path are a cross-worker conflict — the arbiter refuses the whole set
/// rather than letting concurrent writes race. All packets are assumed to share
/// the first packet's workspace.
pub fn run_parallel(
    packets: &[SubPacket],
    max_concurrency: usize,
) -> Result<ParallelOutcome, AgentAdapterError> {
    use std::sync::Mutex;

    let max = max_concurrency.max(1);
    let planned: Mutex<Vec<(String, PlannedWrite)>> = Mutex::new(Vec::new());
    let errors: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

    // Bounded concurrency: each chunk of up to `max` packets is planned in
    // parallel. Planning only reads disk, so concurrent planning is race-free.
    for chunk in packets.chunks(max) {
        std::thread::scope(|scope| {
            for packet in chunk {
                let planned = &planned;
                let errors = &errors;
                scope.spawn(move || {
                    // A packet with no actionable instruction plans nothing.
                    if let Some(instruction) = LocalTestInstruction::from_prompt(&packet.prompt) {
                        match plan_local_edit(&packet.workspace, &instruction) {
                            Ok((rel, before, after, operation)) => {
                                planned.lock().expect("planned lock").push((
                                    packet.id.clone(),
                                    PlannedWrite {
                                        path: rel,
                                        expected_before_hash: content_hash(&before),
                                        after_content: after,
                                        operation,
                                    },
                                ));
                            }
                            Err(error) => {
                                errors
                                    .lock()
                                    .expect("errors lock")
                                    .push((packet.id.clone(), error.to_string()));
                            }
                        }
                    }
                });
            }
        });
    }

    let mut planned = planned.into_inner().expect("planned lock");
    planned.sort_by(|a, b| a.0.cmp(&b.0)); // deterministic order
    let errors = errors.into_inner().expect("errors lock");
    let planned_per_packet = count_per_packet(&planned);

    // Arbiter: two workers writing the same path cannot both land.
    let mut seen = std::collections::BTreeSet::new();
    let mut dup_paths = Vec::new();
    for (_, write) in &planned {
        if !seen.insert(write.path.clone()) {
            dup_paths.push(write.path.clone());
        }
    }
    if !dup_paths.is_empty() {
        return Ok(ParallelOutcome {
            apply: PatchApplyResult {
                schema: PATCH_APPLY_RESULT_SCHEMA.to_string(),
                proposal_id: "parallel".to_string(),
                applied: false,
                applied_files: vec![],
                conflict_paths: dup_paths,
                rolled_back: false,
                reason_code: "cross_worker_path_conflict".to_string(),
                evidence_refs: vec![],
            },
            planned_per_packet,
            errors,
        });
    }

    let writes: Vec<PlannedWrite> = planned.into_iter().map(|(_, write)| write).collect();
    let workspace = packets
        .first()
        .map(|p| p.workspace.clone())
        .unwrap_or_else(|| PathBuf::from("."));
    let apply = apply_file_writes(&workspace, "parallel", &writes)?;
    Ok(ParallelOutcome {
        apply,
        planned_per_packet,
        errors,
    })
}

fn count_per_packet(planned: &[(String, PlannedWrite)]) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (id, _) in planned {
        *counts.entry(id.clone()).or_insert(0) += 1;
    }
    counts.into_iter().collect()
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
    fn steps_instruction_runs_the_agentic_loop_and_edits_multiple_files() {
        // A single conversation turn carrying a multi-step instruction drives the
        // REAL agentic loop end-to-end: an append to an existing file and a new
        // file, each its own transactional step.
        let ws = temp_workspace("steps");
        std::fs::write(ws.join("NOTES.md"), "one\n").unwrap();
        let adapter = LocalTestAdapter::new();
        let sink = CollectingSink::new();
        let prompt = r#"{"local_test":{"op":"steps","steps":[
            {"tool":"append_line","path":"NOTES.md","value":"two"},
            {"tool":"write_file","path":"sub/new.txt","content":"hello"}
        ]}}"#;
        let outcome = adapter.run(&request(&ws, prompt), &sink).unwrap();

        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        assert_eq!(
            std::fs::read_to_string(ws.join("NOTES.md")).unwrap(),
            "one\ntwo\n"
        );
        assert_eq!(
            std::fs::read_to_string(ws.join("sub/new.txt")).unwrap(),
            "hello"
        );
        // Two steps → two transactional applies, both landed.
        assert_eq!(outcome.apply_results.len(), 2);
        assert!(outcome.apply_results.iter().all(|a| a.applied));
        assert_eq!(
            sink.kinds().last(),
            Some(&AgentEventKind::AssistantTextCompleted)
        );
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

    #[test]
    fn parallel_independent_edits_all_apply() {
        let ws = temp_workspace("par-indep");
        let packets = vec![
            SubPacket {
                id: "w1".to_string(),
                workspace: ws.clone(),
                prompt: r#"{"local_test":{"op":"create_file","path":"a.txt","value":"A"}}"#
                    .to_string(),
            },
            SubPacket {
                id: "w2".to_string(),
                workspace: ws.clone(),
                prompt: r#"{"local_test":{"op":"create_file","path":"b.txt","value":"B"}}"#
                    .to_string(),
            },
        ];
        let outcome = run_parallel(&packets, 4).unwrap();
        assert!(outcome.apply.applied);
        assert_eq!(outcome.apply.applied_files.len(), 2);
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "A");
        assert_eq!(std::fs::read_to_string(ws.join("b.txt")).unwrap(), "B");
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn parallel_same_file_is_a_cross_worker_conflict() {
        // Two workers targeting one path is a race; the arbiter refuses the set
        // and the file is left untouched (§8.4 patch-only, no racing writes).
        let ws = temp_workspace("par-conflict");
        std::fs::write(ws.join("shared.txt"), "orig").unwrap();
        let packets = vec![
            SubPacket {
                id: "w1".to_string(),
                workspace: ws.clone(),
                prompt:
                    r#"{"local_test":{"op":"replace_content","path":"shared.txt","value":"X"}}"#
                        .to_string(),
            },
            SubPacket {
                id: "w2".to_string(),
                workspace: ws.clone(),
                prompt:
                    r#"{"local_test":{"op":"replace_content","path":"shared.txt","value":"Y"}}"#
                        .to_string(),
            },
        ];
        let outcome = run_parallel(&packets, 4).unwrap();
        assert!(!outcome.apply.applied);
        assert_eq!(outcome.apply.reason_code, "cross_worker_path_conflict");
        assert!(
            outcome
                .apply
                .conflict_paths
                .contains(&"shared.txt".to_string())
        );
        assert_eq!(
            std::fs::read_to_string(ws.join("shared.txt")).unwrap(),
            "orig"
        );
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn parallel_respects_bounded_concurrency() {
        let ws = temp_workspace("par-bounded");
        let packets: Vec<SubPacket> = (0..5)
            .map(|i| SubPacket {
                id: format!("w{i}"),
                workspace: ws.clone(),
                prompt: format!(
                    r#"{{"local_test":{{"op":"create_file","path":"f{i}.txt","value":"{i}"}}}}"#
                ),
            })
            .collect();
        let outcome = run_parallel(&packets, 2).unwrap(); // at most 2 plan concurrently
        assert!(outcome.apply.applied);
        assert_eq!(outcome.apply.applied_files.len(), 5);
        assert_eq!(outcome.planned_per_packet.len(), 5);
        std::fs::remove_dir_all(&ws).ok();
    }
}
