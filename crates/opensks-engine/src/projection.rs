//! Reducer that folds a run's event stream into a node-level projection (PR-029).
//!
//! [`project_run`] folds an entire ordered slice of events at once (a "rebuild"
//! from the durable log). [`ProjectionReducer::apply`] folds events one at a
//! time (a "live" reducer fed off the event stream). The two are guaranteed to
//! agree: `project_run(all) == events.iter().fold(reducer, apply)`. This is the
//! PR-029 acceptance criterion and is enforced by `rebuild_equals_live_reducer`.
//!
//! ## EventKind -> state mapping
//!
//! Run-level (mutate run state; terminal run states are sticky):
//! - `RunStarted`  -> `running`
//! - `RunResumed`  -> `running`
//! - `RunPaused`   -> `paused`
//! - `RunCancelled`-> `cancelled` (terminal)
//!
//! Node-level (keyed by `work_item_id`/`node_id` in the payload; terminal node
//! states are sticky and never downgraded):
//! - `WorkItemQueued`     -> `queued`
//! - `WorkItemLeased`     -> `dispatching`
//! - `WorkItemRunning`    -> `running`
//! - `WorkItemCompleted`  -> `succeeded` (terminal)
//! - `VerificationFailed` -> `failed` (terminal)
//! - `LeaseExpired`       -> `queued` (only if the node is not already terminal)
//! - `ApprovalRequested`  -> `waiting_for_approval` (only when it carries a node id)
//! - `VerificationPassed` / `LeaseHeartbeat` -> informational; metadata only
//!
//! Lower-information events never downgrade meaningful state:
//! - `SnapshotWritten` carries only run-aggregate counters and MUST NOT touch
//!   node or run state. This is the fix for the bug where a snapshot overwrote
//!   run state with the literal `"snapshot"`.
//! - `Unknown` and any unrecognized kind are ignored without panicking.

use opensks_contracts::{
    EventKind, ExecutionEventEnvelope, NodeProjectionState, PipelineExecutionProjection,
    RunProjectionState,
};
use opensks_event_store::{EventStore, EventStoreError};

/// Incremental reducer that folds events into a [`PipelineExecutionProjection`].
///
/// Construct it with [`ProjectionReducer::new`], feed events via
/// [`ProjectionReducer::apply`] (in non-decreasing `sequence` order), and read
/// the result with [`ProjectionReducer::projection`] /
/// [`ProjectionReducer::into_projection`].
#[derive(Debug, Clone)]
pub struct ProjectionReducer {
    projection: PipelineExecutionProjection,
}

impl ProjectionReducer {
    /// A reducer seeded with an empty projection for `run_id`.
    pub fn new(run_id: impl Into<String>) -> Self {
        Self {
            projection: PipelineExecutionProjection::empty(run_id),
        }
    }

    /// Borrow the current projection.
    pub fn projection(&self) -> &PipelineExecutionProjection {
        &self.projection
    }

    /// Consume the reducer and return the folded projection.
    pub fn into_projection(self) -> PipelineExecutionProjection {
        self.projection
    }

