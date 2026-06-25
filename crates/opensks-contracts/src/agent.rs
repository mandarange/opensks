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
pub const TOOL_DESCRIPTOR_SCHEMA: &str = "opensks.tool-descriptor.v1";
pub const TOOL_REGISTRY_SCHEMA: &str = "opensks.tool-registry.v1";

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

/// Whether a tool is currently executable through the local ToolRegistry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolAvailability {
    Available,
    Unavailable,
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

/// Canonical tool metadata. Provider adapters, UI capability reports, and the
/// executor share these descriptors so unsupported tools are visible but never
/// advertised as callable success paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolDescriptor {
    pub schema: String,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub permission: ToolPermission,
    pub availability: ToolAvailability,
    pub reason_code: String,
    pub input_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
}

impl ToolDescriptor {
    pub fn is_available(&self) -> bool {
        self.availability == ToolAvailability::Available
    }

    pub fn provider_function_name(&self) -> String {
        self.name.replace('.', "__")
    }
}

/// A registry snapshot for the tools known to the current runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolRegistry {
    pub schema: String,
    pub registry_id: String,
    pub revision: u64,
    pub tools: Vec<ToolDescriptor>,
}

impl ToolRegistry {
    pub fn available_provider_tools(&self) -> Vec<&ToolDescriptor> {
        self.tools
            .iter()
            .filter(|tool| tool.is_available())
            .filter(|tool| !matches!(tool.permission, ToolPermission::Deny))
            .collect()
    }

    pub fn descriptor(&self, name: &str) -> Option<&ToolDescriptor> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    /// Validate the canonical registry snapshot used by providers, executors,
    /// capability reports, and UI disablement.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema.trim().is_empty() {
            return Err("tool registry schema is empty".to_string());
        }
        if self.registry_id.trim().is_empty() {
            return Err("tool registry id is empty".to_string());
        }
        let mut seen = std::collections::BTreeSet::new();
        for tool in &self.tools {
            if tool.schema.trim().is_empty() {
                return Err("tool descriptor schema is empty".to_string());
            }
            if tool.name.trim().is_empty() {
                return Err("tool descriptor name is empty".to_string());
            }
            if !seen.insert(tool.name.as_str()) {
                return Err(format!("duplicate tool descriptor `{}`", tool.name));
            }
            if tool.display_name.trim().is_empty() {
                return Err(format!("tool `{}` display_name is empty", tool.name));
            }
            if tool.description.trim().is_empty() {
                return Err(format!("tool `{}` description is empty", tool.name));
            }
            if tool.reason_code.trim().is_empty() {
                return Err(format!("tool `{}` reason_code is empty", tool.name));
            }
            if !tool.input_schema.is_object() {
                return Err(format!(
                    "tool `{}` input_schema must be an object",
                    tool.name
                ));
            }
        }
        Ok(())
    }
}

