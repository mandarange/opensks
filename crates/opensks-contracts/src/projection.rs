//! Node-level pipeline execution projection (PR-029).
//!
//! A [`PipelineExecutionProjection`] is a read model folded from a run's
//! ordered [`ExecutionEventEnvelope`](crate::ExecutionEventEnvelope) stream.
//! It is durable-rebuildable: folding *all* events at once (a "rebuild")
//! produces the same value as applying them one-by-one (a "live" reducer).
//!
//! The reducer itself lives in `opensks-engine`; this module owns the DTOs,
//! state enums, and the monotonic-merge helpers the reducer relies on. Two
//! invariants are encoded directly in the types here:
//!
//! 1. **Terminal monotonicity.** Once a node is `succeeded`/`failed`/`cancelled`
//!    or a run reaches a terminal state, a later snapshot or a lower-information
//!    event never downgrades it. This is the fix for the bug where a
//!    `snapshot_written` event overwrote run state with the literal `"snapshot"`.
//! 2. **Safe unknowns.** Unrecognized event kinds carry no node/run rank, so the
//!    reducer can ignore them without corrupting the projection.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Schema id for [`PipelineExecutionProjection`].
pub const PIPELINE_EXECUTION_PROJECTION_SCHEMA: &str = "opensks.pipeline-execution-projection.v1";

/// Projection format version. Bump this when the fold logic or shape changes in
/// a way that requires rebuilding projections from the durable event log.
pub const PIPELINE_EXECUTION_PROJECTION_VERSION: u64 = 1;

/// Coarse lifecycle state of a whole run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunProjectionState {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl RunProjectionState {
    /// Terminal run states are never downgraded by later events.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Merge a newly observed run state, applied in event order. Three rules:
    /// terminal states are sticky (a late/stale lower-information event never
    /// erases them); the run never regresses to `Queued`; and pause/resume is
    /// honoured (`Running ⇄ Paused`).
    ///
    /// This replaces a rank-only rule that silently dropped `running → paused`
    /// because `Paused` ranked below `Running` (PIPE-002): a real `RunPaused`
    /// event after `RunStarted` was ignored, so a paused run still displayed as
    /// running. Folding remains order-dependent only, so rebuild == live.
    fn merge(self, incoming: Self) -> Self {
        if self.is_terminal() {
            return self;
        }
        if incoming.is_terminal() {
            return incoming;
        }
        // Both non-terminal: allow Queued → {Running, Paused} and Running ⇄
        // Paused, but never move backward to Queued.
        match incoming {
            Self::Queued => self,
            _ => incoming,
        }
    }
}

/// Lifecycle state of a single node (work item) within a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NodeProjectionState {
    Queued,
    Dispatching,
    Running,
    WaitingForApproval,
    Succeeded,
    Failed,
    Cancelled,
    Skipped,
}

impl NodeProjectionState {
    /// Terminal node states are never downgraded by later events (snapshots
    /// included).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Skipped
        )
    }

    /// Monotonic information rank used to merge a newly observed node state.
    /// Live states are ordered by progress; terminal states sit at the top so a
    /// later snapshot or lower-information event cannot pull a node backwards.
    fn rank(self) -> u8 {
        match self {
            Self::Queued => 0,
            Self::Dispatching => 1,
            Self::WaitingForApproval => 2,
            Self::Running => 3,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Skipped => 4,
        }
    }

    fn merge(self, incoming: Self) -> Self {
        if self.is_terminal() {
            return self;
        }
        if incoming.rank() >= self.rank() {
            incoming
        } else {
            self
        }
    }
}

/// Aggregate counts over the run's nodes, recomputed after each fold.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunMetrics {
    pub completed: u64,
    pub active: u64,
    pub queued: u64,
    pub failed: u64,
}

/// Projected execution state of a single node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NodeExecutionProjection {
    pub node_id: String,
    pub state: NodeProjectionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ref: Option<String>,
    pub attempt: u32,
    #[serde(default)]
    pub touched_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_public_message: Option<String>,
}

impl NodeExecutionProjection {
    /// Create a freshly-queued node projection.
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            state: NodeProjectionState::Queued,
            provider_ref: None,
            model_ref: None,
            attempt: 0,
            touched_paths: Vec::new(),
            last_public_message: None,
        }
    }

    /// Merge an observed `state` without downgrading a terminal node.
    pub fn merge_state(&mut self, incoming: NodeProjectionState) {
        self.state = self.state.merge(incoming);
    }

    /// Record a provider reference if one is present and not already known.
    pub fn observe_provider(&mut self, provider_ref: Option<&str>) {
        if let Some(provider_ref) = provider_ref {
            if !provider_ref.is_empty() {
                self.provider_ref = Some(provider_ref.to_string());
            }
        }
    }

    /// Record a model reference if one is present and not already known.
    pub fn observe_model(&mut self, model_ref: Option<&str>) {
        if let Some(model_ref) = model_ref {
            if !model_ref.is_empty() {
                self.model_ref = Some(model_ref.to_string());
            }
        }
    }

    /// Record the highest observed attempt number (never decreases).
    pub fn observe_attempt(&mut self, attempt: Option<u32>) {
        if let Some(attempt) = attempt {
            if attempt > self.attempt {
                self.attempt = attempt;
            }
        }
    }

    /// Merge in touched paths, preserving order and de-duplicating.
    pub fn observe_touched_paths<I, S>(&mut self, paths: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for path in paths {
            let path = path.into();
            if !path.is_empty() && !self.touched_paths.contains(&path) {
                self.touched_paths.push(path);
            }
        }
    }

    /// Record the most recent public-facing message, if non-empty.
    pub fn observe_public_message(&mut self, message: Option<&str>) {
        if let Some(message) = message {
            if !message.is_empty() {
                self.last_public_message = Some(message.to_string());
            }
        }
    }
}