    /// Fold a single event into the projection.
    ///
    /// Events whose `run_id` does not match the reducer's run are ignored, so a
    /// caller may safely feed a mixed stream. Unknown/unrecognized kinds and
    /// lower-information events never corrupt or downgrade existing state.
    /// Metrics are recomputed after every applied event so that the live and
    /// rebuild paths stay byte-for-byte identical.
    pub fn apply(&mut self, event: &ExecutionEventEnvelope) {
        if event.run_id != self.projection.run_id {
            return;
        }

        // Adopt run-identifying metadata from the first event that carries it.
        if let Some(pipeline_id) = payload_str(event, "pipeline_id") {
            if self.projection.pipeline_id.is_none() {
                self.projection.pipeline_id = Some(pipeline_id.to_string());
            }
        }
        if let Some(conversation_id) = payload_str(event, "conversation_id") {
            if self.projection.conversation_id.is_none() {
                self.projection.conversation_id = Some(conversation_id.to_string());
            }
        }

        match event.kind {
            EventKind::RunStarted | EventKind::RunResumed => {
                self.projection.merge_run_state(RunProjectionState::Running);
            }
            EventKind::RunPaused => {
                self.projection.merge_run_state(RunProjectionState::Paused);
            }
            EventKind::RunCancelled => {
                self.projection
                    .merge_run_state(RunProjectionState::Cancelled);
            }

            EventKind::WorkItemQueued => self.apply_node_state(event, NodeProjectionState::Queued),
            EventKind::WorkItemLeased => {
                self.apply_node_state(event, NodeProjectionState::Dispatching)
            }
            EventKind::WorkItemRunning => {
                self.apply_node_state(event, NodeProjectionState::Running)
            }
            EventKind::WorkItemCompleted => {
                self.apply_node_state(event, NodeProjectionState::Succeeded)
            }
            EventKind::VerificationFailed => {
                self.apply_node_state(event, NodeProjectionState::Failed)
            }
            EventKind::LeaseExpired => self.apply_node_state(event, NodeProjectionState::Queued),
            EventKind::ApprovalRequested => {
                // Only treat as a node-level event when it names a node; a
                // run-wide approval request carries no work item id and is
                // metadata-only here.
                if node_id_of(event).is_some() {
                    self.apply_node_state(event, NodeProjectionState::WaitingForApproval);
                }
            }

            // Informational node events: refresh metadata but never change state.
            EventKind::VerificationPassed
            | EventKind::LeaseHeartbeat
            | EventKind::SteeringRequested
            | EventKind::ApprovalApproved
            | EventKind::ApprovalDenied => {
                self.observe_node_metadata(event);
            }

            // Lower-information / safe-to-ignore kinds. `SnapshotWritten`
            // explicitly does NOT touch node or run state.
            EventKind::SnapshotWritten | EventKind::Unknown => {}
        }

        self.projection.recompute_metrics();
    }

    /// Apply a node state transition plus any metadata the event carries.
    fn apply_node_state(&mut self, event: &ExecutionEventEnvelope, state: NodeProjectionState) {
        let Some(node_id) = node_id_of(event) else {
            return;
        };
        let node_id = node_id.to_string();
        let provider = payload_str(event, "provider_ref")
            .or_else(|| payload_str(event, "provider"))
            .map(str::to_string);
        let model = payload_str(event, "model_ref")
            .or_else(|| payload_str(event, "model"))
            .map(str::to_string);
        let attempt = payload_u32(event, "attempt");
        let touched = touched_paths_of(event);
        let message = payload_str(event, "message")
            .or_else(|| payload_str(event, "worker_message"))
            .map(str::to_string);

        let node = self.projection.node_entry(&node_id);
        node.merge_state(state);
        node.observe_provider(provider.as_deref());
        node.observe_model(model.as_deref());
        node.observe_attempt(attempt);
        node.observe_touched_paths(touched);
        node.observe_public_message(message.as_deref());
    }

    /// Refresh node metadata for an informational event without changing state.
    fn observe_node_metadata(&mut self, event: &ExecutionEventEnvelope) {
        let Some(node_id) = node_id_of(event) else {
            return;
        };
        let node_id = node_id.to_string();
        let provider = payload_str(event, "provider_ref")
            .or_else(|| payload_str(event, "provider"))
            .map(str::to_string);
        let model = payload_str(event, "model_ref")
            .or_else(|| payload_str(event, "model"))
            .map(str::to_string);
        let attempt = payload_u32(event, "attempt");
        let touched = touched_paths_of(event);
        let message = payload_str(event, "message")
            .or_else(|| payload_str(event, "worker_message"))
            .map(str::to_string);

        let node = self.projection.node_entry(&node_id);
        node.observe_provider(provider.as_deref());
        node.observe_model(model.as_deref());
        node.observe_attempt(attempt);
        node.observe_touched_paths(touched);
        node.observe_public_message(message.as_deref());
    }
}

/// Fold an entire ordered slice of events into a projection ("rebuild").
///
/// The events are expected in `sequence` order; the `run_id` is taken from the
/// first event (or empty if the slice is empty). Folding all events here equals
/// applying them one-by-one through [`ProjectionReducer::apply`].
pub fn project_run(events: &[ExecutionEventEnvelope]) -> PipelineExecutionProjection {
    let run_id = events
        .first()
        .map(|event| event.run_id.clone())
        .unwrap_or_default();
    let mut reducer = ProjectionReducer::new(run_id);
    for event in events {
        reducer.apply(event);
    }
    reducer.into_projection()
}