pub fn default_tool_registry() -> ToolRegistry {
    ToolRegistry {
        schema: TOOL_REGISTRY_SCHEMA.to_string(),
        registry_id: "opensks-runtime-tools".to_string(),
        revision: 1,
        tools: vec![
            available_tool(
                "workspace.list_directory",
                "List Directory",
                "List non-hidden entries under a workspace-relative directory.",
                ToolPermission::ReadOnly,
                object_schema(&[("path", "string")], &["path"]),
                "workspace_tool_executable",
            ),
            available_tool(
                "workspace.read_file_range",
                "Read File Range",
                "Read UTF-8 text lines from a workspace-relative file.",
                ToolPermission::ReadOnly,
                object_schema(
                    &[
                        ("path", "string"),
                        ("start_line", "integer"),
                        ("end_line", "integer"),
                    ],
                    &["path"],
                ),
                "workspace_tool_executable",
            ),
            available_tool(
                "workspace.search_text",
                "Search Text",
                "Search UTF-8 workspace files for literal text.",
                ToolPermission::ReadOnly,
                object_schema(
                    &[
                        ("query", "string"),
                        ("path", "string"),
                        ("max_results", "integer"),
                    ],
                    &["query"],
                ),
                "workspace_tool_executable",
            ),
            available_tool(
                "codegraph.query_symbol",
                "Query Symbol",
                "Query the persisted CodeGraph symbol index.",
                ToolPermission::ReadOnly,
                object_schema(
                    &[("query", "string"), ("max_results", "integer")],
                    &["query"],
                ),
                "codegraph_executor_available",
            ),
            available_tool(
                "codegraph.references",
                "Symbol References",
                "Find references for a persisted CodeGraph symbol id.",
                ToolPermission::ReadOnly,
                object_schema(&[("symbol_id", "string")], &["symbol_id"]),
                "codegraph_executor_available",
            ),
            available_tool(
                "context.build_pack",
                "Build Context Pack",
                "Build a worker-scoped context pack from TriWiki and code evidence.",
                ToolPermission::ReadOnly,
                object_schema(&[("id", "string"), ("token_budget", "integer")], &["id"]),
                "context_executor_available",
            ),
            available_tool(
                "workspace.propose_patch",
                "Propose Patch",
                "Propose a full-file replacement to be applied through PatchEngine.",
                ToolPermission::Allow,
                object_schema(
                    &[("path", "string"), ("content", "string")],
                    &["path", "content"],
                ),
                "patch_engine_executable",
            ),
            available_tool(
                "workspace.diff_patch",
                "Diff Patch",
                "Append one line through PatchEngine after producing a diff-backed proposal.",
                ToolPermission::Allow,
                object_schema(
                    &[("path", "string"), ("value", "string")],
                    &["path", "value"],
                ),
                "patch_engine_executable",
            ),
            available_tool(
                "command.run",
                "Run Command",
                "Run an approved argv command under sandbox policy.",
                ToolPermission::Ask,
                object_schema(
                    &[("command", "string"), ("timeout_ms", "integer")],
                    &["command"],
                ),
                "command_runner_executable",
            ),
            available_tool(
                "test.run_targeted",
                "Run Targeted Tests",
                "Run an approved targeted test command and capture redacted output.",
                ToolPermission::Ask,
                object_schema(
                    &[("target", "string"), ("timeout_ms", "integer")],
                    &["target"],
                ),
                "command_runner_executable",
            ),
            available_tool(
                "git.status",
                "Git Status",
                "Read git status through the git service.",
                ToolPermission::ReadOnly,
                empty_schema(),
                "git_service_read_only",
            ),
            available_tool(
                "git.diff",
                "Git Diff",
                "Read path-limited git diff through the git service.",
                ToolPermission::ReadOnly,
                object_schema(&[("path", "string")], &[]),
                "git_service_read_only",
            ),
            available_tool(
                "git.log",
                "Git Log",
                "Read recent git history through the git service.",
                ToolPermission::ReadOnly,
                object_schema(&[("max_count", "integer")], &[]),
                "git_service_read_only",
            ),
            available_tool(
                "artifact.read",
                "Read Artifact",
                "Read a runtime artifact by artifact ref.",
                ToolPermission::ReadOnly,
                object_schema(&[("artifact_ref", "string")], &["artifact_ref"]),
                "artifact_store_executable",
            ),
            available_tool(
                "artifact.write",
                "Write Artifact",
                "Write a redacted runtime artifact by artifact ref.",
                ToolPermission::Ask,
                object_schema(
                    &[("artifact_ref", "string"), ("content", "string")],
                    &["artifact_ref", "content"],
                ),
                "artifact_store_executable",
            ),
            available_tool(
                "image.generate",
                "Generate Image",
                "Generate an image through a provider-backed image lane.",
                ToolPermission::Ask,
                object_schema(
                    &[
                        ("prompt", "string"),
                        ("asset_id", "string"),
                        ("width", "integer"),
                        ("height", "integer"),
                    ],
                    &["prompt"],
                ),
                "provider_image_executor_route_required",
            ),
            available_tool(
                "image.inspect",
                "Inspect Image",
                "Inspect an image artifact through a provider-backed vision lane.",
                ToolPermission::ReadOnly,
                object_schema(
                    &[
                        ("artifact_ref", "string"),
                        ("asset_id", "string"),
                        ("prompt", "string"),
                    ],
                    &["artifact_ref"],
                ),
                "provider_vision_executor_route_required",
            ),
            available_tool(
                "mcp.invoke",
                "Invoke MCP",
                "Invoke an allowlisted local MCP tool through the broker.",
                ToolPermission::Ask,
                object_schema(&[("tool", "string"), ("payload", "object")], &["tool"]),
                "local_mcp_broker_executable",
            ),
            available_tool(
                "skill.invoke",
                "Invoke Skill",
                "Load an allowlisted local skill route as bounded context.",
                ToolPermission::Ask,
                object_schema(&[("skill", "string"), ("payload", "object")], &["skill"]),
                "local_skill_registry_executable",
            ),
        ],
    }
}