/// A node-level read model of a single run's execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PipelineExecutionProjection {
    pub schema: String,
    pub projection_version: u64,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_id: Option<String>,
    pub state: RunProjectionState,
    #[serde(default)]
    pub nodes: Vec<NodeExecutionProjection>,
    pub metrics: RunMetrics,
}

impl PipelineExecutionProjection {
    /// An empty projection for `run_id` with no events folded yet.
    pub fn empty(run_id: impl Into<String>) -> Self {
        Self {
            schema: PIPELINE_EXECUTION_PROJECTION_SCHEMA.to_string(),
            projection_version: PIPELINE_EXECUTION_PROJECTION_VERSION,
            run_id: run_id.into(),
            conversation_id: None,
            pipeline_id: None,
            state: RunProjectionState::Queued,
            nodes: Vec::new(),
            metrics: RunMetrics::default(),
        }
    }

    /// Merge run-level state monotonically (terminal states are sticky).
    pub fn merge_run_state(&mut self, incoming: RunProjectionState) {
        self.state = self.state.merge(incoming);
    }

    /// Find a node by id.
    pub fn node(&self, node_id: &str) -> Option<&NodeExecutionProjection> {
        self.nodes.iter().find(|node| node.node_id == node_id)
    }

    /// Get a mutable handle to the node `node_id`, inserting a freshly-queued
    /// node (kept in stable insertion order) if it does not yet exist.
    pub fn node_entry(&mut self, node_id: &str) -> &mut NodeExecutionProjection {
        if let Some(index) = self.nodes.iter().position(|node| node.node_id == node_id) {
            &mut self.nodes[index]
        } else {
            self.nodes.push(NodeExecutionProjection::new(node_id));
            self.nodes
                .last_mut()
                .expect("node was just pushed and must exist")
        }
    }

    /// Recompute aggregate metrics from the current node set. Idempotent, so it
    /// is safe to call after every fold step.
    pub fn recompute_metrics(&mut self) {
        let mut metrics = RunMetrics::default();
        for node in &self.nodes {
            match node.state {
                NodeProjectionState::Succeeded => metrics.completed += 1,
                NodeProjectionState::Failed => metrics.failed += 1,
                NodeProjectionState::Queued => metrics.queued += 1,
                NodeProjectionState::Dispatching
                | NodeProjectionState::Running
                | NodeProjectionState::WaitingForApproval => metrics.active += 1,
                // Cancelled / skipped nodes are terminal but not counted as
                // completed, failed, queued, or active.
                NodeProjectionState::Cancelled | NodeProjectionState::Skipped => {}
            }
        }
        self.metrics = metrics;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold(states: &[RunProjectionState]) -> RunProjectionState {
        let mut p = PipelineExecutionProjection::empty("run");
        for &s in states {
            p.merge_run_state(s);
        }
        p.state
    }

    #[test]
    fn running_can_transition_to_paused_and_back() {
        use RunProjectionState::*;
        // The PIPE-002 regression: a RunPaused after RunStarted must reflect.
        assert_eq!(fold(&[Running, Paused]), Paused);
        assert_eq!(fold(&[Running, Paused, Running]), Running);
        assert_eq!(fold(&[Queued, Running, Paused]), Paused);
    }

    #[test]
    fn terminal_state_is_sticky() {
        use RunProjectionState::*;
        assert_eq!(fold(&[Running, Completed, Running]), Completed);
        assert_eq!(fold(&[Running, Cancelled, Paused]), Cancelled);
        assert_eq!(fold(&[Running, Failed, Queued]), Failed);
    }

    #[test]
    fn never_regresses_to_queued() {
        use RunProjectionState::*;
        assert_eq!(fold(&[Running, Queued]), Running);
        assert_eq!(fold(&[Paused, Queued]), Paused);
    }

    #[test]
    fn non_terminal_can_reach_terminal() {
        use RunProjectionState::*;
        assert_eq!(fold(&[Queued, Running, Completed]), Completed);
        assert_eq!(fold(&[Paused, Failed]), Failed);
    }
}
