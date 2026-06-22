//! Agent runtime contracts (recovery release §6, §7, §8, §19.7).
//!
//! These DTOs describe the boundary between the engine and a pluggable agent
//! adapter: what an adapter advertises ([`AgentAdapterDescriptor`]), the typed
//! events it streams ([`AgentEventEnvelope`] / [`AgentEventKind`]), the work it
//! is subcontracted to do ([`SubcontractPacket`] keyed by [`WorkerRole`]), and
//! the per-tool permission policy ([`ToolPolicy`]) that constrains it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Sensitivity;
use crate::turn::ReasoningEffort;

pub const AGENT_ADAPTER_DESCRIPTOR_SCHEMA: &str = "opensks.agent-adapter-descriptor.v1";
pub const AGENT_EVENT_ENVELOPE_SCHEMA: &str = "opensks.agent-event-envelope.v1";
pub const WORKER_ROLE_SCHEMA: &str = "opensks.worker-role.v1";
pub const SUBCONTRACT_PACKET_SCHEMA: &str = "opensks.subcontract-packet.v1";
pub const TOOL_POLICY_SCHEMA: &str = "opensks.tool-policy.v1";

/// What kind of work an adapter performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentAdapterKind {
    /// A real configured text/code model.
    Model,
    /// A deterministic adapter for tests that still performs real file edits.
    LocalTest,
    /// An external CLI agent.
    Cli,
    /// An image-generation adapter.
    Image,
}

/// What an adapter advertises to the engine/registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentAdapterDescriptor {
    pub schema: String,
    pub adapter_id: String,
    pub display_name: String,
    pub kind: AgentAdapterKind,
    pub supports_streaming: bool,
    pub supports_tools: bool,
    pub supports_resume: bool,
    pub supports_parallel_sessions: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_reasoning_efforts: Vec<ReasoningEffort>,
}

/// The typed kinds of event an adapter can emit (§6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventKind {
    AssistantTextDelta,
    AssistantTextCompleted,
    PlanUpdated,
    ToolCallStarted,
    ToolCallOutput,
    ToolCallCompleted,
    FilePatchProposed,
    FilePatchApplied,
    VerificationStarted,
    VerificationCompleted,
    WorkerSpawned,
    WorkerProgress,
    WorkerCompleted,
    ApprovalRequested,
    ApprovalResolved,
    ImageArtifactCreated,
    Warning,
    Error,
}

impl AgentEventKind {
    /// Whether this kind ends an assistant message / run leg.
    pub fn is_terminal_text(self) -> bool {
        matches!(self, Self::AssistantTextCompleted)
    }
}

/// A single agent event. Every event carries full identity + an ordered
/// `sequence` so the consumer can dedup/replay deterministically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentEventEnvelope {
    pub schema: String,
    pub stream_id: String,
    pub project_id: String,
    pub conversation_id: String,
    pub turn_id: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub sequence: u64,
    pub occurred_at_ms: u64,
    pub kind: AgentEventKind,
    pub payload: serde_json::Value,
    pub sensitivity: Sensitivity,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
}

/// The role a worker plays in a pipeline (§7.4). Routing maps roles to models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRole {
    Planner,
    CodeExplorer,
    Implementer,
    TestAuthor,
    Reviewer,
    SecurityReviewer,
    ImageGenerator,
    Arbiter,
}

/// What a subcontracted worker is required to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutputContractKind {
    /// Patch proposals only — no direct writes (the default, §8.4).
    PatchOnly,
    /// Free-form text/analysis.
    Text,
    /// A structured JSON result.
    StructuredJson,
}

/// The minimal-authority work packet handed to a worker (§8.3). A worker gets
/// least-privilege context/permissions, never blanket project access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubcontractPacket {
    pub schema: String,
    pub contract_version: String,
    pub work_item_id: String,
    pub parent_run_id: String,
    pub role: WorkerRole,
    pub objective: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    pub context_pack_ref: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    pub output_contract: OutputContractKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_requirements: Vec<String>,
}

/// Per-tool permission (§19.7). Risk rises from `Deny` to `Allow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermission {
    Deny,
    ReadOnly,
    Ask,
    Allow,
}

/// One tool's permission within a policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolPolicyEntry {
    pub tool: String,
    pub permission: ToolPermission,
}

/// A named, reusable tool permission policy referenced by id from thread
/// settings / subcontracts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolPolicy {
    pub schema: String,
    pub policy_id: String,
    pub entries: Vec<ToolPolicyEntry>,
}

impl ToolPolicy {
    /// Resolve a tool's permission, defaulting to `Deny` when unlisted (a tool
    /// is never implicitly allowed).
    pub fn permission_for(&self, tool: &str) -> ToolPermission {
        self.entries
            .iter()
            .find(|e| e.tool == tool)
            .map(|e| e.permission)
            .unwrap_or(ToolPermission::Deny)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_envelope_round_trips() {
        let env = AgentEventEnvelope {
            schema: AGENT_EVENT_ENVELOPE_SCHEMA.to_string(),
            stream_id: "s1".to_string(),
            project_id: "p1".to_string(),
            conversation_id: "c1".to_string(),
            turn_id: "t1".to_string(),
            run_id: "r1".to_string(),
            worker_id: Some("w1".to_string()),
            node_id: None,
            sequence: 7,
            occurred_at_ms: 1234,
            kind: AgentEventKind::AssistantTextDelta,
            payload: serde_json::json!({"text": "hi"}),
            sensitivity: Sensitivity::Internal,
            evidence_refs: vec![],
        };
        let json = serde_json::to_string(&env).unwrap();
        let parsed: AgentEventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, parsed);
        assert!(AgentEventKind::AssistantTextCompleted.is_terminal_text());
        assert!(!AgentEventKind::AssistantTextDelta.is_terminal_text());
    }

    #[test]
    fn unlisted_tool_is_denied_by_default() {
        let policy = ToolPolicy {
            schema: TOOL_POLICY_SCHEMA.to_string(),
            policy_id: "project-default".to_string(),
            entries: vec![ToolPolicyEntry {
                tool: "files".to_string(),
                permission: ToolPermission::Allow,
            }],
        };
        assert_eq!(policy.permission_for("files"), ToolPermission::Allow);
        assert_eq!(policy.permission_for("terminal"), ToolPermission::Deny);
    }
}
