//! Pipeline topology snapshot (recovery release §13.3).
//!
//! Emitted right after a run is accepted, the snapshot carries the *compiled*
//! plan graph — nodes and explicit edges — so the projection/graph never has to
//! guess topology from event-arrival order (the PIPE-003 fix).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::agent::WorkerRole;

pub const PIPELINE_TOPOLOGY_SNAPSHOT_SCHEMA: &str = "opensks.pipeline-topology-snapshot.v1";

/// A node in the compiled pipeline plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PipelineTopologyNode {
    pub node_id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<WorkerRole>,
}

/// What an edge represents between two nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TopologyEdgeKind {
    /// `to` depends on `from` completing.
    Dependency,
    /// `from` produces data consumed by `to`.
    Data,
    /// Control/ordering only.
    Control,
}

/// A directed edge in the plan graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PipelineTopologyEdge {
    pub from_node: String,
    pub to_node: String,
    pub kind: TopologyEdgeKind,
}

/// The compiled topology of a run's pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PipelineTopologySnapshot {
    pub schema: String,
    pub run_id: String,
    pub pipeline_id: String,
    pub graph_revision: String,
    pub nodes: Vec<PipelineTopologyNode>,
    pub edges: Vec<PipelineTopologyEdge>,
}

impl PipelineTopologySnapshot {
    /// Whether every edge endpoint refers to a declared node.
    pub fn edges_reference_known_nodes(&self) -> bool {
        let ids: std::collections::BTreeSet<&str> =
            self.nodes.iter().map(|n| n.node_id.as_str()).collect();
        self.edges
            .iter()
            .all(|e| ids.contains(e.from_node.as_str()) && ids.contains(e.to_node.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips_and_validates_edges() {
        let snap = PipelineTopologySnapshot {
            schema: PIPELINE_TOPOLOGY_SNAPSHOT_SCHEMA.to_string(),
            run_id: "r1".to_string(),
            pipeline_id: "parallel-build".to_string(),
            graph_revision: "rev-1".to_string(),
            nodes: vec![
                PipelineTopologyNode {
                    node_id: "plan".to_string(),
                    label: "Plan".to_string(),
                    role: Some(WorkerRole::Planner),
                },
                PipelineTopologyNode {
                    node_id: "impl".to_string(),
                    label: "Implement".to_string(),
                    role: Some(WorkerRole::Implementer),
                },
            ],
            edges: vec![PipelineTopologyEdge {
                from_node: "plan".to_string(),
                to_node: "impl".to_string(),
                kind: TopologyEdgeKind::Dependency,
            }],
        };
        let json = serde_json::to_string(&snap).unwrap();
        let parsed: PipelineTopologySnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, parsed);
        assert!(snap.edges_reference_known_nodes());

        let mut dangling = snap.clone();
        dangling.edges.push(PipelineTopologyEdge {
            from_node: "plan".to_string(),
            to_node: "missing".to_string(),
            kind: TopologyEdgeKind::Control,
        });
        assert!(!dangling.edges_reference_known_nodes());
    }
}