/// Replay a run from the durable store and project it.
pub fn project_run_from_store(
    store: &EventStore,
    run_id: &str,
) -> Result<PipelineExecutionProjection, EventStoreError> {
    let events = store.replay(run_id)?;
    let mut reducer = ProjectionReducer::new(run_id);
    for event in &events {
        reducer.apply(event);
    }
    Ok(reducer.into_projection())
}

/// The node id an event refers to, if any: prefer `node_id`, then `work_item_id`.
fn node_id_of(event: &ExecutionEventEnvelope) -> Option<&str> {
    payload_str(event, "node_id").or_else(|| payload_str(event, "work_item_id"))
}

fn payload_str<'a>(event: &'a ExecutionEventEnvelope, key: &str) -> Option<&'a str> {
    event.payload.get(key).and_then(serde_json::Value::as_str)
}

fn payload_u32(event: &ExecutionEventEnvelope, key: &str) -> Option<u32> {
    event
        .payload
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .map(|value| value.min(u64::from(u32::MAX)) as u32)
}

/// Touched paths from the payload, accepting either `touched_paths` or
/// `target_paths` as an array of strings.
fn touched_paths_of(event: &ExecutionEventEnvelope) -> Vec<String> {
    for key in ["touched_paths", "target_paths"] {
        if let Some(array) = event.payload.get(key).and_then(serde_json::Value::as_array) {
            return array
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect();
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensks_contracts::{EXECUTION_EVENT_ENVELOPE_SCHEMA, Sensitivity};

    fn envelope(
        run_id: &str,
        sequence: u64,
        kind: EventKind,
        payload: serde_json::Value,
    ) -> ExecutionEventEnvelope {
        ExecutionEventEnvelope {
            schema: EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
            id: format!("evt-{run_id}-{sequence}"),
            run_id: run_id.to_string(),
            sequence,
            occurred_at: format!("ts-{sequence}"),
            actor: "test".to_string(),
            causation_id: None,
            correlation_id: None,
            kind,
            payload,
            sensitivity: Sensitivity::Public,
            evidence_refs: Vec::new(),
        }
    }

    /// A representative run: start, two nodes through their lifecycles (one
    /// succeeds, one fails), a snapshot, then cancel.
    fn sample_stream(run_id: &str) -> Vec<ExecutionEventEnvelope> {
        vec![
            envelope(
                run_id,
                1,
                EventKind::RunStarted,
                serde_json::json!({"message": "run.start accepted", "pipeline_id": "single-model-safe"}),
            ),
            envelope(
                run_id,
                2,
                EventKind::WorkItemQueued,
                serde_json::json!({"work_item_id": "node-a", "to": "Ready"}),
            ),
            envelope(
                run_id,
                3,
                EventKind::WorkItemLeased,
                serde_json::json!({"work_item_id": "node-a", "to": "Leased", "provider": "anthropic"}),
            ),
            envelope(
                run_id,
                4,
                EventKind::WorkItemRunning,
                serde_json::json!({"work_item_id": "node-a", "to": "Running", "model": "claude", "attempt": 1}),
            ),
            envelope(
                run_id,
                5,
                EventKind::WorkItemCompleted,
                serde_json::json!({
                    "work_item_id": "node-a",
                    "to": "Completed",
                    "touched_paths": ["src/main.rs", "src/lib.rs"],
                    "message": "node a done"
                }),
            ),
            envelope(
                run_id,
                6,
                EventKind::WorkItemQueued,
                serde_json::json!({"work_item_id": "node-b", "to": "Ready"}),
            ),
            envelope(
                run_id,
                7,
                EventKind::WorkItemRunning,
                serde_json::json!({"work_item_id": "node-b", "to": "Running"}),
            ),
            envelope(
                run_id,
                8,
                EventKind::VerificationFailed,
                serde_json::json!({"work_item_id": "node-b", "to": "Failed", "message": "tests failed"}),
            ),
            envelope(
                run_id,
                9,
                EventKind::SnapshotWritten,
                serde_json::json!({"message": "scheduler snapshot written", "work_item_count": 2}),
            ),
        ]
    }

    /// Acceptance: rebuild (fold all at once) == live (apply one-by-one).
    #[test]
    fn rebuild_equals_live_reducer() {
        let events = sample_stream("run-rebuild");

        let rebuilt = project_run(&events);

        let mut live = ProjectionReducer::new("run-rebuild");
        for event in &events {
            live.apply(event);
        }
        let live = live.into_projection();

        assert_eq!(rebuilt, live);
        // Sanity: the run advanced and node states are as expected.
        assert_eq!(rebuilt.state, RunProjectionState::Running);
        assert_eq!(rebuilt.pipeline_id.as_deref(), Some("single-model-safe"));
        assert_eq!(
            rebuilt.node("node-a").map(|node| node.state),
            Some(NodeProjectionState::Succeeded)
        );
        assert_eq!(
            rebuilt.node("node-b").map(|node| node.state),
            Some(NodeProjectionState::Failed)
        );
        assert_eq!(rebuilt.metrics.completed, 1);
        assert_eq!(rebuilt.metrics.failed, 1);
    }

    /// Acceptance: a snapshot (or late lower-information event) never erases a
    /// terminal node state or a terminal run state.
    #[test]
    fn snapshot_does_not_erase_terminal_state() {
        let run_id = "run-terminal";
        let mut events = vec![
            envelope(
                run_id,
                1,
                EventKind::RunStarted,
                serde_json::json!({"pipeline_id": "p"}),
            ),
            envelope(
                run_id,
                2,
                EventKind::WorkItemRunning,
                serde_json::json!({"work_item_id": "node-a", "to": "Running"}),
            ),
            envelope(
                run_id,
                3,
                EventKind::WorkItemCompleted,
                serde_json::json!({"work_item_id": "node-a", "to": "Completed"}),
            ),
            envelope(
                run_id,
                4,
                EventKind::RunCancelled,
                serde_json::json!({"message": "cancel", "reason_code": "cancelled_by_user"}),
            ),
        ];

        let before = project_run(&events);
        assert_eq!(
            before.node("node-a").map(|node| node.state),
            Some(NodeProjectionState::Succeeded)
        );
        assert_eq!(before.state, RunProjectionState::Cancelled);

        // Late, lower-information events arrive: a snapshot, plus a stale
        // "running"/"queued" for the already-succeeded node, plus a stale
        // RunStarted. None of these may downgrade terminal state.
        events.push(envelope(
            run_id,
            5,
            EventKind::SnapshotWritten,
            serde_json::json!({"state": "snapshot", "work_item_count": 1}),
        ));
        events.push(envelope(
            run_id,
            6,
            EventKind::WorkItemRunning,
            serde_json::json!({"work_item_id": "node-a", "to": "Running"}),
        ));
        events.push(envelope(
            run_id,
            7,
            EventKind::WorkItemQueued,
            serde_json::json!({"work_item_id": "node-a", "to": "Ready"}),
        ));
        events.push(envelope(
            run_id,
            8,
            EventKind::RunStarted,
            serde_json::json!({"pipeline_id": "p"}),
        ));

        let after = project_run(&events);
        assert_eq!(
            after.node("node-a").map(|node| node.state),
            Some(NodeProjectionState::Succeeded),
            "terminal node state must survive snapshot and stale events"
        );
        assert_eq!(
            after.state,
            RunProjectionState::Cancelled,
            "terminal run state must survive snapshot and stale RunStarted"
        );
        // And the live path agrees with the rebuild.
        let mut live = ProjectionReducer::new(run_id);
        for event in &events {
            live.apply(event);
        }
        assert_eq!(after, live.into_projection());
    }

    /// Acceptance: an event whose kind/payload is unexpected is ignored safely
    /// (no panic) and the rest of the projection is unaffected.
    #[test]
    fn unknown_event_is_preserved_safely() {
        let run_id = "run-unknown";
        let events = vec![
            envelope(
                run_id,
                1,
                EventKind::RunStarted,
                serde_json::json!({"pipeline_id": "p"}),
            ),
            // Unknown kind with an entirely unexpected payload shape.
            envelope(run_id, 2, EventKind::Unknown, serde_json::json!([1, 2, 3])),
            // Unknown kind whose payload looks node-ish but must still be ignored.
            envelope(
                run_id,
                3,
                EventKind::Unknown,
                serde_json::json!({"work_item_id": "ghost", "to": "Running"}),
            ),
            envelope(
                run_id,
                4,
                EventKind::WorkItemRunning,
                serde_json::json!({"work_item_id": "node-a", "to": "Running"}),
            ),
            envelope(
                run_id,
                5,
                EventKind::WorkItemCompleted,
                serde_json::json!({"work_item_id": "node-a", "to": "Completed"}),
            ),
            // A node event missing its id is also ignored without panicking.
            envelope(
                run_id,
                6,
                EventKind::WorkItemRunning,
                serde_json::json!({"to": "Running"}),
            ),
        ];

        let projection = project_run(&events);
        assert_eq!(projection.state, RunProjectionState::Running);
        // The unknown "ghost" node must not have been created.
        assert!(projection.node("ghost").is_none());
        assert_eq!(projection.nodes.len(), 1);
        assert_eq!(
            projection.node("node-a").map(|node| node.state),
            Some(NodeProjectionState::Succeeded)
        );
        assert_eq!(projection.metrics.completed, 1);

        // Rebuild == live even with the unknown events interleaved.
        let mut live = ProjectionReducer::new(run_id);
        for event in &events {
            live.apply(event);
        }
        assert_eq!(projection, live.into_projection());
    }

    /// Benchmark-style acceptance: projecting 10_000 events completes and yields
    /// consistent metrics. Each node goes queued -> running -> completed.
    #[test]
    fn ten_thousand_event_rebuild() {
        let run_id = "run-10k";
        let node_count = 3_333_u64;
        let mut events = Vec::with_capacity(10_000);
        events.push(envelope(
            run_id,
            1,
            EventKind::RunStarted,
            serde_json::json!({"pipeline_id": "p"}),
        ));
        let mut sequence = 2;
        for index in 0..node_count {
            let node_id = format!("node-{index:05}");
            for (kind, to) in [
                (EventKind::WorkItemQueued, "Ready"),
                (EventKind::WorkItemRunning, "Running"),
                (EventKind::WorkItemCompleted, "Completed"),
            ] {
                events.push(envelope(
                    run_id,
                    sequence,
                    kind,
                    serde_json::json!({"work_item_id": node_id, "to": to}),
                ));
                sequence += 1;
            }
        }
        assert_eq!(events.len(), 1 + (node_count as usize) * 3);
        assert!(events.len() >= 9_999);

        let projection = project_run(&events);
        assert_eq!(projection.nodes.len(), node_count as usize);
        assert_eq!(projection.metrics.completed, node_count);
        assert_eq!(projection.metrics.failed, 0);
        assert_eq!(projection.metrics.queued, 0);
        assert_eq!(projection.metrics.active, 0);
        // Metric invariant: every node is accounted for exactly once across the
        // completed/active/queued/failed buckets (no cancelled/skipped here).
        assert_eq!(
            projection.metrics.completed
                + projection.metrics.active
                + projection.metrics.queued
                + projection.metrics.failed,
            node_count
        );

        // Rebuild == live at scale.
        let mut live = ProjectionReducer::new(run_id);
        for event in &events {
            live.apply(event);
        }
        assert_eq!(projection, live.into_projection());
    }

    /// The store-backed entry point projects a replayed run.
    #[test]
    fn project_run_from_store_replays_and_projects() {
        let mut store = EventStore::open_memory().expect("store");
        let run_id = "run-store";
        for event in [
            envelope(
                run_id,
                1,
                EventKind::RunStarted,
                serde_json::json!({"pipeline_id": "p"}),
            ),
            envelope(
                run_id,
                2,
                EventKind::WorkItemRunning,
                serde_json::json!({"work_item_id": "node-a", "to": "Running"}),
            ),
            envelope(
                run_id,
                3,
                EventKind::WorkItemCompleted,
                serde_json::json!({"work_item_id": "node-a", "to": "Completed"}),
            ),
        ] {
            store.append_event(event).expect("append");
        }

        let projection = project_run_from_store(&store, run_id).expect("project");
        assert_eq!(projection.run_id, run_id);
        assert_eq!(projection.state, RunProjectionState::Running);
        assert_eq!(
            projection.node("node-a").map(|node| node.state),
            Some(NodeProjectionState::Succeeded)
        );
        assert_eq!(projection.metrics.completed, 1);
    }
}