fn available_tool(
    name: &str,
    display_name: &str,
    description: &str,
    permission: ToolPermission,
    input_schema: serde_json::Value,
    reason_code: &str,
) -> ToolDescriptor {
    tool_descriptor(
        name,
        display_name,
        description,
        permission,
        ToolAvailability::Available,
        input_schema,
        reason_code,
    )
}

fn tool_descriptor(
    name: &str,
    display_name: &str,
    description: &str,
    permission: ToolPermission,
    availability: ToolAvailability,
    input_schema: serde_json::Value,
    reason_code: &str,
) -> ToolDescriptor {
    ToolDescriptor {
        schema: TOOL_DESCRIPTOR_SCHEMA.to_string(),
        name: name.to_string(),
        display_name: display_name.to_string(),
        description: description.to_string(),
        permission,
        availability,
        reason_code: reason_code.to_string(),
        input_schema,
        evidence_refs: vec!["tool-registry:canonical-catalog".to_string()],
    }
}

fn empty_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
}

fn object_schema(fields: &[(&str, &str)], required: &[&str]) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    for (name, kind) in fields {
        properties.insert((*name).to_string(), serde_json::json!({ "type": kind }));
    }
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
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

    #[test]
    fn default_tool_registry_lists_required_tools_truthfully() {
        let registry = default_tool_registry();
        assert_eq!(registry.schema, TOOL_REGISTRY_SCHEMA);
        let names = registry
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        for required in [
            "workspace.list_directory",
            "workspace.read_file_range",
            "workspace.search_text",
            "codegraph.query_symbol",
            "codegraph.references",
            "context.build_pack",
            "workspace.propose_patch",
            "workspace.diff_patch",
            "command.run",
            "test.run_targeted",
            "git.status",
            "git.diff",
            "git.log",
            "artifact.read",
            "artifact.write",
            "image.generate",
            "image.inspect",
            "mcp.invoke",
            "skill.invoke",
        ] {
            assert!(
                names.contains(&required),
                "missing required tool {required}"
            );
        }

        assert_eq!(
            registry
                .descriptor("workspace.propose_patch")
                .expect("propose patch")
                .availability,
            ToolAvailability::Available
        );
        assert_eq!(
            registry
                .descriptor("git.status")
                .expect("git status")
                .availability,
            ToolAvailability::Available
        );
        assert_eq!(
            registry
                .descriptor("mcp.invoke")
                .expect("mcp invoke")
                .availability,
            ToolAvailability::Available
        );
        assert_eq!(
            registry
                .descriptor("skill.invoke")
                .expect("skill invoke")
                .availability,
            ToolAvailability::Available
        );
        assert!(
            registry
                .available_provider_tools()
                .iter()
                .all(|tool| tool.availability == ToolAvailability::Available)
        );
    }
}
