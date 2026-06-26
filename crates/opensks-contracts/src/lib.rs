use std::collections::BTreeMap;

use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};

pub const CONTRACT_VERSION: &str = "opensks.contracts.v1";
pub const ENGINE_REQUEST_SCHEMA: &str = "opensks.engine-request.v1";
pub const ENGINE_EVENT_SCHEMA: &str = "opensks.engine-event.v1";
pub const EXECUTION_EVENT_SCHEMA: &str = "opensks.execution-event.v1";
pub const EXECUTION_EVENT_ENVELOPE_SCHEMA: &str = "opensks.execution-event-envelope.v1";
pub const WORK_ITEM_SCHEMA: &str = "opensks.work-item.v1";
pub const PROVIDER_CONNECTION_SCHEMA: &str = "opensks.provider-connection.v1";
pub const PROVIDER_MUTATION_SCHEMA: &str = "opensks.provider-mutation.v1";
pub const SECRET_REF_SCHEMA: &str = "opensks.secret-ref.v1";
pub const MODEL_CATALOG_ENTRY_SCHEMA: &str = "opensks.model-catalog-entry.v1";
pub const PROVIDER_PROBE_RECEIPT_SCHEMA: &str = "opensks.provider-probe-receipt.v1";
pub const PROVIDER_ADAPTER_CHECK_SCHEMA: &str = "opensks.provider-adapter-check.v1";
pub const PROVIDER_MOCK_E2E_SCHEMA: &str = "opensks.provider-mock-e2e.v1";
pub const MODEL_PROFILE_SCHEMA: &str = "opensks.model-profile.v1";
pub const PROVIDER_DESCRIPTOR_SCHEMA: &str = "opensks.provider-descriptor.v1";
pub const ROUTING_DECISION_SCHEMA: &str = "opensks.routing-decision.v1";
pub const SCHEDULER_WORK_ITEM_SCHEMA: &str = "opensks.scheduler-work-item.v1";
pub const CONCURRENCY_DECISION_SCHEMA: &str = "opensks.concurrency-decision.v1";
pub const PIPELINE_GRAPH_SCHEMA: &str = "opensks.pipeline-graph.v1";
pub const OBJECTIVE_PLAN_REQUEST_SCHEMA: &str = "opensks.objective-plan-request.v1";
pub const OBJECTIVE_PLAN_RECEIPT_SCHEMA: &str = "opensks.objective-plan-receipt.v1";
pub const PLANNER_SHARD_POLICY_SCHEMA: &str = "opensks.planner-shard-policy.v1";
pub const COMPILED_PLAN_SCHEMA: &str = "opensks.compiled-plan.v1";
pub const GIT_ISOLATION_SCHEMA: &str = "opensks.git-isolation.v1";
pub const WORKTREE_ISOLATION_INVENTORY_RECEIPT_SCHEMA: &str =
    "opensks.worktree-isolation-inventory-receipt.v1";
pub const WORKTREE_ISOLATION_RECOVERY_RECEIPT_SCHEMA: &str =
    "opensks.worktree-isolation-recovery-receipt.v1";
pub const PATCH_ENVELOPE_SCHEMA: &str = "opensks.patch-envelope.v1";
pub const ROLE_SUBCONTRACT_CANDIDATE_RECEIPT_SCHEMA: &str = "opensks.role-subcontract-candidate.v1";
pub const SEMANTIC_VERIFIER_JUDGMENT_SCHEMA: &str = "opensks.semantic-verifier-judgment.v1";
pub const INTEGRATION_CANDIDATE_RECEIPT_SCHEMA: &str = "opensks.integration-candidate.v1";
pub const INTEGRATION_CANDIDATE_SELECTION_RECEIPT_SCHEMA: &str =
    "opensks.integration-candidate-selection-receipt.v1";
pub const INTEGRATION_VERIFICATION_RECEIPT_SCHEMA: &str =
    "opensks.integration-verification-receipt.v1";
pub const INTEGRATION_APPLY_RECEIPT_SCHEMA: &str = "opensks.integration-apply-receipt.v1";
pub const INTEGRATION_REPAIR_ITEM_SCHEMA: &str = "opensks.integration-repair-item.v1";
pub const INTEGRATION_FINAL_SEAL_SCHEMA: &str = "opensks.integration-final-seal.v1";
pub const INTEGRATION_CLEANUP_RECEIPT_SCHEMA: &str = "opensks.integration-cleanup-receipt.v1";
pub const COMPLETION_PROOF_SCHEMA: &str = "opensks.completion-proof.v1";
pub const HOOK_SPEC_SCHEMA: &str = "opensks.hook-spec.v1";
pub const HOOK_DECISION_SCHEMA: &str = "opensks.hook-decision.v1";
pub const CODEGRAPH_RECORD_SCHEMA: &str = "opensks.codegraph-record.v1";
pub const CODEGRAPH_INDEX_SCHEMA: &str = "opensks.codegraph-index.v1";
pub const TRIWIKI_RECORD_SCHEMA: &str = "opensks.triwiki-record.v1";
pub const CONTEXT_PACK_SCHEMA: &str = "opensks.context-pack.v1";
pub const IMAGE_ASSET_SCHEMA: &str = "opensks.image-asset.v1";
pub const IMAGE_LEDGER_SCHEMA: &str = "opensks.image-ledger.v1";
pub const IMAGE_PROVENANCE_RECEIPT_SCHEMA: &str = "opensks.image-provenance-receipt.v1";
pub const REASONING_REPORT_SCHEMA: &str = "opensks.reasoning-report.v1";
pub const OUTBOX_ITEM_SCHEMA: &str = "opensks.outbox-item.v1";
pub const OUTBOX_DISPATCH_REPORT_SCHEMA: &str = "opensks.outbox-dispatch-report.v1";
pub const DATA_PLANE_MANIFEST_SCHEMA: &str = "opensks.data-plane-manifest.v1";
pub const RETENTION_PLAN_SCHEMA: &str = "opensks.retention-plan.v1";
pub const RELEASE_PROOF_SCHEMA: &str = "opensks.release-proof.v1";
pub const PERF_STRESS_REPORT_SCHEMA: &str = "opensks.perf-stress-report.v1";
pub const TERMINAL_SESSION_SCHEMA: &str = "opensks.terminal-session.v1";
pub const TERMINAL_EVENT_SCHEMA: &str = "opensks.terminal-event.v1";
pub const TERMINAL_COMMAND_BLOCK_SCHEMA: &str = "opensks.terminal-command-block.v1";
pub const TERMINAL_SUGGESTION_REQUEST_SCHEMA: &str = "opensks.terminal-suggestion-request.v1";
pub const TERMINAL_SUGGESTION_SCHEMA: &str = "opensks.terminal-suggestion.v1";
pub const TERMINAL_AGENT_TURN_SCHEMA: &str = "opensks.terminal-agent-turn.v1";
pub const TERMINAL_RISK_DECISION_SCHEMA: &str = "opensks.terminal-risk-decision.v1";
pub const TERMINAL_MCP_TOOL_DESCRIPTOR_SCHEMA: &str = "opensks.terminal-mcp-tool-descriptor.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EngineRequestKind {
    Hello,
    Health,
    SubscribeEvents,
    ConversationTurnStart,
    ConversationSupervisorTick,
    IntegrationCandidateApply,
    WorktreeInventory,
    WorktreeRecover,
    RunStart,
    RunPause,
    RunResume,
    RunCancel,
    RunSteer,
    ApprovalRequest,
    ApprovalApprove,
    ApprovalDeny,
    OutboxDispatch,
    TerminalSessionStart,
    TerminalSessionInput,
    TerminalSessionResize,
    TerminalSessionStop,
    TerminalSuggestionRequest,
    TerminalAgentTurnStart,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EngineEventType {
    EngineHello,
    EngineHealth,
    ExecutionEvent,
    Error,
    /// STREAM-001: the explicit per-request terminal marker. The daemon emits one
    /// as the FINAL event of every request response so the client completes on it,
    /// never on a silence/quiet-window heuristic. Carries the `request_id`.
    RequestCompleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Ready,
    Queued,
    Leased,
    Running,
    Waiting,
    Verifying,
    AwaitingApproval,
    Applying,
    Paused,
    Retrying,
    Blocked,
    Failed,
    Cancelled,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrustStatus {
    Verified,
    VerifiedWithPartialEvidence,
    NotVerified,
    Stale,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    Public,
    Internal,
    Secret,
}

impl Sensitivity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Internal => "internal",
            Self::Secret => "secret",
        }
    }

    pub fn parse_label(value: &str) -> Self {
        match value {
            "public" => Self::Public,
            "secret" => Self::Secret,
            _ => Self::Internal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    RunStarted,
    RunPaused,
    RunResumed,
    RunCancelled,
    RunCompleted,
    SteeringRequested,
    ApprovalRequested,
    ApprovalApproved,
    ApprovalDenied,
    WorkItemQueued,
    WorkItemLeased,
    WorkItemRunning,
    WorkItemCompleted,
    LeaseHeartbeat,
    LeaseExpired,
    VerificationPassed,
    VerificationFailed,
    GitCommitReceipt,
    GitPushReceipt,
    GitPushFailed,
    ImageArtifactCreated,
    SnapshotWritten,
    Unknown,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RunStarted => "run_started",
            Self::RunPaused => "run_paused",
            Self::RunResumed => "run_resumed",
            Self::RunCancelled => "run_cancelled",
            Self::RunCompleted => "run_completed",
            Self::SteeringRequested => "steering_requested",
            Self::ApprovalRequested => "approval_requested",
            Self::ApprovalApproved => "approval_approved",
            Self::ApprovalDenied => "approval_denied",
            Self::WorkItemQueued => "work_item_queued",
            Self::WorkItemLeased => "work_item_leased",
            Self::WorkItemRunning => "work_item_running",
            Self::WorkItemCompleted => "work_item_completed",
            Self::LeaseHeartbeat => "lease_heartbeat",
            Self::LeaseExpired => "lease_expired",
            Self::VerificationPassed => "verification_passed",
            Self::VerificationFailed => "verification_failed",
            Self::GitCommitReceipt => "git_commit_receipt",
            Self::GitPushReceipt => "git_push_receipt",
            Self::GitPushFailed => "git_push_failed",
            Self::ImageArtifactCreated => "image_artifact_created",
            Self::SnapshotWritten => "snapshot_written",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse_label(value: &str) -> Self {
        match value {
            "run_started" => Self::RunStarted,
            "run_paused" => Self::RunPaused,
            "run_resumed" => Self::RunResumed,
            "run_cancelled" => Self::RunCancelled,
            "run_completed" => Self::RunCompleted,
            "steering_requested" => Self::SteeringRequested,
            "approval_requested" => Self::ApprovalRequested,
            "approval_approved" => Self::ApprovalApproved,
            "approval_denied" => Self::ApprovalDenied,
            "work_item_queued" => Self::WorkItemQueued,
            "work_item_leased" => Self::WorkItemLeased,
            "work_item_running" => Self::WorkItemRunning,
            "work_item_completed" => Self::WorkItemCompleted,
            "lease_heartbeat" => Self::LeaseHeartbeat,
            "lease_expired" => Self::LeaseExpired,
            "verification_passed" => Self::VerificationPassed,
            "verification_failed" => Self::VerificationFailed,
            "git_commit_receipt" => Self::GitCommitReceipt,
            "git_push_receipt" => Self::GitPushReceipt,
            "git_push_failed" => Self::GitPushFailed,
            "image_artifact_created" => Self::ImageArtifactCreated,
            "snapshot_written" => Self::SnapshotWritten,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EngineRequest {
    pub schema: String,
    pub id: String,
    pub kind: EngineRequestKind,
    #[serde(default)]
    pub protocol_version: String,
    #[serde(default)]
    pub params: EngineRequestParams,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct EngineRequestParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_turn_start: Option<ConversationTurnStartRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_session_start: Option<TerminalSessionStartRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_session_input: Option<TerminalSessionInputRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_session_resize: Option<TerminalSessionResizeRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_session_stop: Option<TerminalSessionStopRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_suggestion_request: Option<TerminalSuggestionRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_agent_turn_start: Option<TerminalAgentTurnStartRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_ttl_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_interval_ms: Option<u64>,
}

impl EngineRequest {
    pub fn hello(id: impl Into<String>) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::Hello,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams::default(),
        }
    }

    pub fn health(id: impl Into<String>) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::Health,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams::default(),
        }
    }

    pub fn run_start(
        id: impl Into<String>,
        pipeline_id: impl Into<String>,
        objective: impl Into<String>,
    ) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::RunStart,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                pipeline_id: Some(pipeline_id.into()),
                graph_path: None,
                objective: Some(objective.into()),
                run_id: None,
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn conversation_turn_start(request: ConversationTurnStartRequest) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: request.request_id.clone(),
            kind: EngineRequestKind::ConversationTurnStart,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                conversation_turn_start: Some(request),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn conversation_supervisor_tick(
        id: impl Into<String>,
        supervisor_id: impl Into<String>,
        lease_ttl_ms: u64,
    ) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::ConversationSupervisorTick,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                supervisor_id: Some(supervisor_id.into()),
                lease_ttl_ms: Some(lease_ttl_ms),
                reason_code: Some("conversation_supervisor_tick_requested".to_string()),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn conversation_supervisor_tick_for_run(
        id: impl Into<String>,
        supervisor_id: impl Into<String>,
        lease_ttl_ms: u64,
        run_id: impl Into<String>,
    ) -> Self {
        let mut request = Self::conversation_supervisor_tick(id, supervisor_id, lease_ttl_ms);
        request.params.run_id = Some(run_id.into());
        request
    }

    pub fn integration_candidate_apply(
        id: impl Into<String>,
        run_id: impl Into<String>,
        approval_id: impl Into<String>,
    ) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::IntegrationCandidateApply,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                run_id: Some(run_id.into()),
                approval_id: Some(approval_id.into()),
                scope: Some("integration_apply".to_string()),
                reason_code: Some("integration_apply_requested".to_string()),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn worktree_inventory(id: impl Into<String>, run_id: impl Into<String>) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::WorktreeInventory,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                run_id: Some(run_id.into()),
                scope: Some("worktree_inventory".to_string()),
                reason_code: Some("worktree_inventory_requested".to_string()),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn worktree_recover(id: impl Into<String>, run_id: impl Into<String>) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::WorktreeRecover,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                run_id: Some(run_id.into()),
                scope: Some("worktree_recover".to_string()),
                reason_code: Some("worktree_recover_requested".to_string()),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn run_cancel(id: impl Into<String>, run_id: impl Into<String>) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::RunCancel,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                run_id: Some(run_id.into()),
                reason_code: Some("cancelled_by_user".to_string()),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn run_steer(
        id: impl Into<String>,
        run_id: impl Into<String>,
        target_id: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::RunSteer,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                run_id: Some(run_id.into()),
                target_id: Some(target_id.into()),
                message: Some(message.into()),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn approval_request(
        id: impl Into<String>,
        run_id: impl Into<String>,
        approval_id: impl Into<String>,
        scope: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::ApprovalRequest,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                run_id: Some(run_id.into()),
                approval_id: Some(approval_id.into()),
                scope: Some(scope.into()),
                message: Some(message.into()),
                reason_code: Some("approval_required".to_string()),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn approval_decision(
        id: impl Into<String>,
        run_id: impl Into<String>,
        approval_id: impl Into<String>,
        approved: bool,
    ) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: if approved {
                EngineRequestKind::ApprovalApprove
            } else {
                EngineRequestKind::ApprovalDeny
            },
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                run_id: Some(run_id.into()),
                approval_id: Some(approval_id.into()),
                reason_code: Some(if approved {
                    "approved_by_user".to_string()
                } else {
                    "denied_by_user".to_string()
                }),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn outbox_dispatch(
        id: impl Into<String>,
        target_id: impl Into<String>,
        approval_id: Option<String>,
    ) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::OutboxDispatch,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                target_id: Some(target_id.into()),
                approval_id,
                scope: Some("git_push".to_string()),
                reason_code: Some("outbox_dispatch_requested".to_string()),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn subscribe_events(
        id: impl Into<String>,
        run_id: impl Into<String>,
        since_sequence: Option<u64>,
    ) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::SubscribeEvents,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                run_id: Some(run_id.into()),
                since_sequence,
                reason_code: Some("reconnect_replay_requested".to_string()),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn terminal_session_start(
        id: impl Into<String>,
        request: TerminalSessionStartRequest,
    ) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::TerminalSessionStart,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                terminal_session_start: Some(request),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn terminal_session_input(
        id: impl Into<String>,
        request: TerminalSessionInputRequest,
    ) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::TerminalSessionInput,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                terminal_session_input: Some(request),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn terminal_session_resize(
        id: impl Into<String>,
        request: TerminalSessionResizeRequest,
    ) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::TerminalSessionResize,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                terminal_session_resize: Some(request),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn terminal_session_stop(
        id: impl Into<String>,
        request: TerminalSessionStopRequest,
    ) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::TerminalSessionStop,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                terminal_session_stop: Some(request),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn terminal_suggestion_request(
        id: impl Into<String>,
        request: TerminalSuggestionRequest,
    ) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::TerminalSuggestionRequest,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                terminal_suggestion_request: Some(request),
                ..EngineRequestParams::default()
            },
        }
    }

    pub fn terminal_agent_turn_start(
        id: impl Into<String>,
        request: TerminalAgentTurnStartRequest,
    ) -> Self {
        Self {
            schema: ENGINE_REQUEST_SCHEMA.to_string(),
            id: id.into(),
            kind: EngineRequestKind::TerminalAgentTurnStart,
            protocol_version: CONTRACT_VERSION.to_string(),
            params: EngineRequestParams {
                terminal_agent_turn_start: Some(request),
                ..EngineRequestParams::default()
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSession {
    pub schema: String,
    pub session_id: String,
    pub cwd_redacted: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    pub env_policy: TerminalEnvPolicy,
    pub cols: u16,
    pub rows: u16,
    pub started_by: TerminalSessionStarter,
    pub started_at_ms: u64,
    pub state: TerminalSessionState,
    pub provider_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_transcript_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shareable_summary_ref: Option<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalSessionState {
    Starting,
    Running,
    Stopped,
    Failed,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalEvent {
    pub schema: String,
    pub event_id: String,
    pub session_id: String,
    pub event_kind: TerminalEventKind,
    pub sequence: u64,
    pub timestamp_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_block: Option<TerminalCommandBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_redacted: Option<String>,
    pub redacted: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalEventKind {
    SessionStarted,
    SessionResized,
    InputAccepted,
    CommandStarted,
    CommandFinished,
    OutputDigest,
    SuggestionCreated,
    RiskDecisionRecorded,
    AgentTurnStarted,
    SessionStopped,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSessionStartRequest {
    pub schema: String,
    pub session_id: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    pub env_policy: TerminalEnvPolicy,
    pub cols: u16,
    pub rows: u16,
    pub started_by: TerminalSessionStarter,
}

impl TerminalSessionStartRequest {
    pub fn normalized_cols(&self) -> u16 {
        self.cols.clamp(20, 500)
    }

    pub fn normalized_rows(&self) -> u16 {
        self.rows.clamp(5, 200)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSessionInputRequest {
    pub schema: String,
    pub session_id: String,
    pub text: String,
    pub input_kind: TerminalInputKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_risk_decision_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSessionResizeRequest {
    pub schema: String,
    pub session_id: String,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSessionStopRequest {
    pub schema: String,
    pub session_id: String,
    pub reason_code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSuggestionRequest {
    pub schema: String,
    pub request_id: String,
    pub cwd: String,
    pub input: String,
    pub cursor: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_exit_code: Option<i32>,
    pub max_suggestions: u8,
    pub include_ai: bool,
    #[serde(default)]
    pub context_refs: Vec<String>,
}

impl TerminalSuggestionRequest {
    pub fn normalized_max_suggestions(&self) -> u8 {
        self.max_suggestions.clamp(1, 20)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSuggestion {
    pub schema: String,
    pub id: String,
    pub replacement: String,
    pub display: String,
    pub description: String,
    pub source: TerminalSuggestionSource,
    pub confidence: f32,
    pub risk: TerminalRiskLevel,
    pub requires_approval: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalCommandBlock {
    pub schema: String,
    pub block_id: String,
    pub session_id: String,
    pub cwd_redacted: String,
    pub command_redacted: String,
    pub started_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_digest: Option<String>,
    pub redacted: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalRiskDecision {
    pub schema: String,
    pub id: String,
    pub command_redacted: String,
    pub risk: TerminalRiskLevel,
    pub decision: TerminalExecutionDecision,
    pub reason_code: String,
    pub requires_approval: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

impl TerminalRiskDecision {
    pub fn default_for_risk(
        id: impl Into<String>,
        command_redacted: impl Into<String>,
        risk: TerminalRiskLevel,
    ) -> Self {
        let decision = match &risk {
            TerminalRiskLevel::Safe => TerminalExecutionDecision::Allow,
            TerminalRiskLevel::Caution => TerminalExecutionDecision::Warn,
            TerminalRiskLevel::SecretExposure => TerminalExecutionDecision::Block,
            TerminalRiskLevel::Destructive
            | TerminalRiskLevel::Privileged
            | TerminalRiskLevel::NetworkMutation
            | TerminalRiskLevel::Unknown => TerminalExecutionDecision::RequireApproval,
        };
        Self::new(
            id,
            command_redacted,
            risk,
            decision,
            "terminal_risk_policy_default",
        )
    }

    pub fn new(
        id: impl Into<String>,
        command_redacted: impl Into<String>,
        risk: TerminalRiskLevel,
        decision: TerminalExecutionDecision,
        reason_code: impl Into<String>,
    ) -> Self {
        let requires_approval = risk.requires_approval_by_default()
            || matches!(
                &decision,
                TerminalExecutionDecision::RequireApproval | TerminalExecutionDecision::Block
            );
        Self {
            schema: TERMINAL_RISK_DECISION_SCHEMA.to_string(),
            id: id.into(),
            command_redacted: command_redacted.into(),
            risk,
            decision,
            reason_code: reason_code.into(),
            requires_approval,
            evidence_refs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalAgentTurnStartRequest {
    pub schema: String,
    pub turn_id: String,
    pub session_id: String,
    pub cwd: String,
    pub prompt: String,
    pub mode: TerminalAgentMode,
    pub allow_command_proposals: bool,
    pub allow_auto_execute_safe_commands: bool,
    #[serde(default)]
    pub context_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalMcpToolDescriptor {
    pub schema: String,
    pub tool_id: String,
    pub display_name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema_ref: Option<String>,
    pub risk: TerminalRiskLevel,
    pub requires_approval: bool,
    pub provider_available: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalEnvPolicy {
    Minimal,
    InheritSafe,
    DenySecrets,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalSessionStarter {
    User,
    SwiftUi,
    Cli,
    Agent,
    Test,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalInputKind {
    UserCommand,
    RawBytes,
    AgentProposedCommand,
    Control,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalSuggestionSource {
    ShellHistory,
    Completion,
    ProjectCatalog,
    FilePath,
    OpenSksContext,
    Provider,
    Fallback,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalRiskLevel {
    Safe,
    Caution,
    Destructive,
    Privileged,
    SecretExposure,
    NetworkMutation,
    #[serde(other)]
    Unknown,
}

impl TerminalRiskLevel {
    pub fn requires_approval_by_default(&self) -> bool {
        matches!(
            self,
            Self::Destructive
                | Self::Privileged
                | Self::SecretExposure
                | Self::NetworkMutation
                | Self::Unknown
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalExecutionDecision {
    Allow,
    Warn,
    RequireApproval,
    Block,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalAgentMode {
    ExplainOnly,
    SuggestCommands,
    DiagnoseFailure,
    PlanThenSuggest,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EngineEvent {
    pub schema: String,
    pub event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub event_type: EngineEventType,
    pub severity: EventSeverity,
    pub message: String,
    pub protocol_version: String,
    pub timestamp_ms: u64,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub redacted: bool,
}

impl EngineEvent {
    pub fn new(
        event_id: impl Into<String>,
        request_id: Option<String>,
        event_type: EngineEventType,
        message: impl Into<String>,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            schema: ENGINE_EVENT_SCHEMA.to_string(),
            event_id: event_id.into(),
            request_id,
            event_type,
            severity: EventSeverity::Info,
            message: message.into(),
            protocol_version: CONTRACT_VERSION.to_string(),
            timestamp_ms,
            evidence_refs: Vec::new(),
            redacted: true,
        }
    }

    pub fn error(
        event_id: impl Into<String>,
        request_id: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: EventSeverity::Error,
            ..Self::new(event_id, request_id, EngineEventType::Error, message, 0)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionEvent {
    pub schema: String,
    pub event_id: String,
    pub run_id: String,
    pub phase: String,
    pub status: ExecutionStatus,
    pub trust_status: TrustStatus,
    pub reason_code: String,
    pub message: String,
    pub timestamp_ms: u64,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionEventEnvelope {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub sequence: u64,
    pub occurred_at: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub kind: EventKind,
    pub payload: serde_json::Value,
    pub sensitivity: Sensitivity,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkItem {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub title: String,
    pub state: ExecutionStatus,
    pub reason_code: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    General,
    Planning,
    Code,
    Verification,
    Vision,
    Image,
    Arbiter,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Unknown,
    Healthy,
    Degraded,
    Unavailable,
    OpenCircuit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct ModelCapabilities {
    pub text: bool,
    pub code: bool,
    pub vision_input: bool,
    pub image_output: bool,
    pub image_edit: bool,
    pub tool_use: bool,
    pub structured_output: bool,
    pub long_context: bool,
    pub streaming: bool,
}

impl ModelCapabilities {
    pub fn text_code() -> Self {
        Self {
            text: true,
            code: true,
            structured_output: true,
            ..Self::default()
        }
    }

    pub fn image() -> Self {
        Self {
            image_output: true,
            vision_input: true,
            ..Self::default()
        }
    }

    pub fn satisfies(&self, required: &CapabilityRequirements) -> bool {
        (!required.text || self.text)
            && (!required.code || self.code)
            && (!required.vision_input || self.vision_input)
            && (!required.image_output || self.image_output)
            && (!required.image_edit || self.image_edit)
            && (!required.tool_use || self.tool_use)
            && (!required.structured_output || self.structured_output)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct CapabilityRequirements {
    pub text: bool,
    pub code: bool,
    pub vision_input: bool,
    pub image_output: bool,
    pub image_edit: bool,
    pub tool_use: bool,
    pub structured_output: bool,
}

impl CapabilityRequirements {
    pub fn text() -> Self {
        Self {
            text: true,
            ..Self::default()
        }
    }

    pub fn code() -> Self {
        Self {
            text: true,
            code: true,
            structured_output: true,
            ..Self::default()
        }
    }

    pub fn image_output() -> Self {
        Self {
            image_output: true,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct ModelLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_per_minute: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_per_minute: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PricingInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_per_million_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_per_million_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_output_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecretlessConfigRef {
    pub source: String,
    pub reference: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SecretStoreKind {
    MacosKeychain,
    ExternalBroker,
    LocalDevelopment,
    TestMemory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecretRef {
    pub schema: String,
    pub store: SecretStoreKind,
    pub service: String,
    pub account: String,
    pub version: u64,
}

impl SecretRef {
    pub fn macos_keychain(
        service: impl Into<String>,
        account: impl Into<String>,
        version: u64,
    ) -> Self {
        Self {
            schema: SECRET_REF_SCHEMA.to_string(),
            store: SecretStoreKind::MacosKeychain,
            service: service.into(),
            account: account.into(),
            version,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenRouter,
    OpenAi,
    CodexLb,
    OpenAiCompatible,
    AnthropicCompatible,
    GoogleCompatible,
    LocalOpenAiCompatible,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderEndpoint {
    pub base_url: String,
    #[serde(default)]
    pub allow_insecure_http: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderHealthSnapshot {
    pub state: HealthState,
    pub circuit_open: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at_ms: Option<u64>,
    pub reason_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic_ref: Option<String>,
}

impl ProviderHealthSnapshot {
    pub fn unknown() -> Self {
        Self {
            state: HealthState::Unknown,
            circuit_open: false,
            checked_at_ms: None,
            reason_code: "not_probed".to_string(),
            diagnostic_ref: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderConcurrencyPolicy {
    pub max_concurrent_requests: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_per_minute: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_per_minute: Option<u64>,
}

impl Default for ProviderConcurrencyPolicy {
    fn default() -> Self {
        Self {
            max_concurrent_requests: 1,
            requests_per_minute: None,
            tokens_per_minute: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderConnection {
    pub schema: String,
    pub id: String,
    pub kind: ProviderKind,
    pub display_name: String,
    pub enabled: bool,
    pub endpoint: ProviderEndpoint,
    pub auth: SecretRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_ref: Option<String>,
    pub health: ProviderHealthSnapshot,
    pub concurrency: ProviderConcurrencyPolicy,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelCatalogEntry {
    pub schema: String,
    pub id: String,
    pub provider_id: String,
    pub remote_model_id: String,
    pub display_name: String,
    pub enabled: bool,
    pub capabilities: ModelCapabilities,
    pub limits: ModelLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<PricingInfo>,
    pub health: HealthState,
    #[serde(default)]
    pub role_scores: BTreeMap<ModelRole, RoleScore>,
    pub catalog_revision: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProbeHttpCategory {
    NotSent,
    Success,
    AuthRejected,
    RateLimited,
    ClientError,
    ServerError,
    NetworkError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LatencyBucket {
    NotMeasured,
    Under250Ms,
    Under1S,
    Under5S,
    Over5S,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderProbeReceipt {
    pub schema: String,
    pub provider_id: String,
    pub endpoint_host_redacted: String,
    pub http_category: ProviderProbeHttpCategory,
    pub latency_bucket: LatencyBucket,
    pub auth_accepted: bool,
    pub model_list_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_count: Option<u32>,
    pub occurred_at_ms: u64,
    pub reason_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractTimestamp {
    pub unix_seconds: i64,
    pub nanos: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderAdapterCheckSummary {
    pub total: usize,
    pub attempted: usize,
    pub reachable: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderAdapterRemediationAction {
    pub blocker: String,
    pub action: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderAdapterCheckRow {
    pub name: String,
    pub configured: bool,
    pub attempted: bool,
    pub status: String,
    #[serde(default)]
    pub blockers: Vec<String>,
    pub credential_source: String,
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_code: Option<String>,
    pub duration_ms: u128,
    pub transport: String,
    pub secret_value_exposed: bool,
    #[serde(default)]
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderAdapterCheckReport {
    pub schema: String,
    pub generated_at: ContractTimestamp,
    pub remote_probe_opt_in: bool,
    pub secret_value_exposed: bool,
    pub summary: ProviderAdapterCheckSummary,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub remediation_actions: Vec<ProviderAdapterRemediationAction>,
    #[serde(default)]
    pub adapters: Vec<ProviderAdapterCheckRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderMockE2eCheck {
    pub id: String,
    pub status: TrustStatus,
    pub evidence_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderMockE2eReport {
    pub schema: String,
    pub generated_at: ContractTimestamp,
    pub status: TrustStatus,
    pub fixture_kind: String,
    pub live_vendor_calls_performed: bool,
    pub secret_value_exposed: bool,
    pub provider_id: String,
    pub model_id: String,
    pub model_catalog_count: usize,
    pub model_catalog_synced: bool,
    pub model_enabled: bool,
    pub registry_route_status: RoutingStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_receipt: Option<ModelRouteReceipt>,
    #[serde(default)]
    pub checks: Vec<ProviderMockE2eCheck>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderMutationKind {
    Created,
    Updated,
    CredentialReplaced,
    CredentialDeleted,
    Enabled,
    Disabled,
    Deleted,
    ModelsSynced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderMutationReceipt {
    pub schema: String,
    pub provider_id: String,
    pub mutation: ProviderMutationKind,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<SecretRef>,
    pub secret_value_exposed: bool,
    pub occurred_at_ms: u64,
    pub reason_code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RoleScore {
    pub score: f64,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelProfile {
    pub schema: String,
    pub id: String,
    pub provider_id: String,
    pub display_name: String,
    pub enabled: bool,
    pub capabilities: ModelCapabilities,
    pub limits: ModelLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<PricingInfo>,
    pub health: HealthState,
    #[serde(default)]
    pub role_scores: BTreeMap<ModelRole, RoleScore>,
    #[serde(default)]
    pub user_tags: Vec<String>,
    pub config_ref: SecretlessConfigRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderDescriptor {
    pub schema: String,
    pub id: String,
    pub display_name: String,
    pub enabled: bool,
    pub capabilities: ModelCapabilities,
    pub health: HealthState,
    pub config_ref: SecretlessConfigRef,
    pub secret_value_exposed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoutingStatus {
    /// A user/thread setting requested a model or auto route, but no registry
    /// snapshot has validated provider/model/capability/health yet.
    Requested,
    /// A registry snapshot found an enabled compatible provider/model.
    Resolved,
    /// Secret resolution, policy, health/circuit, and concurrency admission are
    /// all satisfied; the request can be dispatched.
    DispatchReady,
    /// A provider request was actually sent.
    Dispatched,
    /// Legacy compatibility label. New product paths should prefer the staged
    /// statuses above and only use this for old receipts already persisted.
    Routed,
    BlockedMissingCapability,
    BlockedDisabled,
    BlockedPolicy,
    BlockedProviderHealth,
}

impl RoutingStatus {
    pub fn has_resolved_model(&self) -> bool {
        matches!(
            self,
            Self::Resolved | Self::DispatchReady | Self::Dispatched | Self::Routed
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelRejection {
    pub model_id: String,
    pub reason_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelRouteReceipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    pub registry_revision: String,
    pub reason_code: String,
    pub requested_capabilities: CapabilityRequirements,
    pub effective_limits: ModelLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_index: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoutingDecision {
    pub schema: String,
    pub id: String,
    pub status: RoutingStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_model_id: Option<String>,
    pub reason_code: String,
    #[serde(default)]
    pub eligible_model_ids: Vec<String>,
    #[serde(default)]
    pub rejected_models: Vec<ModelRejection>,
    pub model_snapshot_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_receipt: Option<ModelRouteReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    Draft,
    Ready,
    Leased,
    Dispatched,
    Running,
    ResultReceived,
    Verifying,
    AwaitingApproval,
    Applying,
    Completed,
    RetryWait,
    Blocked,
    Failed,
    Cancelled,
    Superseded,
}

impl WorkState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Blocked | Self::Failed | Self::Cancelled | Self::Superseded
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkKind {
    Planning,
    ModelInference,
    ToolExecution,
    WriteCandidate,
    Verification,
    Integration,
    Approval,
    ExternalSideEffect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct PathScope {
    #[serde(default)]
    pub workspace_relative_roots: Vec<String>,
    pub allow_external_write: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkBudget {
    pub max_attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
}

impl Default for WorkBudget {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            timeout_ms: None,
            max_cost_usd: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff_ms: u64,
    #[serde(default)]
    pub retryable_reason_codes: Vec<String>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            backoff_ms: 0,
            retryable_reason_codes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LeaseType {
    PathWrite,
    GitWorktree,
    ProviderSlot,
    ToolSession,
    BrowserSession,
    Approval,
    Integration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Lease {
    pub id: String,
    pub lease_type: LeaseType,
    pub holder: String,
    pub acquired_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_heartbeat_at_ms: Option<u64>,
    pub ttl_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SchedulerWorkItem {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub kind: WorkKind,
    pub priority: i32,
    pub state: WorkState,
    #[serde(default)]
    pub dependencies: Vec<String>,
    pub capability_requirements: CapabilityRequirements,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pack_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_context_pack_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_selection_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_required_source_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_required_verifier_count: Option<usize>,
    pub path_scope: PathScope,
    pub budget: WorkBudget,
    pub retry: RetryPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<Lease>,
    pub idempotency_key: String,
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConcurrencyDecision {
    pub schema: String,
    pub requested: u32,
    pub admitted: u32,
    pub visible_lanes: u32,
    pub headless_lanes: u32,
    #[serde(default)]
    pub limits: BTreeMap<String, u32>,
    #[serde(default)]
    pub backpressure: Vec<String>,
    pub sampled_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SchedulerSnapshot {
    pub schema: String,
    pub run_id: String,
    #[serde(default)]
    pub work_items: Vec<SchedulerWorkItem>,
    pub decision: ConcurrencyDecision,
    pub overlap_ratio: f64,
    pub max_concurrent_workers: u32,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

/// Result of the PR-043 high-rate stress harness: a deterministic report proving
/// the bounded event batcher + LRU cache stay within their configured retention
/// budget regardless of input size, lose nothing silently (only counted drops),
/// and that the supervised run reaped every child with zero leaked handles.
///
/// The invariant `within_budget` is `true` iff `peak_retained <= retention_cap`
/// AND `processed + dropped == events` AND `children_reaped == children_spawned`
/// AND `leaked_handles == 0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PerfStressReport {
    pub schema: String,
    /// Number of synthetic events fed into the harness.
    pub events: u64,
    /// Events that flowed all the way through the batcher to the sink.
    pub processed: u64,
    /// Events dropped by an explicit, counted overflow policy (never silent).
    pub dropped: u64,
    /// Hard cap on retained items (cache capacity + the largest in-flight batch),
    /// expressed as a single item budget for the wire contract.
    pub retention_cap: u64,
    /// High-water mark of simultaneously retained items across the whole run.
    pub peak_retained: u64,
    /// Children spawned by the supervisor during the run.
    pub children_spawned: u64,
    /// Children deterministically reaped by the supervisor (must equal spawned).
    pub children_reaped: u64,
    /// Live OS handles still registered after the supervised run drained. Zero
    /// proves no orphaned process / leaked file descriptor.
    pub leaked_handles: u64,
    /// The single load-bearing invariant the harness asserts.
    pub within_budget: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    GoalInput,
    RequirementExtractor,
    RequirementGate,
    Branch,
    Switch,
    JoinAll,
    JoinAny,
    Loop,
    Delay,
    Queue,
    Approval,
    Breakpoint,
    Subgraph,
    FinalSeal,
    CodeGraphQuery,
    ContextPack,
    TriWikiRecall,
    WrongnessRecall,
    GlossaryQuery,
    ArchitectureSnapshot,
    WebResearch,
    McpResource,
    ReasoningStrategy,
    SocraticReview,
    Debate,
    RedTeam,
    Consensus,
    Arbiter,
    Decompose,
    Critique,
    Synthesize,
    ModelCall,
    Delegate,
    CandidatePool,
    WorkerPool,
    VerifierPool,
    RoleRouter,
    FallbackRouter,
    Quorum,
    ReadFiles,
    SearchCode,
    RunCommand,
    McpTool,
    Skill,
    GeneratePatch,
    ApplyPatch,
    RunTests,
    StaticAnalysis,
    SecurityScan,
    ImageGenerate,
    ImageEdit,
    ImageVariation,
    ScreenshotCapture,
    VisualReview,
    ImageVoxelAnchor,
    BeforeAfterCompare,
    GitStatus,
    GitDiff,
    GitWorktree,
    GitStage,
    GitCommit,
    GitPush,
    PullRequest,
    BrowserObserve,
    BrowserAction,
    AppInspect,
    AppAction,
    ComputerObserve,
    ComputerAction,
    Cancelled,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Data,
    Control,
    Error,
    Evidence,
    Cancellation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PortType {
    String,
    Boolean,
    Integer,
    Float,
    Json,
    FileRef,
    FileSet,
    SymbolRef,
    CodeGraphQuery,
    ContextPack,
    RequirementSet,
    WorkItemSet,
    ModelSelector,
    ModelResponse,
    PatchEnvelope,
    VerificationResult,
    ImageRef,
    ImageSet,
    VisualAnchorSet,
    GitDiff,
    GitCommitRef,
    EvidenceRef,
    ProofRef,
    Error,
    Control,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PipelineGraph {
    pub schema: String,
    pub id: String,
    pub name: String,
    pub version: u32,
    #[serde(default)]
    pub entry_nodes: Vec<String>,
    #[serde(default)]
    pub nodes: BTreeMap<String, NodeSpec>,
    #[serde(default)]
    pub edges: Vec<EdgeSpec>,
    #[serde(default)]
    pub variables: BTreeMap<String, GraphValue>,
    pub policies: GraphPolicies,
    pub metadata: GraphMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NodeSpec {
    pub id: String,
    pub kind: NodeKind,
    pub display_name: String,
    pub enabled: bool,
    pub position: GraphPoint,
    #[serde(default)]
    pub inputs: BTreeMap<String, PortBinding>,
    pub config: serde_json::Value,
    pub retry: RetryPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    pub approval: ApprovalPolicy,
    #[serde(default)]
    pub hook_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EdgeSpec {
    pub id: String,
    pub from: PortRef,
    pub to: PortRef,
    pub kind: EdgeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<Expression>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PortRef {
    pub node_id: String,
    pub port: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PortBinding {
    pub port_type: PortType,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PortRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GraphValue {
    pub value_type: PortType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GraphPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalPolicy {
    pub required: bool,
    pub scope: String,
}

impl ApprovalPolicy {
    pub fn none() -> Self {
        Self {
            required: false,
            scope: "none".to_string(),
        }
    }

    pub fn required(scope: impl Into<String>) -> Self {
        Self {
            required: true,
            scope: scope.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GraphPolicies {
    pub max_parallelism: u32,
    pub allow_external_side_effects: bool,
    pub final_seal_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GraphMetadata {
    pub description: String,
    pub created_by: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObjectivePlanRequest {
    pub schema: String,
    pub objective: String,
    #[serde(default = "default_objective_plan_max_parallelism")]
    pub max_parallelism: u32,
    #[serde(default = "default_objective_plan_role_count")]
    pub role_count: u32,
    #[serde(default = "default_true")]
    pub require_git_worktree: bool,
    #[serde(default = "default_true")]
    pub require_integration_approval: bool,
    #[serde(default)]
    pub include_image_lane: bool,
    #[serde(default)]
    pub include_research_lane: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

impl ObjectivePlanRequest {
    pub fn new(objective: impl Into<String>) -> Self {
        Self {
            schema: OBJECTIVE_PLAN_REQUEST_SCHEMA.to_string(),
            objective: objective.into(),
            max_parallelism: default_objective_plan_max_parallelism(),
            role_count: default_objective_plan_role_count(),
            require_git_worktree: true,
            require_integration_approval: true,
            include_image_lane: false,
            include_research_lane: false,
            evidence_refs: Vec::new(),
        }
    }
}

fn default_objective_plan_max_parallelism() -> u32 {
    8
}

fn default_objective_plan_role_count() -> u32 {
    4
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObjectivePlanReceipt {
    pub schema: String,
    pub objective_hash: String,
    pub graph_id: String,
    pub graph_hash: String,
    pub plan_hash: String,
    pub source: String,
    pub max_parallelism: u32,
    pub role_count: u32,
    pub work_template_count: u32,
    pub repair_action_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy: Option<PlannerShardPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiled_plan_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner_provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner_model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner_response_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner_response_bytes: Option<usize>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PlannerShardPolicy {
    pub schema: String,
    pub id: String,
    pub source: String,
    pub role_count: u32,
    pub max_parallelism: u32,
    pub implementation_shard_count: u32,
    pub verifier_shard_count: u32,
    pub candidate_selection_policy: String,
    #[serde(default)]
    pub required_gates: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Expression {
    pub language: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompiledPlan {
    pub schema: String,
    pub graph_id: String,
    pub graph_version: u32,
    pub graph_hash: String,
    pub plan_hash: String,
    #[serde(default)]
    pub work_templates: Vec<WorkTemplate>,
    pub dependency_index: DependencyIndex,
    pub resource_plan: ResourcePlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy: Option<PlannerShardPolicy>,
    #[serde(default)]
    pub approval_points: Vec<ApprovalPoint>,
    pub proof_contract: CompletionContract,
    #[serde(default)]
    pub diagnostics: Vec<CompileDiagnostic>,
    #[serde(default)]
    pub repair_plan: CompileRepairPlan,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkTemplate {
    pub id: String,
    pub node_id: String,
    pub kind: WorkKind,
    #[serde(default)]
    pub dependencies: Vec<String>,
    pub capability_requirements: CapabilityRequirements,
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_selection_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_required_source_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_required_verifier_count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DependencyIndex {
    #[serde(default)]
    pub prerequisites: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResourcePlan {
    pub max_parallelism: u32,
    pub requires_git_worktree: bool,
    pub requires_image: bool,
    pub requires_external_side_effect_approval: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalPoint {
    pub node_id: String,
    pub scope: String,
    pub reason_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CompletionContract {
    #[serde(default)]
    pub required_requirement_ids: Vec<String>,
    #[serde(default)]
    pub requirements: Vec<ProofRequirement>,
    #[serde(default)]
    pub final_seal_node_ids: Vec<String>,
    pub evidence_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProofRequirement {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_node_id: Option<String>,
    pub description: String,
    pub evidence_required: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CompileDiagnostic {
    pub severity: DiagnosticSeverity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_id: Option<String>,
    pub reason_code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CompileRepairPlan {
    pub bounded: bool,
    pub max_iterations: u32,
    pub reason_code: String,
    #[serde(default)]
    pub actions: Vec<CompileRepairAction>,
    #[serde(default)]
    pub groups: Vec<CompileRepairGroup>,
}

impl Default for CompileRepairPlan {
    fn default() -> Self {
        Self {
            bounded: true,
            max_iterations: 0,
            reason_code: "no_repairs_required".to_string(),
            actions: Vec::new(),
            groups: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CompileRepairAction {
    pub id: String,
    pub diagnostic_reason_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_edge_id: Option<String>,
    pub action: String,
    pub description: String,
    pub evidence_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CompileRepairGroup {
    pub id: String,
    pub group_kind: String,
    pub description: String,
    #[serde(default)]
    pub action_ids: Vec<String>,
    #[serde(default)]
    pub diagnostic_reason_codes: Vec<String>,
    #[serde(default)]
    pub node_ids: Vec<String>,
    #[serde(default)]
    pub edge_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    GitWorktree,
    Snapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitIsolationReport {
    pub schema: String,
    pub id: String,
    pub mode: IsolationMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    pub worktree_path: String,
    pub git_available: bool,
    pub reason_code: String,
    pub submodule_detected: bool,
    pub lfs_detected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorktreeIsolationInventoryEntry {
    pub isolation_id: String,
    pub run_id: String,
    pub worker_id: String,
    pub mode: IsolationMode,
    pub artifact_ref: String,
    pub exists: bool,
    pub has_git_metadata: bool,
    pub path_redacted: bool,
    pub content_redacted: bool,
    pub reason_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorktreeIsolationInventoryReceipt {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub state: String,
    pub reason_code: String,
    pub inventory_ref: String,
    #[serde(default)]
    pub isolations: Vec<WorktreeIsolationInventoryEntry>,
    pub isolation_count: usize,
    pub git_available: bool,
    pub path_redacted: bool,
    pub content_redacted: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub generated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorktreeIsolationRecoveryTarget {
    pub isolation_id: String,
    pub run_id: String,
    pub worker_id: String,
    pub mode: IsolationMode,
    pub artifact_ref: String,
    pub existed: bool,
    pub removed: bool,
    pub reason_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorktreeIsolationRecoveryReceipt {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub state: String,
    pub reason_code: String,
    pub inventory_ref: String,
    pub recovery_ref: String,
    #[serde(default)]
    pub targets: Vec<WorktreeIsolationRecoveryTarget>,
    pub target_count: usize,
    pub recovered_count: usize,
    pub prune_attempted: bool,
    pub prune_succeeded: bool,
    pub path_redacted: bool,
    pub content_redacted: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub generated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    Passed,
    Failed,
    Blocked,
    Pending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GateResult {
    pub status: GateStatus,
    pub reason_code: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub secret_value_exposed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProducerRef {
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PatchEnvelope {
    pub schema: String,
    pub id: String,
    pub work_item_id: String,
    pub lease_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    #[serde(default)]
    pub target_paths: Vec<String>,
    #[serde(default)]
    pub before_hashes: BTreeMap<String, String>,
    #[serde(default)]
    pub after_hashes: BTreeMap<String, String>,
    pub unified_diff_ref: String,
    pub rollback_ref: String,
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    pub producer: ProducerRef,
    pub secret_scan: GateResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoleSubcontractCandidateReceipt {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub work_item_id: String,
    pub role: String,
    pub worker_id: String,
    pub state: String,
    pub reason_code: String,
    pub source_isolation_id: String,
    pub source_isolation_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_base_commit: Option<String>,
    pub source_git_available: bool,
    #[serde(default)]
    pub target_paths: Vec<String>,
    pub patch_count: usize,
    pub apply_result_count: usize,
    #[serde(default)]
    pub applied_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_selection_policy: Option<String>,
    #[serde(default)]
    pub planner_required_source_candidate_count: usize,
    #[serde(default)]
    pub planner_required_verifier_count: usize,
    pub receipt_ref: String,
    pub patch_ref: String,
    pub main_workspace_modified: bool,
    pub integration_required: bool,
    pub approval_required: bool,
    pub path_redacted: bool,
    pub content_redacted: bool,
    pub generated_at_ms: u64,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SemanticVerifierJudgmentReceipt {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub work_item_id: String,
    pub role: String,
    pub worker_id: String,
    pub state: String,
    pub reason_code: String,
    pub verifier_kind: String,
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub passed_gates: Vec<String>,
    #[serde(default)]
    pub failed_gates: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    pub response_hash: String,
    pub response_bytes: usize,
    pub judgment_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pack_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_context_pack_ref: Option<String>,
    pub path_redacted: bool,
    pub content_redacted: bool,
    pub generated_at_ms: u64,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IntegrationSourceCandidateRef {
    pub source: String,
    pub id: String,
    pub receipt_ref: String,
    pub patch_ref: String,
    pub worker_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_isolation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_isolation_mode: Option<String>,
    #[serde(default)]
    pub target_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_selection_policy: Option<String>,
    #[serde(default)]
    pub planner_required_source_candidate_count: usize,
    #[serde(default)]
    pub planner_required_verifier_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IntegrationTurnSettingsSnapshot {
    pub model: turn::ModelSelection,
    pub reasoning_effort: turn::ReasoningEffort,
    pub execution_mode: turn::ExecutionMode,
    pub pipeline_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_revision: Option<String>,
    pub max_parallelism: u32,
    pub verifier_count: u32,
    pub tool_policy_id: String,
    pub approval_policy_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_budget_usd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_model_id: Option<String>,
}

impl From<&turn::ConversationTurnSettings> for IntegrationTurnSettingsSnapshot {
    fn from(settings: &turn::ConversationTurnSettings) -> Self {
        Self {
            model: settings.model.clone(),
            reasoning_effort: settings.reasoning_effort,
            execution_mode: settings.execution_mode,
            pipeline_id: settings.pipeline_id.clone(),
            graph_revision: settings.graph_revision.clone(),
            max_parallelism: settings.max_parallelism,
            verifier_count: settings.verifier_count,
            tool_policy_id: settings.tool_policy_id.clone(),
            approval_policy_id: settings.approval_policy_id.clone(),
            token_budget: settings.token_budget,
            cost_budget_usd: settings.cost_budget_usd.map(|budget| budget.to_string()),
            timeout_ms: settings.timeout_ms,
            image_model_id: settings.image_model_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IntegrationCandidateReceipt {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub turn_id: String,
    pub conversation_id: String,
    pub project_id: String,
    pub worker_id: String,
    pub state: String,
    pub reason_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_isolation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_isolation_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_base_commit: Option<String>,
    pub source_git_available: bool,
    #[serde(default)]
    pub source_candidates: Vec<IntegrationSourceCandidateRef>,
    #[serde(default)]
    pub aggregate_candidate_count: usize,
    #[serde(default)]
    pub aggregate_target_count: usize,
    #[serde(default)]
    pub planned_verifier_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_settings: Option<IntegrationTurnSettingsSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_selection_policy: Option<String>,
    #[serde(default)]
    pub planner_required_source_candidate_count: usize,
    #[serde(default)]
    pub planner_selected_source_candidate_count: usize,
    #[serde(default)]
    pub planner_required_verifier_count: usize,
    #[serde(default)]
    pub target_paths: Vec<String>,
    pub patch_count: usize,
    pub apply_result_count: usize,
    #[serde(default)]
    pub applied_files: Vec<String>,
    pub receipt_ref: String,
    pub patch_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_ref: Option<String>,
    pub main_workspace_modified: bool,
    pub integration_required: bool,
    pub approval_required: bool,
    pub path_redacted: bool,
    pub content_redacted: bool,
    pub generated_at_ms: u64,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IntegrationCandidateSelectionReceipt {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub selected_candidate_id: String,
    pub selection_ref: String,
    pub selected_candidate_ref: String,
    pub selected_patch_ref: String,
    #[serde(default)]
    pub candidate_pool: Vec<IntegrationSourceCandidateRef>,
    #[serde(default)]
    pub selected_source_candidate_ids: Vec<String>,
    pub selection_policy: String,
    pub reason_code: String,
    #[serde(default)]
    pub required_verification_gates: Vec<String>,
    #[serde(default)]
    pub aggregate_candidate_count: usize,
    #[serde(default)]
    pub aggregate_target_count: usize,
    #[serde(default)]
    pub planned_verifier_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_settings: Option<IntegrationTurnSettingsSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_policy_selection_policy: Option<String>,
    #[serde(default)]
    pub planner_required_source_candidate_count: usize,
    #[serde(default)]
    pub planner_selected_source_candidate_count: usize,
    #[serde(default)]
    pub planner_required_verifier_count: usize,
    #[serde(default)]
    pub target_paths: Vec<String>,
    pub path_redacted: bool,
    pub content_redacted: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub generated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IntegrationVerifierLaneReceipt {
    pub id: String,
    pub lane_index: usize,
    pub worker_id: String,
    pub verifier_kind: String,
    pub state: String,
    pub reason_code: String,
    #[serde(default)]
    pub passed_gates: Vec<String>,
    #[serde(default)]
    pub failed_gates: Vec<String>,
    pub path_redacted: bool,
    pub content_redacted: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub generated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IntegrationVerificationReceipt {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub candidate_id: String,
    pub state: String,
    pub reason_code: String,
    #[serde(default)]
    pub target_paths: Vec<String>,
    #[serde(default)]
    pub passed_gates: Vec<String>,
    #[serde(default)]
    pub failed_gates: Vec<String>,
    #[serde(default)]
    pub planned_verifier_count: usize,
    #[serde(default)]
    pub passed_verifier_count: usize,
    #[serde(default)]
    pub failed_verifier_count: usize,
    #[serde(default)]
    pub verifier_lanes: Vec<IntegrationVerifierLaneReceipt>,
    pub candidate_ref: String,
    pub patch_ref: String,
    pub verification_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_ref: Option<String>,
    pub path_redacted: bool,
    pub content_redacted: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub generated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IntegrationApplyReceipt {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub candidate_id: String,
    pub state: String,
    pub reason_code: String,
    #[serde(default)]
    pub target_paths: Vec<String>,
    pub approval_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_settings: Option<IntegrationTurnSettingsSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    pub candidate_ref: String,
    pub patch_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_ref: Option<String>,
    pub receipt_ref: String,
    pub integration_ref: String,
    pub final_diff_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seal_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_ref: Option<String>,
    pub main_workspace_modified: bool,
    pub verifier_passed: bool,
    pub path_redacted: bool,
    pub content_redacted: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub generated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IntegrationRepairItem {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub candidate_id: String,
    pub state: String,
    pub reason_code: String,
    #[serde(default)]
    pub target_paths: Vec<String>,
    #[serde(default)]
    pub conflict_paths: Vec<String>,
    pub candidate_ref: String,
    pub patch_ref: String,
    pub integration_ref: String,
    pub repair_ref: String,
    #[serde(default)]
    pub suggested_actions: Vec<String>,
    pub path_redacted: bool,
    pub content_redacted: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub generated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IntegrationFinalSeal {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub candidate_id: String,
    pub state: String,
    pub reason_code: String,
    #[serde(default)]
    pub target_paths: Vec<String>,
    #[serde(default)]
    pub passed_gates: Vec<String>,
    #[serde(default)]
    pub failed_gates: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_settings: Option<IntegrationTurnSettingsSnapshot>,
    pub candidate_ref: String,
    pub patch_ref: String,
    pub verification_ref: String,
    pub integration_ref: String,
    pub final_diff_ref: String,
    pub seal_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_ref: Option<String>,
    pub path_redacted: bool,
    pub content_redacted: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub generated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IntegrationCleanupTarget {
    pub source_isolation_id: String,
    pub source_isolation_mode: String,
    pub worker_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    pub existed: bool,
    pub removed: bool,
    pub reason_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IntegrationCleanupReceipt {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub candidate_id: String,
    pub state: String,
    pub reason_code: String,
    pub integration_ref: String,
    pub seal_ref: String,
    pub cleanup_ref: String,
    #[serde(default)]
    pub source_isolations: Vec<IntegrationCleanupTarget>,
    pub cleanup_target_count: usize,
    pub cleaned_count: usize,
    pub retained_candidate_ref: String,
    pub retained_patch_ref: String,
    pub retained_final_diff_ref: String,
    pub path_redacted: bool,
    pub content_redacted: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub generated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CompletionProof {
    pub schema: String,
    pub id: String,
    pub run_id: String,
    pub status: TrustStatus,
    #[serde(default)]
    pub claims: Vec<ProofClaim>,
    pub coverage: CoverageSeal,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub generated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProofClaim {
    pub id: String,
    pub text: String,
    pub status: TrustStatus,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CoverageSeal {
    pub required: u32,
    pub covered: u32,
    #[serde(default)]
    pub uncovered_requirement_ids: Vec<String>,
    pub final_seal_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HookPhase {
    BeforeRun,
    BeforeNode,
    BeforeProviderCall,
    BeforeToolCall,
    BeforeApplyPatch,
    BeforeExternalWrite,
    AfterNode,
    AfterRun,
    OnError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HookAction {
    Allow,
    Block,
    Modify,
    Redirect,
    Retry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HookSpec {
    pub schema: String,
    pub id: String,
    pub phase: HookPhase,
    pub order: i32,
    pub enabled: bool,
    pub timeout_ms: u64,
    pub allow_secret_read: bool,
    pub action: HookAction,
    pub reason_code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HookInvocation {
    pub schema: String,
    pub id: String,
    pub phase: HookPhase,
    pub run_id: String,
    pub payload: serde_json::Value,
    pub sensitivity: Sensitivity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HookDecision {
    pub schema: String,
    pub invocation_id: String,
    pub hook_id: String,
    pub phase: HookPhase,
    pub action: HookAction,
    pub reason_code: String,
    pub payload: serde_json::Value,
    pub redacted: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeGraphNodeKind {
    File,
    Symbol,
    Import,
    Test,
    Route,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeGraphEdgeKind {
    Contains,
    Imports,
    Calls,
    Tests,
    References,
    OwnsRoute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeGraphEdge {
    pub from_id: String,
    pub to_id: String,
    pub kind: CodeGraphEdgeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeGraphRecord {
    pub schema: String,
    pub id: String,
    pub kind: CodeGraphNodeKind,
    pub path: String,
    pub name: String,
    pub line: u32,
    pub content_hash: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeGraphIndex {
    pub schema: String,
    pub workspace_fingerprint: String,
    #[serde(default)]
    pub records: Vec<CodeGraphRecord>,
    #[serde(default)]
    pub edges: Vec<CodeGraphEdge>,
    pub freshness: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TriWikiRecordKind {
    Claim,
    Decision,
    Glossary,
    Wrongness,
    Architecture,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TriWikiRecord {
    pub schema: String,
    pub id: String,
    pub kind: TriWikiRecordKind,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub shard: String,
    pub redacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TurnContextItem {
    pub ref_id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_hash: Option<String>,
    pub resolved: bool,
    pub stale: bool,
    pub redacted: bool,
    pub reason_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextPackConversationSummary {
    pub conversation_id: String,
    pub summary_redacted: String,
    pub source_message_sequence: i64,
    pub generated_at_ms: u64,
    pub redacted: bool,
    pub reason_code: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextPackBranchFreshness {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_base: Option<String>,
    #[serde(default)]
    pub changed_paths_since_base: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ahead_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behind_count: Option<u32>,
    pub reason_code: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextPack {
    pub schema: String,
    pub id: String,
    pub token_budget: u32,
    pub estimated_tokens: u32,
    #[serde(default)]
    pub record_ids: Vec<String>,
    #[serde(default)]
    pub codegraph_record_ids: Vec<String>,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub selected_test_targets: Vec<String>,
    #[serde(default)]
    pub turn_context_refs: Vec<String>,
    #[serde(default)]
    pub turn_context_items: Vec<TurnContextItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_summary: Option<ContextPackConversationSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_freshness: Option<ContextPackBranchFreshness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<intel::FreshnessStamp>,
    pub body: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VisualAnchor {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageOperation {
    Generate,
    Inspect,
    Edit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImageProvenanceReceipt {
    pub schema: String,
    pub asset_id: String,
    pub operation: ImageOperation,
    pub provider_id: String,
    pub model_id: String,
    pub content_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_hash: Option<String>,
    pub provenance_hash: String,
    pub route_receipt: ModelRouteReceipt,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImageAsset {
    pub schema: String,
    pub id: String,
    pub provider_id: String,
    pub model_id: String,
    pub path: String,
    pub content_hash: String,
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_asset_id: Option<String>,
    #[serde(default)]
    pub anchors: Vec<VisualAnchor>,
    pub temporary: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_receipt: Option<ModelRouteReceipt>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImageLedger {
    pub schema: String,
    #[serde(default)]
    pub assets: Vec<ImageAsset>,
    #[serde(default)]
    pub provenance_receipts: Vec<ImageProvenanceReceipt>,
    #[serde(default)]
    pub gc_candidate_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReasoningClaim {
    pub id: String,
    pub role: String,
    pub claim: String,
    pub evidence: String,
    pub counterexample: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReasoningReport {
    pub schema: String,
    pub id: String,
    pub strategy: String,
    pub rounds: u32,
    pub max_rounds: u32,
    pub status: TrustStatus,
    #[serde(default)]
    pub claims: Vec<ReasoningClaim>,
    pub hidden_reasoning_persisted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutboxAction {
    Commit,
    Push,
    PullRequest,
    ExternalSend,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutboxItem {
    pub schema: String,
    pub id: String,
    pub action: OutboxAction,
    pub target: String,
    pub approval_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    pub protected_branch: bool,
    pub idempotency_key: String,
    pub state: String,
    #[serde(default)]
    pub attempt_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reason_code: Option<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutboxApproval {
    pub approval_id: String,
    pub scope: String,
    pub target: String,
    pub approved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutboxDispatchReport {
    pub schema: String,
    pub item_id: String,
    pub action: OutboxAction,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    pub executed: bool,
    pub state: String,
    pub reason_code: String,
    pub attempt_count: u32,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DataPlane {
    SharedDurable,
    LocalDurable,
    EphemeralLocal,
    SecretLocal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GitTrackingPolicy {
    Track,
    Ignore,
    NeverTrack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DataPlanePathRule {
    pub path: String,
    pub plane: DataPlane,
    pub git_tracking: GitTrackingPolicy,
    pub retention: String,
    pub contains_secrets: bool,
    pub allows_machine_absolute_paths: bool,
    pub allows_raw_provider_responses: bool,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DataPlaneManifest {
    pub schema: String,
    pub version: String,
    pub managed_by: String,
    pub default_gitignore_block_ref: String,
    #[serde(default)]
    pub shared_paths: Vec<DataPlanePathRule>,
    #[serde(default)]
    pub local_paths: Vec<DataPlanePathRule>,
    #[serde(default)]
    pub invariants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RetentionPlan {
    pub schema: String,
    #[serde(default)]
    pub delete_paths: Vec<String>,
    #[serde(default)]
    pub keep_paths: Vec<String>,
    #[serde(default)]
    pub blocked_paths: Vec<String>,
    pub active_run_protected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseArtifactDigest {
    pub name: String,
    pub path: String,
    pub required: bool,
    pub present: bool,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub source_commit_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseProofBlocker {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseRemediationAction {
    pub blocker: String,
    pub action: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseSigningEvidence {
    pub checked: bool,
    pub app_bundle_path: String,
    #[serde(default)]
    pub identifier: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub team_identifier: Option<String>,
    #[serde(default)]
    pub cd_hash: Option<String>,
    pub production_signed: bool,
    pub notarized: bool,
    #[serde(default)]
    pub codesign_status: Option<i32>,
    #[serde(default)]
    pub notarization_status: Option<i32>,
    #[serde(default)]
    pub diagnostic: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseProof {
    pub schema: String,
    pub version: String,
    #[serde(default)]
    pub source_commit_sha: Option<String>,
    #[serde(default)]
    pub workspace_dirty: bool,
    #[serde(default)]
    pub artifact_digests: Vec<ReleaseArtifactDigest>,
    #[serde(default)]
    pub missing_artifacts: Vec<String>,
    #[serde(default)]
    pub same_sha_artifact_binding: bool,
    #[serde(default)]
    pub artifact_digest_gate_passed: bool,
    #[serde(default)]
    pub blockers: Vec<ReleaseProofBlocker>,
    #[serde(default)]
    pub remediation_actions: Vec<ReleaseRemediationAction>,
    #[serde(default)]
    pub signing_evidence: Option<ReleaseSigningEvidence>,
    pub signed_app: bool,
    pub notarized: bool,
    pub rollback_plan_ref: String,
    pub fresh_install_checked: bool,
    pub fresh_clone_checked: bool,
    pub upgrade_checked: bool,
    pub status: TrustStatus,
}

pub mod agent;
pub mod capability;
pub mod conversation;
pub mod design;
pub mod diagnostic;
pub mod file;
pub mod git;
pub mod git_mutation;
pub mod git_push;
pub mod intel;
pub mod patch;
pub mod project;
pub mod projection;
pub mod security;
pub mod stream;
pub mod text_diff;
pub mod topology;
pub mod turn;
pub mod vault;

pub use agent::{
    AGENT_ADAPTER_DESCRIPTOR_SCHEMA, AGENT_EVENT_ENVELOPE_SCHEMA, AgentAdapterDescriptor,
    AgentAdapterKind, AgentEventEnvelope, AgentEventKind, OutputContractKind,
    SUBCONTRACT_PACKET_SCHEMA, SubcontractPacket, TOOL_DESCRIPTOR_SCHEMA, TOOL_POLICY_SCHEMA,
    TOOL_REGISTRY_SCHEMA, ToolAvailability, ToolDescriptor, ToolPermission, ToolPolicy,
    ToolPolicyEntry, ToolRegistry, WORKER_ROLE_SCHEMA, WorkerRole, default_tool_registry,
};
pub use capability::{
    CapabilityMaturity, RUNTIME_CAPABILITY_REPORT_SCHEMA, RUNTIME_CAPABILITY_SCHEMA,
    RuntimeCapability, RuntimeCapabilityReport, baseline_capability_report,
};
pub use conversation::{
    CONVERSATION_DIGEST_SCHEMA, CONVERSATION_MESSAGE_SCHEMA, CONVERSATION_SUMMARY_SCHEMA,
    ConversationDeleteCounts, ConversationDigest, ConversationFilter, ConversationMessage,
    ConversationRunRelation, ConversationStatus, ConversationSummary, MessageRole, MessageState,
    TitleSource,
};
pub use design::{
    DESIGN_CONTEXT_PACK_SCHEMA, DESIGN_CONTEXT_PIN_SCHEMA, DESIGN_PACKAGE_COMPONENTS_SCHEMA,
    DESIGN_PACKAGE_MANIFEST_SCHEMA, DESIGN_PACKAGE_TOKENS_SCHEMA, DesignContentHash,
    DesignContextItem, DesignContextItemKind, DesignContextPack, DesignContextPin,
    DesignPackageComponent, DesignPackageComponents, DesignPackageFiles, DesignPackageManifest,
    DesignPackageSecurity, DesignPackageSource, DesignPackageToken, DesignPackageTokens,
};
pub use diagnostic::{PROCESS_DIAGNOSTIC_SCHEMA, ProcessDiagnostic};
pub use file::{
    ConflictResolution, FileServiceError, LineEnding, OPEN_TEXT_REQUEST_SCHEMA, OpenTextRequest,
    SAVE_TEXT_REQUEST_SCHEMA, SAVE_TEXT_RESULT_SCHEMA, STAT_REQUEST_SCHEMA, SaveTextRequest,
    SaveTextResult, StatRequest, TEXT_DOCUMENT_SCHEMA, TextDocument, TextEncoding,
    WORKSPACE_ENTRY_SCHEMA, WorkspaceEntry,
};
pub use git::{
    GIT_BRANCHES_SCHEMA, GIT_DIFF_SCHEMA, GIT_LOG_SCHEMA, GIT_STATUS_SCHEMA, GitBranchInfo,
    GitBranches, GitDiff, GitDiffFile, GitDiffHunk, GitLog, GitLogEntry, GitStatus, GitStatusEntry,
    GitStatusKind,
};
pub use git_mutation::{
    GIT_COMMIT_PREVIEW_SCHEMA, GIT_COMMIT_SCHEMA, GIT_CREATE_BRANCH_SCHEMA, GIT_ERROR_SCHEMA,
    GIT_STAGE_SCHEMA, GIT_SWITCH_PREFLIGHT_SCHEMA, GIT_SWITCH_SCHEMA, GIT_UNSTAGE_SCHEMA,
    GitCommit, GitCommitPreview, GitCreateBranch, GitMutationError, GitMutationErrorBody,
    GitMutationErrorCode, GitStageRejectReason, GitStageRejection, GitStageResult, GitSwitch,
    GitSwitchBlocker, GitSwitchBlockerKind, GitSwitchPreflight, GitUnstageResult,
};
pub use git_push::{
    PUSH_APPROVAL_SCHEMA, PUSH_ERROR_SCHEMA, PUSH_FAILURE_DIAGNOSTIC_SCHEMA, PUSH_INTENT_SCHEMA,
    PUSH_RECEIPT_SCHEMA, PUSH_STATUS_SCHEMA, PushApproval, PushError, PushErrorBody, PushErrorCode,
    PushFailureDiagnostic, PushIntent, PushReceipt, PushStatus,
};
pub use intel::{
    Architecture, ArchitectureRecord, CodegraphQuery, CodegraphRecordView, FreshnessCheck,
    FreshnessCurrent, FreshnessStamp, Glossary, GlossaryTerm, INTEL_ARCHITECTURE_SCHEMA,
    INTEL_CODEGRAPH_SCHEMA, INTEL_FRESHNESS_CHECK_SCHEMA, INTEL_FRESHNESS_SCHEMA,
    INTEL_GLOSSARY_SCHEMA, StaleReason,
};
pub use patch::{
    FileOperation, FilePatch, PATCH_APPLY_RESULT_SCHEMA, PATCH_PROPOSAL_SCHEMA, PatchApplyResult,
    PatchProposal, RiskLevel, VERIFICATION_RESULT_SCHEMA, VerificationKind, VerificationResult,
};
pub use project::{PROJECT_SUMMARY_SCHEMA, ProjectSummary};
pub use projection::{
    NodeExecutionProjection, NodeProjectionState, PIPELINE_EXECUTION_PROJECTION_SCHEMA,
    PIPELINE_EXECUTION_PROJECTION_VERSION, PipelineExecutionProjection, RunMetrics,
    RunProjectionState,
};
pub use security::{
    FindingStatus, SECURITY_REPORT_SCHEMA, SecurityCheck, SecurityFinding, SecurityReport,
    Severity as SecuritySeverity, SeveritySummary,
};
pub use stream::{
    ENGINE_STREAM_FRAME_SCHEMA, EngineStreamFrame, PublicEngineError, STREAM_PROTOCOL_VERSION,
};
pub use text_diff::{DiffHunk, DiffHunkKind, TEXT_DIFF_SCHEMA, TextDiff};
pub use topology::{
    PIPELINE_TOPOLOGY_SNAPSHOT_SCHEMA, PipelineTopologyEdge, PipelineTopologyNode,
    PipelineTopologySnapshot, TopologyEdgeKind,
};
pub use turn::{
    CONVERSATION_THREAD_SETTINGS_SCHEMA, CONVERSATION_TURN_ACCEPTED_SCHEMA,
    CONVERSATION_TURN_START_REQUEST_SCHEMA, ConversationThreadSettings, ConversationTurnAccepted,
    ConversationTurnSettings, ConversationTurnStartRequest, ExecutionMode, ModelSelection,
    ModelSelectionMode, ReasoningEffort, TIMELINE_ITEM_SCHEMA, TimelineItem, TimelineItemKind,
    TurnContextSelection, UserMessageInput,
};
pub use vault::{
    VAULT_DECRYPT_SCHEMA, VAULT_ENCRYPT_SCHEMA, VAULT_ERROR_SCHEMA, VAULT_STATUS_SCHEMA,
    VAULT_SUMMARY_SCHEMA, VaultDecryptResult, VaultEncryptResult, VaultEntry, VaultErrorBody,
    VaultErrorCode, VaultErrorEnvelope, VaultErrorSchemaTag, VaultStatusResult, VaultSummary,
    VaultSummaryEntry, VaultSummaryResult,
};

pub fn schema_jsons() -> Result<Vec<(&'static str, String)>, serde_json::Error> {
    Ok(vec![
        (
            "project-summary.schema.json",
            serde_json::to_string_pretty(&schema_for!(ProjectSummary))?,
        ),
        (
            "runtime-capability.schema.json",
            serde_json::to_string_pretty(&schema_for!(RuntimeCapability))?,
        ),
        (
            "runtime-capability-report.schema.json",
            serde_json::to_string_pretty(&schema_for!(RuntimeCapabilityReport))?,
        ),
        (
            "conversation-turn-start-request.schema.json",
            serde_json::to_string_pretty(&schema_for!(ConversationTurnStartRequest))?,
        ),
        (
            "conversation-turn-accepted.schema.json",
            serde_json::to_string_pretty(&schema_for!(ConversationTurnAccepted))?,
        ),
        (
            "conversation-thread-settings.schema.json",
            serde_json::to_string_pretty(&schema_for!(ConversationThreadSettings))?,
        ),
        (
            "timeline-item.schema.json",
            serde_json::to_string_pretty(&schema_for!(TimelineItem))?,
        ),
        (
            "agent-adapter-descriptor.schema.json",
            serde_json::to_string_pretty(&schema_for!(AgentAdapterDescriptor))?,
        ),
        (
            "agent-event-envelope.schema.json",
            serde_json::to_string_pretty(&schema_for!(AgentEventEnvelope))?,
        ),
        (
            "worker-role.schema.json",
            serde_json::to_string_pretty(&schema_for!(WorkerRole))?,
        ),
        (
            "subcontract-packet.schema.json",
            serde_json::to_string_pretty(&schema_for!(SubcontractPacket))?,
        ),
        (
            "tool-policy.schema.json",
            serde_json::to_string_pretty(&schema_for!(ToolPolicy))?,
        ),
        (
            "tool-descriptor.schema.json",
            serde_json::to_string_pretty(&schema_for!(ToolDescriptor))?,
        ),
        (
            "tool-registry.schema.json",
            serde_json::to_string_pretty(&schema_for!(ToolRegistry))?,
        ),
        (
            "patch-proposal.schema.json",
            serde_json::to_string_pretty(&schema_for!(PatchProposal))?,
        ),
        (
            "patch-apply-result.schema.json",
            serde_json::to_string_pretty(&schema_for!(PatchApplyResult))?,
        ),
        (
            "verification-result.schema.json",
            serde_json::to_string_pretty(&schema_for!(VerificationResult))?,
        ),
        (
            "pipeline-topology-snapshot.schema.json",
            serde_json::to_string_pretty(&schema_for!(PipelineTopologySnapshot))?,
        ),
        (
            "process-diagnostic.schema.json",
            serde_json::to_string_pretty(&schema_for!(ProcessDiagnostic))?,
        ),
        (
            "conversation-summary.schema.json",
            serde_json::to_string_pretty(&schema_for!(ConversationSummary))?,
        ),
        (
            "conversation-message.schema.json",
            serde_json::to_string_pretty(&schema_for!(ConversationMessage))?,
        ),
        (
            "conversation-digest.schema.json",
            serde_json::to_string_pretty(&schema_for!(ConversationDigest))?,
        ),
        (
            "design-package-manifest.schema.json",
            serde_json::to_string_pretty(&schema_for!(DesignPackageManifest))?,
        ),
        (
            "design-package-tokens.schema.json",
            serde_json::to_string_pretty(&schema_for!(DesignPackageTokens))?,
        ),
        (
            "design-package-components.schema.json",
            serde_json::to_string_pretty(&schema_for!(DesignPackageComponents))?,
        ),
        (
            "design-context-pack.schema.json",
            serde_json::to_string_pretty(&schema_for!(DesignContextPack))?,
        ),
        (
            "design-context-pin.schema.json",
            serde_json::to_string_pretty(&schema_for!(DesignContextPin))?,
        ),
        (
            "open-text-request.schema.json",
            serde_json::to_string_pretty(&schema_for!(OpenTextRequest))?,
        ),
        (
            "text-document.schema.json",
            serde_json::to_string_pretty(&schema_for!(TextDocument))?,
        ),
        (
            "save-text-request.schema.json",
            serde_json::to_string_pretty(&schema_for!(SaveTextRequest))?,
        ),
        (
            "save-text-result.schema.json",
            serde_json::to_string_pretty(&schema_for!(SaveTextResult))?,
        ),
        (
            "stat-request.schema.json",
            serde_json::to_string_pretty(&schema_for!(StatRequest))?,
        ),
        (
            "workspace-entry.schema.json",
            serde_json::to_string_pretty(&schema_for!(WorkspaceEntry))?,
        ),
        (
            "file-service-error.schema.json",
            serde_json::to_string_pretty(&schema_for!(FileServiceError))?,
        ),
        (
            "pipeline-execution-projection.schema.json",
            serde_json::to_string_pretty(&schema_for!(PipelineExecutionProjection))?,
        ),
        (
            "engine-stream-frame.schema.json",
            serde_json::to_string_pretty(&schema_for!(EngineStreamFrame))?,
        ),
        (
            "engine-request.schema.json",
            serde_json::to_string_pretty(&schema_for!(EngineRequest))?,
        ),
        (
            "engine-event.schema.json",
            serde_json::to_string_pretty(&schema_for!(EngineEvent))?,
        ),
        (
            "terminal-session.schema.json",
            serde_json::to_string_pretty(&schema_for!(TerminalSession))?,
        ),
        (
            "terminal-event.schema.json",
            serde_json::to_string_pretty(&schema_for!(TerminalEvent))?,
        ),
        (
            "terminal-command-block.schema.json",
            serde_json::to_string_pretty(&schema_for!(TerminalCommandBlock))?,
        ),
        (
            "terminal-suggestion-request.schema.json",
            serde_json::to_string_pretty(&schema_for!(TerminalSuggestionRequest))?,
        ),
        (
            "terminal-suggestion.schema.json",
            serde_json::to_string_pretty(&schema_for!(TerminalSuggestion))?,
        ),
        (
            "terminal-agent-turn.schema.json",
            serde_json::to_string_pretty(&schema_for!(TerminalAgentTurnStartRequest))?,
        ),
        (
            "terminal-risk-decision.schema.json",
            serde_json::to_string_pretty(&schema_for!(TerminalRiskDecision))?,
        ),
        (
            "terminal-mcp-tool-descriptor.schema.json",
            serde_json::to_string_pretty(&schema_for!(TerminalMcpToolDescriptor))?,
        ),
        (
            "execution-event.schema.json",
            serde_json::to_string_pretty(&schema_for!(ExecutionEvent))?,
        ),
        (
            "execution-event-envelope.schema.json",
            serde_json::to_string_pretty(&schema_for!(ExecutionEventEnvelope))?,
        ),
        (
            "work-item.schema.json",
            serde_json::to_string_pretty(&schema_for!(WorkItem))?,
        ),
        (
            "provider-connection.schema.json",
            serde_json::to_string_pretty(&schema_for!(ProviderConnection))?,
        ),
        (
            "secret-ref.schema.json",
            serde_json::to_string_pretty(&schema_for!(SecretRef))?,
        ),
        (
            "model-catalog-entry.schema.json",
            serde_json::to_string_pretty(&schema_for!(ModelCatalogEntry))?,
        ),
        (
            "provider-probe-receipt.schema.json",
            serde_json::to_string_pretty(&schema_for!(ProviderProbeReceipt))?,
        ),
        (
            "provider-adapter-check.schema.json",
            serde_json::to_string_pretty(&schema_for!(ProviderAdapterCheckReport))?,
        ),
        (
            "provider-mock-e2e.schema.json",
            serde_json::to_string_pretty(&schema_for!(ProviderMockE2eReport))?,
        ),
        (
            "provider-mutation.schema.json",
            serde_json::to_string_pretty(&schema_for!(ProviderMutationReceipt))?,
        ),
        (
            "model-profile.schema.json",
            serde_json::to_string_pretty(&schema_for!(ModelProfile))?,
        ),
        (
            "provider-descriptor.schema.json",
            serde_json::to_string_pretty(&schema_for!(ProviderDescriptor))?,
        ),
        (
            "routing-decision.schema.json",
            serde_json::to_string_pretty(&schema_for!(RoutingDecision))?,
        ),
        (
            "scheduler-work-item.schema.json",
            serde_json::to_string_pretty(&schema_for!(SchedulerWorkItem))?,
        ),
        (
            "concurrency-decision.schema.json",
            serde_json::to_string_pretty(&schema_for!(ConcurrencyDecision))?,
        ),
        (
            "pipeline-graph.schema.json",
            serde_json::to_string_pretty(&schema_for!(PipelineGraph))?,
        ),
        (
            "objective-plan-request.schema.json",
            serde_json::to_string_pretty(&schema_for!(ObjectivePlanRequest))?,
        ),
        (
            "objective-plan-receipt.schema.json",
            serde_json::to_string_pretty(&schema_for!(ObjectivePlanReceipt))?,
        ),
        (
            "planner-shard-policy.schema.json",
            serde_json::to_string_pretty(&schema_for!(PlannerShardPolicy))?,
        ),
        (
            "compiled-plan.schema.json",
            serde_json::to_string_pretty(&schema_for!(CompiledPlan))?,
        ),
        (
            "git-isolation.schema.json",
            serde_json::to_string_pretty(&schema_for!(GitIsolationReport))?,
        ),
        (
            "worktree-isolation-inventory-receipt.schema.json",
            serde_json::to_string_pretty(&schema_for!(WorktreeIsolationInventoryReceipt))?,
        ),
        (
            "worktree-isolation-recovery-receipt.schema.json",
            serde_json::to_string_pretty(&schema_for!(WorktreeIsolationRecoveryReceipt))?,
        ),
        (
            "patch-envelope.schema.json",
            serde_json::to_string_pretty(&schema_for!(PatchEnvelope))?,
        ),
        (
            "role-subcontract-candidate-receipt.schema.json",
            serde_json::to_string_pretty(&schema_for!(RoleSubcontractCandidateReceipt))?,
        ),
        (
            "semantic-verifier-judgment.schema.json",
            serde_json::to_string_pretty(&schema_for!(SemanticVerifierJudgmentReceipt))?,
        ),
        (
            "integration-candidate-receipt.schema.json",
            serde_json::to_string_pretty(&schema_for!(IntegrationCandidateReceipt))?,
        ),
        (
            "integration-candidate-selection-receipt.schema.json",
            serde_json::to_string_pretty(&schema_for!(IntegrationCandidateSelectionReceipt))?,
        ),
        (
            "integration-verification-receipt.schema.json",
            serde_json::to_string_pretty(&schema_for!(IntegrationVerificationReceipt))?,
        ),
        (
            "integration-apply-receipt.schema.json",
            serde_json::to_string_pretty(&schema_for!(IntegrationApplyReceipt))?,
        ),
        (
            "integration-repair-item.schema.json",
            serde_json::to_string_pretty(&schema_for!(IntegrationRepairItem))?,
        ),
        (
            "integration-final-seal.schema.json",
            serde_json::to_string_pretty(&schema_for!(IntegrationFinalSeal))?,
        ),
        (
            "integration-cleanup-receipt.schema.json",
            serde_json::to_string_pretty(&schema_for!(IntegrationCleanupReceipt))?,
        ),
        (
            "completion-proof.schema.json",
            serde_json::to_string_pretty(&schema_for!(CompletionProof))?,
        ),
        (
            "hook-spec.schema.json",
            serde_json::to_string_pretty(&schema_for!(HookSpec))?,
        ),
        (
            "hook-decision.schema.json",
            serde_json::to_string_pretty(&schema_for!(HookDecision))?,
        ),
        (
            "codegraph-record.schema.json",
            serde_json::to_string_pretty(&schema_for!(CodeGraphRecord))?,
        ),
        (
            "codegraph-index.schema.json",
            serde_json::to_string_pretty(&schema_for!(CodeGraphIndex))?,
        ),
        (
            "triwiki-record.schema.json",
            serde_json::to_string_pretty(&schema_for!(TriWikiRecord))?,
        ),
        (
            "context-pack.schema.json",
            serde_json::to_string_pretty(&schema_for!(ContextPack))?,
        ),
        (
            "image-asset.schema.json",
            serde_json::to_string_pretty(&schema_for!(ImageAsset))?,
        ),
        (
            "image-ledger.schema.json",
            serde_json::to_string_pretty(&schema_for!(ImageLedger))?,
        ),
        (
            "image-provenance-receipt.schema.json",
            serde_json::to_string_pretty(&schema_for!(ImageProvenanceReceipt))?,
        ),
        (
            "reasoning-report.schema.json",
            serde_json::to_string_pretty(&schema_for!(ReasoningReport))?,
        ),
        (
            "outbox-item.schema.json",
            serde_json::to_string_pretty(&schema_for!(OutboxItem))?,
        ),
        (
            "outbox-dispatch-report.schema.json",
            serde_json::to_string_pretty(&schema_for!(OutboxDispatchReport))?,
        ),
        (
            "data-plane-manifest.schema.json",
            serde_json::to_string_pretty(&schema_for!(DataPlaneManifest))?,
        ),
        (
            "retention-plan.schema.json",
            serde_json::to_string_pretty(&schema_for!(RetentionPlan))?,
        ),
        (
            "release-proof.schema.json",
            serde_json::to_string_pretty(&schema_for!(ReleaseProof))?,
        ),
        (
            "perf-stress-report.schema.json",
            serde_json::to_string_pretty(&schema_for!(PerfStressReport))?,
        ),
        (
            "text-diff.schema.json",
            serde_json::to_string_pretty(&schema_for!(TextDiff))?,
        ),
        (
            "git-status.schema.json",
            serde_json::to_string_pretty(&schema_for!(GitStatus))?,
        ),
        (
            "git-branches.schema.json",
            serde_json::to_string_pretty(&schema_for!(GitBranches))?,
        ),
        (
            "git-diff.schema.json",
            serde_json::to_string_pretty(&schema_for!(GitDiff))?,
        ),
        (
            "git-log.schema.json",
            serde_json::to_string_pretty(&schema_for!(GitLog))?,
        ),
        (
            "git-switch-preflight.schema.json",
            serde_json::to_string_pretty(&schema_for!(GitSwitchPreflight))?,
        ),
        (
            "git-create-branch.schema.json",
            serde_json::to_string_pretty(&schema_for!(GitCreateBranch))?,
        ),
        (
            "git-switch.schema.json",
            serde_json::to_string_pretty(&schema_for!(GitSwitch))?,
        ),
        (
            "git-stage.schema.json",
            serde_json::to_string_pretty(&schema_for!(GitStageResult))?,
        ),
        (
            "git-unstage.schema.json",
            serde_json::to_string_pretty(&schema_for!(GitUnstageResult))?,
        ),
        (
            "git-commit-preview.schema.json",
            serde_json::to_string_pretty(&schema_for!(GitCommitPreview))?,
        ),
        (
            "git-commit.schema.json",
            serde_json::to_string_pretty(&schema_for!(GitCommit))?,
        ),
        (
            "git-mutation-error.schema.json",
            serde_json::to_string_pretty(&schema_for!(GitMutationError))?,
        ),
        (
            "push-intent.schema.json",
            serde_json::to_string_pretty(&schema_for!(PushIntent))?,
        ),
        (
            "push-approval.schema.json",
            serde_json::to_string_pretty(&schema_for!(PushApproval))?,
        ),
        (
            "push-receipt.schema.json",
            serde_json::to_string_pretty(&schema_for!(PushReceipt))?,
        ),
        (
            "push-failure-diagnostic.schema.json",
            serde_json::to_string_pretty(&schema_for!(PushFailureDiagnostic))?,
        ),
        (
            "push-status.schema.json",
            serde_json::to_string_pretty(&schema_for!(PushStatus))?,
        ),
        (
            "push-error.schema.json",
            serde_json::to_string_pretty(&schema_for!(PushError))?,
        ),
        (
            "intel-freshness.schema.json",
            serde_json::to_string_pretty(&schema_for!(FreshnessStamp))?,
        ),
        (
            "intel-freshness-check.schema.json",
            serde_json::to_string_pretty(&schema_for!(FreshnessCheck))?,
        ),
        (
            "intel-codegraph.schema.json",
            serde_json::to_string_pretty(&schema_for!(CodegraphQuery))?,
        ),
        (
            "intel-glossary.schema.json",
            serde_json::to_string_pretty(&schema_for!(Glossary))?,
        ),
        (
            "intel-architecture.schema.json",
            serde_json::to_string_pretty(&schema_for!(Architecture))?,
        ),
        (
            "vault-summary.schema.json",
            serde_json::to_string_pretty(&schema_for!(VaultSummary))?,
        ),
        (
            "vault-summary-result.schema.json",
            serde_json::to_string_pretty(&schema_for!(VaultSummaryResult))?,
        ),
        (
            "vault-encrypt-result.schema.json",
            serde_json::to_string_pretty(&schema_for!(VaultEncryptResult))?,
        ),
        (
            "vault-decrypt-result.schema.json",
            serde_json::to_string_pretty(&schema_for!(VaultDecryptResult))?,
        ),
        (
            "vault-status.schema.json",
            serde_json::to_string_pretty(&schema_for!(VaultStatusResult))?,
        ),
        (
            "vault-error.schema.json",
            serde_json::to_string_pretty(&schema_for!(VaultErrorEnvelope))?,
        ),
        (
            "security-report.schema.json",
            serde_json::to_string_pretty(&schema_for!(SecurityReport))?,
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_request_roundtrips_with_typed_kind() {
        let request = EngineRequest::health("req-1");
        let json = serde_json::to_string(&request).expect("serialize request");
        assert!(json.contains("\"kind\":\"health\""));
        let decoded: EngineRequest = serde_json::from_str(&json).expect("decode request");
        assert_eq!(decoded.kind, EngineRequestKind::Health);
    }

    #[test]
    fn run_start_request_roundtrips_with_typed_params() {
        let request =
            EngineRequest::run_start("req-run", "single-model-safe", "Prove daemon run.start");
        let mut graph_request =
            EngineRequest::run_start("req-graph", "editor-draft", "Prove graph path run.start");
        graph_request.params.graph_path =
            Some(".opensks/pipelines/editor/current.graph.json".into());
        let json = serde_json::to_string(&request).expect("serialize request");
        let graph_json = serde_json::to_string(&graph_request).expect("serialize graph request");
        assert!(json.contains("\"kind\":\"run_start\""));
        assert!(json.contains("\"pipeline_id\":\"single-model-safe\""));
        assert!(
            graph_json.contains("\"graph_path\":\".opensks/pipelines/editor/current.graph.json\"")
        );
        let decoded: EngineRequest = serde_json::from_str(&json).expect("decode request");
        let decoded_graph: EngineRequest =
            serde_json::from_str(&graph_json).expect("decode graph request");
        assert_eq!(decoded.kind, EngineRequestKind::RunStart);
        assert_eq!(
            decoded.params.objective.as_deref(),
            Some("Prove daemon run.start")
        );
        assert_eq!(
            decoded_graph.params.graph_path.as_deref(),
            Some(".opensks/pipelines/editor/current.graph.json")
        );
    }

    #[test]
    fn provider_adapter_check_report_decodes_remediation_actions() {
        let json = r#"{
            "schema":"opensks.provider-adapter-check.v1",
            "generated_at":{"unix_seconds":1782399232,"nanos":95853000},
            "remote_probe_opt_in":false,
            "secret_value_exposed":false,
            "summary":{"total":2,"attempted":0,"reachable":0},
            "blockers":["configure_OPENROUTER_API_KEY_credential"],
            "remediation_actions":[
                {
                    "blocker":"configure_OPENROUTER_API_KEY_credential",
                    "action":"Add an OpenRouter API key credential through Provider Center or the configured secret store.",
                    "scope":"provider_credential"
                }
            ],
            "adapters":[
                {
                    "name":"OpenRouter",
                    "configured":false,
                    "attempted":false,
                    "status":"not_configured",
                    "credential_source":"none",
                    "endpoint":"https://openrouter.ai/api/v1/models",
                    "http_code":null,
                    "duration_ms":0,
                    "transport":"native_reqwest_blocking_http",
                    "secret_value_exposed":false
                }
            ]
        }"#;

        let report: ProviderAdapterCheckReport =
            serde_json::from_str(json).expect("decode provider adapter report");

        assert_eq!(report.schema, PROVIDER_ADAPTER_CHECK_SCHEMA);
        assert_eq!(report.summary.total, 2);
        assert_eq!(
            report.blockers,
            vec!["configure_OPENROUTER_API_KEY_credential"]
        );
        assert_eq!(report.remediation_actions.len(), 1);
        assert_eq!(report.remediation_actions[0].scope, "provider_credential");
        assert_eq!(report.adapters[0].blockers, Vec::<String>::new());
        assert!(!report.secret_value_exposed);
    }

    #[test]
    fn provider_mock_e2e_report_roundtrips_fixture_truth() {
        let report = ProviderMockE2eReport {
            schema: PROVIDER_MOCK_E2E_SCHEMA.to_string(),
            generated_at: ContractTimestamp {
                unix_seconds: 1782400000,
                nanos: 0,
            },
            status: TrustStatus::Verified,
            fixture_kind: "openai_compatible_registry_fixture".to_string(),
            live_vendor_calls_performed: false,
            secret_value_exposed: false,
            provider_id: "mock-openai-compatible".to_string(),
            model_id: "mock-openai-compatible/code-model".to_string(),
            model_catalog_count: 1,
            model_catalog_synced: true,
            model_enabled: true,
            registry_route_status: RoutingStatus::Resolved,
            selected_model_id: Some("mock-openai-compatible/code-model".to_string()),
            route_receipt: None,
            checks: vec![ProviderMockE2eCheck {
                id: "registry_route_resolved".to_string(),
                status: TrustStatus::Verified,
                evidence_ref: "provider mock-e2e".to_string(),
            }],
        };

        let json = serde_json::to_string(&report).expect("serialize mock provider e2e");
        assert!(json.contains("\"live_vendor_calls_performed\":false"));
        let decoded: ProviderMockE2eReport =
            serde_json::from_str(&json).expect("decode mock provider e2e");

        assert_eq!(decoded.schema, PROVIDER_MOCK_E2E_SCHEMA);
        assert_eq!(decoded.status, TrustStatus::Verified);
        assert_eq!(decoded.registry_route_status, RoutingStatus::Resolved);
        assert!(!decoded.live_vendor_calls_performed);
        assert!(!decoded.secret_value_exposed);
    }

    #[test]
    fn objective_plan_request_and_receipt_roundtrip() {
        let mut request = ObjectivePlanRequest::new("Build provider UI and verify final diff");
        request.include_image_lane = true;
        request.evidence_refs = vec!["test:objective-planner".to_string()];
        let json = serde_json::to_string(&request).expect("serialize objective plan request");
        assert!(json.contains("\"schema\":\"opensks.objective-plan-request.v1\""));
        assert!(json.contains("\"include_image_lane\":true"));
        let decoded: ObjectivePlanRequest =
            serde_json::from_str(&json).expect("decode objective plan request");
        assert_eq!(decoded.max_parallelism, 8);
        assert_eq!(decoded.role_count, 4);
        assert!(decoded.require_git_worktree);
        assert!(decoded.require_integration_approval);

        let receipt = ObjectivePlanReceipt {
            schema: OBJECTIVE_PLAN_RECEIPT_SCHEMA.to_string(),
            objective_hash: "fnv1a64:1111111111111111".to_string(),
            graph_id: "objective-plan-11111111".to_string(),
            graph_hash: "fnv1a64:2222222222222222".to_string(),
            plan_hash: "fnv1a64:3333333333333333".to_string(),
            source: "objective_planner".to_string(),
            max_parallelism: 8,
            role_count: 4,
            work_template_count: 8,
            repair_action_count: 0,
            shard_policy_id: Some("planner-shard-policy-22222222".to_string()),
            shard_policy: Some(PlannerShardPolicy {
                schema: PLANNER_SHARD_POLICY_SCHEMA.to_string(),
                id: "planner-shard-policy-22222222".to_string(),
                source: "objective_planner".to_string(),
                role_count: 4,
                max_parallelism: 8,
                implementation_shard_count: 4,
                verifier_shard_count: 4,
                candidate_selection_policy: "planner_required_shards_before_approval_apply"
                    .to_string(),
                required_gates: vec![
                    "candidate_receipt_valid".to_string(),
                    "target_policy_check".to_string(),
                    "patch_apply_check".to_string(),
                    "read_only_verifier_lanes".to_string(),
                    "approval_event".to_string(),
                    "final_seal".to_string(),
                ],
                evidence_refs: vec!["planner:shard-policy".to_string()],
            }),
            graph_ref: Some(".opensks/pipelines/objective/objective.graph.json".to_string()),
            compiled_plan_ref: Some(".opensks/pipelines/compiled/objective.plan.json".to_string()),
            planner_provider_id: None,
            planner_model_id: None,
            planner_response_hash: None,
            planner_response_bytes: None,
            evidence_refs: vec!["graph:objective-planner".to_string()],
        };
        let json = serde_json::to_string(&receipt).expect("serialize objective plan receipt");
        let decoded: ObjectivePlanReceipt =
            serde_json::from_str(&json).expect("decode objective plan receipt");
        assert_eq!(decoded.source, "objective_planner");
        assert_eq!(decoded.repair_action_count, 0);
        let shard_policy = decoded.shard_policy.expect("shard policy");
        assert_eq!(
            decoded.shard_policy_id.as_deref(),
            Some(shard_policy.id.as_str())
        );
        assert_eq!(shard_policy.schema, PLANNER_SHARD_POLICY_SCHEMA);
        assert_eq!(shard_policy.implementation_shard_count, 4);
        assert!(
            shard_policy
                .required_gates
                .contains(&"approval_event".to_string())
        );
    }

    #[test]
    fn run_control_requests_roundtrip_with_typed_params() {
        let cancel = EngineRequest::run_cancel("req-cancel", "run-1");
        let json = serde_json::to_string(&cancel).expect("cancel json");
        assert!(json.contains("\"kind\":\"run_cancel\""));
        let decoded: EngineRequest = serde_json::from_str(&json).expect("decode cancel");
        assert_eq!(decoded.kind, EngineRequestKind::RunCancel);
        assert_eq!(decoded.params.run_id.as_deref(), Some("run-1"));
        assert_eq!(
            decoded.params.reason_code.as_deref(),
            Some("cancelled_by_user")
        );

        let steer = EngineRequest::run_steer("req-steer", "run-1", "work-1", "Focus tests");
        let steer_json = serde_json::to_string(&steer).expect("steer json");
        assert!(steer_json.contains("\"kind\":\"run_steer\""));
        let decoded: EngineRequest = serde_json::from_str(&steer_json).expect("decode steer");
        assert_eq!(decoded.kind, EngineRequestKind::RunSteer);
        assert_eq!(decoded.params.target_id.as_deref(), Some("work-1"));
        assert_eq!(decoded.params.message.as_deref(), Some("Focus tests"));
        assert_eq!(EventKind::RunCancelled.as_str(), "run_cancelled");
        assert_eq!(
            EventKind::parse_label("steering_requested"),
            EventKind::SteeringRequested
        );
        assert_eq!(EventKind::RunCompleted.as_str(), "run_completed");
        assert_eq!(
            EventKind::parse_label("run_completed"),
            EventKind::RunCompleted
        );
        assert_eq!(EventKind::GitCommitReceipt.as_str(), "git_commit_receipt");
        assert_eq!(
            EventKind::parse_label("git_push_receipt"),
            EventKind::GitPushReceipt
        );
        assert_eq!(
            EventKind::parse_label("git_push_failed"),
            EventKind::GitPushFailed
        );
        assert_eq!(
            EventKind::ImageArtifactCreated.as_str(),
            "image_artifact_created"
        );
        assert_eq!(
            EventKind::parse_label("image_artifact_created"),
            EventKind::ImageArtifactCreated
        );
    }

    #[test]
    fn conversation_turn_start_request_roundtrips_as_engine_request() {
        let turn_request = ConversationTurnStartRequest {
            schema: CONVERSATION_TURN_START_REQUEST_SCHEMA.to_string(),
            request_id: "req-conversation-turn".to_string(),
            project_id: "proj-1".to_string(),
            conversation_id: "conv-1".to_string(),
            client_turn_id: "client-turn-1".to_string(),
            message: UserMessageInput {
                text: "make the daemon accept this turn".to_string(),
                attachment_refs: vec![],
            },
            thread_settings_updated_at_ms: Some(0),
            settings: Some(ConversationTurnSettings {
                model: ModelSelection {
                    mode: ModelSelectionMode::Auto,
                    model_id: None,
                    fallback_model_ids: vec![],
                },
                reasoning_effort: ReasoningEffort::Standard,
                execution_mode: ExecutionMode::Worktree,
                pipeline_id: "auto".to_string(),
                graph_revision: None,
                max_parallelism: 4,
                verifier_count: 1,
                tool_policy_id: "project-default".to_string(),
                approval_policy_id: "safe-interactive".to_string(),
                token_budget: None,
                cost_budget_usd: None,
                timeout_ms: None,
                image_model_id: None,
            }),
            context: TurnContextSelection::default(),
            idempotency_key: "idem-conversation-turn".to_string(),
        };
        let request = EngineRequest::conversation_turn_start(turn_request.clone());
        let json = serde_json::to_string(&request).expect("conversation turn request json");
        assert!(json.contains("\"kind\":\"conversation_turn_start\""));
        assert!(json.contains("\"conversation_turn_start\""));
        let decoded: EngineRequest =
            serde_json::from_str(&json).expect("decode conversation turn request");
        assert_eq!(decoded.kind, EngineRequestKind::ConversationTurnStart);
        assert_eq!(
            decoded.params.conversation_turn_start.as_ref(),
            Some(&turn_request)
        );
    }

    #[test]
    fn conversation_supervisor_tick_request_roundtrips() {
        let request =
            EngineRequest::conversation_supervisor_tick("req-supervisor", "daemon-supervisor", 750);
        let json = serde_json::to_string(&request).expect("supervisor tick request json");
        assert!(json.contains("\"kind\":\"conversation_supervisor_tick\""));
        assert!(json.contains("\"supervisor_id\":\"daemon-supervisor\""));
        let decoded: EngineRequest =
            serde_json::from_str(&json).expect("decode supervisor tick request");
        assert_eq!(decoded.kind, EngineRequestKind::ConversationSupervisorTick);
        assert_eq!(
            decoded.params.supervisor_id.as_deref(),
            Some("daemon-supervisor")
        );
        assert_eq!(decoded.params.lease_ttl_ms, Some(750));
    }

    #[test]
    fn conversation_supervisor_tick_for_run_request_roundtrips() {
        let request = EngineRequest::conversation_supervisor_tick_for_run(
            "req-supervisor-run",
            "daemon-supervisor",
            750,
            "run-foreground",
        );
        let json = serde_json::to_string(&request).expect("supervisor tick request json");
        assert!(json.contains("\"run_id\":\"run-foreground\""));
        let decoded: EngineRequest =
            serde_json::from_str(&json).expect("decode supervisor tick request");
        assert_eq!(decoded.kind, EngineRequestKind::ConversationSupervisorTick);
        assert_eq!(decoded.params.run_id.as_deref(), Some("run-foreground"));
        assert_eq!(decoded.params.lease_ttl_ms, Some(750));
    }

    #[test]
    fn integration_candidate_apply_request_roundtrips() {
        let request = EngineRequest::integration_candidate_apply(
            "req-apply",
            "run-1",
            "approval-integration-run-1",
        );
        let json = serde_json::to_string(&request).expect("integration apply request json");
        assert!(json.contains("\"kind\":\"integration_candidate_apply\""));
        assert!(json.contains("\"run_id\":\"run-1\""));
        assert!(json.contains("\"scope\":\"integration_apply\""));
        let decoded: EngineRequest =
            serde_json::from_str(&json).expect("decode integration apply request");
        assert_eq!(decoded.kind, EngineRequestKind::IntegrationCandidateApply);
        assert_eq!(decoded.params.run_id.as_deref(), Some("run-1"));
        assert_eq!(
            decoded.params.approval_id.as_deref(),
            Some("approval-integration-run-1")
        );
    }

    #[test]
    fn semantic_verifier_judgment_receipt_roundtrips() {
        let receipt = SemanticVerifierJudgmentReceipt {
            schema: SEMANTIC_VERIFIER_JUDGMENT_SCHEMA.to_string(),
            id: "semantic-verifier-run-1-turn-role-verification".to_string(),
            run_id: "run-1".to_string(),
            work_item_id: "turn-role-turn-1-2-verification".to_string(),
            role: "verification".to_string(),
            worker_id: "role-worker-verification".to_string(),
            state: "judgment_ready".to_string(),
            reason_code: "model_semantic_verifier_judgment_recorded".to_string(),
            verifier_kind: "model_semantic_judgment".to_string(),
            verdict: "pass".to_string(),
            passed_gates: vec!["semantic_verifier_verdict_passed".to_string()],
            failed_gates: Vec::new(),
            provider_id: Some("provider-1".to_string()),
            model_id: Some("provider-1/code-model".to_string()),
            response_hash: "fnv64:1111111111111111".to_string(),
            response_bytes: 42,
            judgment_ref:
                "artifact://.opensks/runtime/semantic-verifiers/run-1/turn-role-verification/judgment.json"
                    .to_string(),
            context_pack_ref: Some(
                "artifact://.opensks/wiki/context-packs/generated/turn-context-turn-1.json"
                    .to_string(),
            ),
            worker_context_pack_ref: Some(
                "artifact://.opensks/wiki/context-packs/generated/turn-context-turn-1--worker-turn-role-verification.json#work_item_id=turn-role-turn-1-2-verification"
                    .to_string(),
            ),
            path_redacted: true,
            content_redacted: true,
            generated_at_ms: 1_000,
            evidence_refs: vec!["daemon:semantic-verifier-judgment".to_string()],
        };
        let json = serde_json::to_string(&receipt).expect("semantic verifier receipt json");
        assert!(json.contains("\"schema\":\"opensks.semantic-verifier-judgment.v1\""));
        assert!(json.contains("\"content_redacted\":true"));
        let decoded: SemanticVerifierJudgmentReceipt =
            serde_json::from_str(&json).expect("decode semantic verifier receipt");
        assert_eq!(decoded.schema, SEMANTIC_VERIFIER_JUDGMENT_SCHEMA);
        assert_eq!(decoded.role, "verification");
        assert_eq!(decoded.verifier_kind, "model_semantic_judgment");
        assert_eq!(decoded.verdict, "pass");
        assert_eq!(
            decoded.passed_gates,
            vec!["semantic_verifier_verdict_passed".to_string()]
        );
        assert!(decoded.failed_gates.is_empty());
        assert_eq!(decoded.provider_id.as_deref(), Some("provider-1"));
        assert_eq!(decoded.model_id.as_deref(), Some("provider-1/code-model"));
        assert_eq!(decoded.response_hash, "fnv64:1111111111111111");
        assert!(decoded.path_redacted);
        assert!(decoded.content_redacted);
    }

    #[test]
    fn integration_candidate_receipt_roundtrips_with_source_candidates() {
        let turn_settings = integration_turn_settings_fixture();
        let receipt = IntegrationCandidateReceipt {
            schema: INTEGRATION_CANDIDATE_RECEIPT_SCHEMA.to_string(),
            id: "integration-candidate-run-1".to_string(),
            run_id: "run-1".to_string(),
            turn_id: "turn-1".to_string(),
            conversation_id: "conversation-1".to_string(),
            project_id: "project-1".to_string(),
            worker_id: "integration-coordinator".to_string(),
            state: "candidate_ready".to_string(),
            reason_code: "aggregate_isolated_patch_candidate_ready".to_string(),
            source_isolation_id: Some("isolation-run-1-turn-supervisor".to_string()),
            source_isolation_mode: Some("git_worktree".to_string()),
            source_base_commit: Some("abc123".to_string()),
            source_git_available: true,
            source_candidates: vec![IntegrationSourceCandidateRef {
                source: "role_subcontract".to_string(),
                id: "role-candidate-run-1-code".to_string(),
                receipt_ref:
                    "artifact://.opensks/runtime/role-candidates/run-1/code/candidate.json"
                        .to_string(),
                patch_ref: "artifact://.opensks/runtime/role-candidates/run-1/code/candidate.patch"
                    .to_string(),
                worker_id: "role-subcontract-code".to_string(),
                work_item_id: Some("turn-role-code".to_string()),
                role: Some("code".to_string()),
                source_isolation_id: Some("isolation-run-1-role-subcontract-code".to_string()),
                source_isolation_mode: Some("git_worktree".to_string()),
                target_paths: vec!["ROLE_CODE_NOTE.md".to_string()],
                shard_policy_id: Some("planner-shard-policy-11111111".to_string()),
                shard_policy_selection_policy: Some(
                    "planner_required_shards_before_approval_apply".to_string(),
                ),
                planner_required_source_candidate_count: 2,
                planner_required_verifier_count: 3,
            }],
            aggregate_candidate_count: 1,
            aggregate_target_count: 1,
            planned_verifier_count: 2,
            shard_policy_id: Some("planner-shard-policy-11111111".to_string()),
            shard_policy_selection_policy: Some(
                "planner_required_shards_before_approval_apply".to_string(),
            ),
            planner_required_source_candidate_count: 2,
            planner_selected_source_candidate_count: 1,
            planner_required_verifier_count: 3,
            target_paths: vec!["ROLE_CODE_NOTE.md".to_string()],
            patch_count: 1,
            apply_result_count: 1,
            applied_files: vec!["ROLE_CODE_NOTE.md".to_string()],
            receipt_ref: "artifact://.opensks/runtime/integration-candidates/run-1/candidate.json"
                .to_string(),
            patch_ref: "artifact://.opensks/runtime/integration-candidates/run-1/candidate.patch"
                .to_string(),
            selection_ref: Some(
                "artifact://.opensks/runtime/integration-candidates/run-1/selection.json"
                    .to_string(),
            ),
            main_workspace_modified: false,
            integration_required: true,
            approval_required: true,
            approval_policy_id: Some("safe-interactive".to_string()),
            turn_settings: Some(turn_settings.clone()),
            path_redacted: true,
            content_redacted: true,
            generated_at_ms: 1_000,
            evidence_refs: vec!["integration:aggregate-candidate-ready".to_string()],
        };
        let json = serde_json::to_string(&receipt).expect("candidate receipt json");
        assert!(json.contains("\"schema\":\"opensks.integration-candidate.v1\""));
        assert!(json.contains("\"source_candidates\""));
        let decoded: IntegrationCandidateReceipt =
            serde_json::from_str(&json).expect("decode candidate receipt");
        assert_eq!(decoded.schema, INTEGRATION_CANDIDATE_RECEIPT_SCHEMA);
        assert_eq!(decoded.source_candidates.len(), 1);
        assert_eq!(
            decoded.source_candidates[0].source_isolation_id.as_deref(),
            Some("isolation-run-1-role-subcontract-code")
        );
        assert_eq!(decoded.aggregate_candidate_count, 1);
        assert_eq!(decoded.planned_verifier_count, 2);
        assert_eq!(
            decoded.shard_policy_id.as_deref(),
            Some("planner-shard-policy-11111111")
        );
        assert_eq!(decoded.planner_required_source_candidate_count, 2);
        assert_eq!(decoded.planner_selected_source_candidate_count, 1);
        assert_eq!(decoded.planner_required_verifier_count, 3);
        assert_eq!(
            decoded.source_candidates[0].shard_policy_id.as_deref(),
            Some("planner-shard-policy-11111111")
        );
        assert_eq!(decoded.target_paths, vec!["ROLE_CODE_NOTE.md".to_string()]);
        assert_eq!(
            decoded.selection_ref.as_deref(),
            Some("artifact://.opensks/runtime/integration-candidates/run-1/selection.json")
        );
        assert_eq!(
            decoded.approval_policy_id.as_deref(),
            Some("safe-interactive")
        );
        let decoded_settings = decoded.turn_settings.expect("turn settings receipt");
        assert_eq!(decoded_settings.pipeline_id, "integration-test-pipeline");
        assert_eq!(decoded_settings.max_parallelism, 6);
        assert_eq!(decoded_settings.verifier_count, 3);
        assert_eq!(decoded_settings.tool_policy_id, "integration-tools");
    }

    #[test]
    fn integration_candidate_selection_receipt_roundtrips() {
        let turn_settings = integration_turn_settings_fixture();
        let receipt = IntegrationCandidateSelectionReceipt {
            schema: INTEGRATION_CANDIDATE_SELECTION_RECEIPT_SCHEMA.to_string(),
            id: "integration-candidate-selection-run-1".to_string(),
            run_id: "run-1".to_string(),
            selected_candidate_id: "integration-candidate-run-1".to_string(),
            selection_ref:
                "artifact://.opensks/runtime/integration-candidates/run-1/selection.json"
                    .to_string(),
            selected_candidate_ref:
                "artifact://.opensks/runtime/integration-candidates/run-1/candidate.json"
                    .to_string(),
            selected_patch_ref:
                "artifact://.opensks/runtime/integration-candidates/run-1/candidate.patch"
                    .to_string(),
            candidate_pool: vec![IntegrationSourceCandidateRef {
                source: "role_subcontract".to_string(),
                id: "role-candidate-run-1-code".to_string(),
                receipt_ref:
                    "artifact://.opensks/runtime/role-candidates/run-1/code/candidate.json"
                        .to_string(),
                patch_ref: "artifact://.opensks/runtime/role-candidates/run-1/code/candidate.patch"
                    .to_string(),
                worker_id: "role-subcontract-code".to_string(),
                work_item_id: Some("turn-role-code".to_string()),
                role: Some("code".to_string()),
                source_isolation_id: Some("isolation-run-1-role-subcontract-code".to_string()),
                source_isolation_mode: Some("git_worktree".to_string()),
                target_paths: vec!["ROLE_CODE_NOTE.md".to_string()],
                shard_policy_id: Some("planner-shard-policy-11111111".to_string()),
                shard_policy_selection_policy: Some(
                    "planner_required_shards_before_approval_apply".to_string(),
                ),
                planner_required_source_candidate_count: 2,
                planner_required_verifier_count: 3,
            }],
            selected_source_candidate_ids: vec!["role-candidate-run-1-code".to_string()],
            selection_policy: "planner_required_shards_before_approval_apply".to_string(),
            reason_code: "planner_required_shards_selected".to_string(),
            required_verification_gates: vec![
                "candidate_receipt_valid".to_string(),
                "target_policy_check".to_string(),
                "patch_apply_check".to_string(),
                "approval_event".to_string(),
            ],
            aggregate_candidate_count: 1,
            aggregate_target_count: 1,
            planned_verifier_count: 2,
            approval_policy_id: Some("safe-interactive".to_string()),
            turn_settings: Some(turn_settings),
            shard_policy_id: Some("planner-shard-policy-11111111".to_string()),
            shard_policy_selection_policy: Some(
                "planner_required_shards_before_approval_apply".to_string(),
            ),
            planner_required_source_candidate_count: 2,
            planner_selected_source_candidate_count: 1,
            planner_required_verifier_count: 3,
            target_paths: vec!["ROLE_CODE_NOTE.md".to_string()],
            path_redacted: true,
            content_redacted: true,
            evidence_refs: vec!["integration:candidate-selection-receipt".to_string()],
            generated_at_ms: 1_000,
        };
        let json = serde_json::to_string(&receipt).expect("selection receipt json");
        assert!(json.contains("opensks.integration-candidate-selection-receipt.v1"));
        assert!(json.contains("\"selected_source_candidate_ids\""));
        let decoded: IntegrationCandidateSelectionReceipt =
            serde_json::from_str(&json).expect("decode selection receipt");
        assert_eq!(
            decoded.schema,
            INTEGRATION_CANDIDATE_SELECTION_RECEIPT_SCHEMA
        );
        assert_eq!(
            decoded.approval_policy_id.as_deref(),
            Some("safe-interactive")
        );
        assert_eq!(
            decoded
                .turn_settings
                .as_ref()
                .map(|settings| settings.pipeline_id.as_str()),
            Some("integration-test-pipeline")
        );
        assert_eq!(decoded.candidate_pool.len(), 1);
        assert_eq!(decoded.planned_verifier_count, 2);
        assert_eq!(
            decoded.shard_policy_id.as_deref(),
            Some("planner-shard-policy-11111111")
        );
        assert_eq!(decoded.planner_required_source_candidate_count, 2);
        assert_eq!(decoded.planner_selected_source_candidate_count, 1);
        assert_eq!(decoded.planner_required_verifier_count, 3);
        assert_eq!(
            decoded.selected_candidate_ref,
            "artifact://.opensks/runtime/integration-candidates/run-1/candidate.json"
        );
        assert_eq!(
            decoded.required_verification_gates,
            vec![
                "candidate_receipt_valid".to_string(),
                "target_policy_check".to_string(),
                "patch_apply_check".to_string(),
                "approval_event".to_string()
            ]
        );
    }

    fn integration_turn_settings_fixture() -> IntegrationTurnSettingsSnapshot {
        let settings = ConversationTurnSettings {
            model: ModelSelection {
                mode: turn::ModelSelectionMode::Pinned,
                model_id: Some("provider-1/code-model".to_string()),
                fallback_model_ids: vec!["provider-1/fallback-code".to_string()],
            },
            reasoning_effort: ReasoningEffort::Deep,
            execution_mode: ExecutionMode::Worktree,
            pipeline_id: "integration-test-pipeline".to_string(),
            graph_revision: Some("graph-rev-1".to_string()),
            max_parallelism: 6,
            verifier_count: 3,
            tool_policy_id: "integration-tools".to_string(),
            approval_policy_id: "safe-interactive".to_string(),
            token_budget: Some(12_000),
            cost_budget_usd: Some(2.5),
            timeout_ms: Some(45_000),
            image_model_id: Some("provider-1/image-model".to_string()),
        };
        IntegrationTurnSettingsSnapshot::from(&settings)
    }

    #[test]
    fn integration_cleanup_receipt_roundtrips() {
        let cleanup_ref =
            "artifact://.opensks/runtime/integration-candidates/run-1/cleanup.json".to_string();
        let receipt = IntegrationCleanupReceipt {
            schema: INTEGRATION_CLEANUP_RECEIPT_SCHEMA.to_string(),
            id: "integration-cleanup-run-1".to_string(),
            run_id: "run-1".to_string(),
            candidate_id: "integration-candidate-run-1".to_string(),
            state: "cleaned".to_string(),
            reason_code: "source_isolations_removed".to_string(),
            integration_ref:
                "artifact://.opensks/runtime/integration-candidates/run-1/integration.json"
                    .to_string(),
            seal_ref: "artifact://.opensks/runtime/integration-candidates/run-1/seal.json"
                .to_string(),
            cleanup_ref: cleanup_ref.clone(),
            source_isolations: vec![IntegrationCleanupTarget {
                source_isolation_id: "isolation-run-1-turn-supervisor".to_string(),
                source_isolation_mode: "git_worktree".to_string(),
                worker_id: "turn-supervisor".to_string(),
                work_item_id: None,
                existed: true,
                removed: true,
                reason_code: "source_isolation_removed".to_string(),
            }],
            cleanup_target_count: 1,
            cleaned_count: 1,
            retained_candidate_ref:
                "artifact://.opensks/runtime/integration-candidates/run-1/candidate.json"
                    .to_string(),
            retained_patch_ref:
                "artifact://.opensks/runtime/integration-candidates/run-1/candidate.patch"
                    .to_string(),
            retained_final_diff_ref:
                "artifact://.opensks/runtime/integration-candidates/run-1/final.diff".to_string(),
            path_redacted: true,
            content_redacted: true,
            evidence_refs: vec!["integration:cleanup-receipt".to_string()],
            generated_at_ms: 1_000,
        };
        let json = serde_json::to_string(&receipt).expect("cleanup receipt json");
        assert!(json.contains("opensks.integration-cleanup-receipt.v1"));
        assert!(json.contains("\"cleanup_ref\""));
        let decoded: IntegrationCleanupReceipt =
            serde_json::from_str(&json).expect("decode cleanup receipt");
        assert_eq!(decoded.schema, INTEGRATION_CLEANUP_RECEIPT_SCHEMA);
        assert_eq!(decoded.cleanup_ref, cleanup_ref);
        assert_eq!(decoded.cleanup_target_count, 1);
        assert_eq!(decoded.cleaned_count, 1);
        assert!(decoded.path_redacted);
        assert!(decoded.content_redacted);
    }

    #[test]
    fn worktree_isolation_inventory_receipt_roundtrips() {
        let inventory_ref =
            "artifact://.opensks/runtime/worktrees/run-1/inventory.json".to_string();
        let receipt = WorktreeIsolationInventoryReceipt {
            schema: WORKTREE_ISOLATION_INVENTORY_RECEIPT_SCHEMA.to_string(),
            id: "worktree-inventory-run-1".to_string(),
            run_id: "run-1".to_string(),
            state: "present".to_string(),
            reason_code: "runtime_isolations_discovered".to_string(),
            inventory_ref: inventory_ref.clone(),
            isolations: vec![WorktreeIsolationInventoryEntry {
                isolation_id: "isolation-run-1-turn-supervisor".to_string(),
                run_id: "run-1".to_string(),
                worker_id: "turn-supervisor".to_string(),
                mode: IsolationMode::GitWorktree,
                artifact_ref: "artifact://.opensks/runtime/worktrees/run-1/turn-supervisor"
                    .to_string(),
                exists: true,
                has_git_metadata: true,
                path_redacted: true,
                content_redacted: true,
                reason_code: "runtime_isolation_present".to_string(),
            }],
            isolation_count: 1,
            git_available: true,
            path_redacted: true,
            content_redacted: true,
            evidence_refs: vec!["git:worktree-inventory".to_string()],
            generated_at_ms: 1_000,
        };
        let json = serde_json::to_string(&receipt).expect("inventory receipt json");
        assert!(json.contains("opensks.worktree-isolation-inventory-receipt.v1"));
        assert!(json.contains("\"inventory_ref\""));
        let decoded: WorktreeIsolationInventoryReceipt =
            serde_json::from_str(&json).expect("decode inventory receipt");
        assert_eq!(decoded.schema, WORKTREE_ISOLATION_INVENTORY_RECEIPT_SCHEMA);
        assert_eq!(decoded.inventory_ref, inventory_ref);
        assert_eq!(decoded.isolation_count, 1);
        assert!(decoded.path_redacted);
        assert_eq!(decoded.isolations[0].mode, IsolationMode::GitWorktree);
    }

    #[test]
    fn worktree_isolation_recovery_receipt_roundtrips() {
        let recovery_ref = "artifact://.opensks/runtime/worktrees/run-1/recovery.json".to_string();
        let receipt = WorktreeIsolationRecoveryReceipt {
            schema: WORKTREE_ISOLATION_RECOVERY_RECEIPT_SCHEMA.to_string(),
            id: "worktree-recovery-run-1".to_string(),
            run_id: "run-1".to_string(),
            state: "recovered".to_string(),
            reason_code: "runtime_isolations_recovered".to_string(),
            inventory_ref: "artifact://.opensks/runtime/worktrees/run-1/inventory.json".to_string(),
            recovery_ref: recovery_ref.clone(),
            targets: vec![WorktreeIsolationRecoveryTarget {
                isolation_id: "isolation-run-1-turn-supervisor".to_string(),
                run_id: "run-1".to_string(),
                worker_id: "turn-supervisor".to_string(),
                mode: IsolationMode::Snapshot,
                artifact_ref: "artifact://.opensks/runtime/worktrees/run-1/turn-supervisor"
                    .to_string(),
                existed: true,
                removed: true,
                reason_code: "source_isolation_removed".to_string(),
            }],
            target_count: 1,
            recovered_count: 1,
            prune_attempted: true,
            prune_succeeded: true,
            path_redacted: true,
            content_redacted: true,
            evidence_refs: vec!["git:worktree-recovery".to_string()],
            generated_at_ms: 1_000,
        };
        let json = serde_json::to_string(&receipt).expect("recovery receipt json");
        assert!(json.contains("opensks.worktree-isolation-recovery-receipt.v1"));
        assert!(json.contains("\"recovery_ref\""));
        let decoded: WorktreeIsolationRecoveryReceipt =
            serde_json::from_str(&json).expect("decode recovery receipt");
        assert_eq!(decoded.schema, WORKTREE_ISOLATION_RECOVERY_RECEIPT_SCHEMA);
        assert_eq!(decoded.recovery_ref, recovery_ref);
        assert_eq!(decoded.target_count, 1);
        assert_eq!(decoded.recovered_count, 1);
        assert!(decoded.prune_attempted);
        assert!(decoded.path_redacted);
    }

    #[test]
    fn worktree_isolation_requests_roundtrip_with_run_id_scope() {
        let inventory = EngineRequest::worktree_inventory("req-worktree-inventory", "run-1");
        let inventory_json =
            serde_json::to_string(&inventory).expect("worktree inventory request json");
        assert!(inventory_json.contains("\"kind\":\"worktree_inventory\""));
        assert!(inventory_json.contains("\"run_id\":\"run-1\""));
        assert!(inventory_json.contains("\"scope\":\"worktree_inventory\""));
        let decoded_inventory: EngineRequest =
            serde_json::from_str(&inventory_json).expect("decode inventory request");
        assert_eq!(decoded_inventory.kind, EngineRequestKind::WorktreeInventory);
        assert_eq!(decoded_inventory.params.run_id.as_deref(), Some("run-1"));

        let recovery = EngineRequest::worktree_recover("req-worktree-recover", "run-1");
        let recovery_json =
            serde_json::to_string(&recovery).expect("worktree recover request json");
        assert!(recovery_json.contains("\"kind\":\"worktree_recover\""));
        assert!(recovery_json.contains("\"scope\":\"worktree_recover\""));
        let decoded_recovery: EngineRequest =
            serde_json::from_str(&recovery_json).expect("decode recover request");
        assert_eq!(decoded_recovery.kind, EngineRequestKind::WorktreeRecover);
        assert_eq!(
            decoded_recovery.params.reason_code.as_deref(),
            Some("worktree_recover_requested")
        );
    }

    #[test]
    fn approval_requests_roundtrip_with_scope_and_decision() {
        let request = EngineRequest::approval_request(
            "req-approval",
            "run-1",
            "approval-1",
            "git_push",
            "Approve push",
        );
        let json = serde_json::to_string(&request).expect("approval request json");
        assert!(json.contains("\"kind\":\"approval_request\""));
        assert!(json.contains("\"approval_id\":\"approval-1\""));
        assert!(json.contains("\"scope\":\"git_push\""));
        let decoded: EngineRequest = serde_json::from_str(&json).expect("decode approval request");
        assert_eq!(decoded.kind, EngineRequestKind::ApprovalRequest);
        assert_eq!(decoded.params.approval_id.as_deref(), Some("approval-1"));

        let approved = EngineRequest::approval_decision("req-approve", "run-1", "approval-1", true);
        let approved_json = serde_json::to_string(&approved).expect("approval decision json");
        assert!(approved_json.contains("\"kind\":\"approval_approve\""));
        let decoded: EngineRequest =
            serde_json::from_str(&approved_json).expect("decode approval decision");
        assert_eq!(decoded.kind, EngineRequestKind::ApprovalApprove);
        assert_eq!(
            decoded.params.reason_code.as_deref(),
            Some("approved_by_user")
        );
        assert_eq!(EventKind::ApprovalDenied.as_str(), "approval_denied");
    }

    #[test]
    fn outbox_dispatch_request_and_report_roundtrip() {
        let request = EngineRequest::outbox_dispatch(
            "req-outbox",
            "main",
            Some("approval-push-main".to_string()),
        );
        let json = serde_json::to_string(&request).expect("outbox request json");
        assert!(json.contains("\"kind\":\"outbox_dispatch\""));
        assert!(json.contains("\"target_id\":\"main\""));
        assert!(json.contains("\"approval_id\":\"approval-push-main\""));
        let decoded: EngineRequest = serde_json::from_str(&json).expect("decode outbox request");
        assert_eq!(decoded.kind, EngineRequestKind::OutboxDispatch);

        let report = OutboxDispatchReport {
            schema: OUTBOX_DISPATCH_REPORT_SCHEMA.to_string(),
            item_id: "push-main".to_string(),
            action: OutboxAction::Push,
            target: "main".to_string(),
            approval_id: Some("approval-push-main".to_string()),
            executed: false,
            state: "awaiting_approval".to_string(),
            reason_code: "approval_required".to_string(),
            attempt_count: 0,
            evidence_refs: vec!["daemon:outbox-dispatch".to_string()],
        };
        let report_json = serde_json::to_string(&report).expect("outbox report json");
        assert!(report_json.contains("\"schema\":\"opensks.outbox-dispatch-report.v1\""));
        assert!(report_json.contains("\"executed\":false"));
    }

    #[test]
    fn subscribe_events_request_roundtrips_since_sequence() {
        let mut request = EngineRequest::subscribe_events("req-sub", "run-1", Some(7));
        request.params.tail_ms = Some(250);
        request.params.poll_interval_ms = Some(25);
        let json = serde_json::to_string(&request).expect("subscribe request json");
        assert!(json.contains("\"kind\":\"subscribe_events\""));
        assert!(json.contains("\"run_id\":\"run-1\""));
        assert!(json.contains("\"since_sequence\":7"));
        assert!(json.contains("\"tail_ms\":250"));
        assert!(json.contains("\"poll_interval_ms\":25"));
        let decoded: EngineRequest = serde_json::from_str(&json).expect("decode subscribe");
        assert_eq!(decoded.kind, EngineRequestKind::SubscribeEvents);
        assert_eq!(decoded.params.since_sequence, Some(7));
        assert_eq!(decoded.params.tail_ms, Some(250));
        assert_eq!(decoded.params.poll_interval_ms, Some(25));
    }

    #[test]
    fn terminal_suggestion_roundtrip_preserves_required_fields() {
        let suggestion = TerminalSuggestion {
            schema: TERMINAL_SUGGESTION_SCHEMA.to_string(),
            id: "suggestion-1".to_string(),
            replacement: "cargo test -p opensks-contracts".to_string(),
            display: "cargo test -p opensks-contracts".to_string(),
            description: "Run the focused contract test package.".to_string(),
            source: TerminalSuggestionSource::ProjectCatalog,
            confidence: 0.92,
            risk: TerminalRiskLevel::Safe,
            requires_approval: false,
            evidence_refs: vec!["catalog:contracts-tests".to_string()],
        };

        let json = serde_json::to_string(&suggestion).expect("serialize terminal suggestion");
        assert!(json.contains("\"schema\":\"opensks.terminal-suggestion.v1\""));
        assert!(json.contains("\"source\":\"project_catalog\""));
        let decoded: TerminalSuggestion =
            serde_json::from_str(&json).expect("decode terminal suggestion");
        assert_eq!(decoded.id, "suggestion-1");
        assert_eq!(decoded.replacement, "cargo test -p opensks-contracts");
        assert_eq!(decoded.source, TerminalSuggestionSource::ProjectCatalog);
        assert_eq!(decoded.risk, TerminalRiskLevel::Safe);
        assert!(!decoded.requires_approval);
        assert_eq!(decoded.evidence_refs, vec!["catalog:contracts-tests"]);
    }

    #[test]
    fn terminal_session_start_normalizes_zero_size() {
        let request = TerminalSessionStartRequest {
            schema: TERMINAL_SESSION_SCHEMA.to_string(),
            session_id: "terminal-1".to_string(),
            cwd: "/workspace".to_string(),
            shell: Some("zsh".to_string()),
            env_policy: TerminalEnvPolicy::DenySecrets,
            cols: 0,
            rows: 0,
            started_by: TerminalSessionStarter::SwiftUi,
        };

        assert_eq!(request.normalized_cols(), 20);
        assert_eq!(request.normalized_rows(), 5);

        let oversized = TerminalSessionStartRequest {
            cols: 999,
            rows: 999,
            ..request
        };
        assert_eq!(oversized.normalized_cols(), 500);
        assert_eq!(oversized.normalized_rows(), 200);
    }

    #[test]
    fn terminal_request_kind_serializes_snake_case() {
        let request = EngineRequest::terminal_session_start(
            "req-terminal-start",
            TerminalSessionStartRequest {
                schema: TERMINAL_SESSION_SCHEMA.to_string(),
                session_id: "terminal-1".to_string(),
                cwd: "/workspace".to_string(),
                shell: None,
                env_policy: TerminalEnvPolicy::Minimal,
                cols: 120,
                rows: 32,
                started_by: TerminalSessionStarter::Cli,
            },
        );

        let json = serde_json::to_string(&request).expect("serialize terminal request");
        assert!(json.contains("\"kind\":\"terminal_session_start\""));
        assert!(json.contains("\"terminal_session_start\""));
        let decoded: EngineRequest = serde_json::from_str(&json).expect("decode terminal request");
        assert_eq!(decoded.kind, EngineRequestKind::TerminalSessionStart);
        assert_eq!(
            decoded
                .params
                .terminal_session_start
                .as_ref()
                .map(|payload| payload.started_by.clone()),
            Some(TerminalSessionStarter::Cli)
        );
    }

    #[test]
    fn terminal_risk_decision_blocks_secret_exposure_by_default() {
        let decision = TerminalRiskDecision::default_for_risk(
            "risk-1",
            "cat <redacted-secret-file>",
            TerminalRiskLevel::SecretExposure,
        );

        assert_eq!(decision.schema, TERMINAL_RISK_DECISION_SCHEMA);
        assert_eq!(decision.risk, TerminalRiskLevel::SecretExposure);
        assert_eq!(decision.decision, TerminalExecutionDecision::Block);
        assert!(decision.requires_approval);

        let safe =
            TerminalRiskDecision::default_for_risk("risk-2", "cargo fmt", TerminalRiskLevel::Safe);
        assert_eq!(safe.decision, TerminalExecutionDecision::Allow);
        assert!(!safe.requires_approval);
    }

    #[test]
    fn terminal_enums_tolerate_unknown_labels() {
        let source: TerminalSuggestionSource =
            serde_json::from_str("\"future_completion_source\"").expect("decode source");
        let mode: TerminalAgentMode =
            serde_json::from_str("\"future_agent_mode\"").expect("decode mode");
        assert_eq!(source, TerminalSuggestionSource::Unknown);
        assert_eq!(mode, TerminalAgentMode::Unknown);
    }

    #[test]
    fn terminal_schemas_are_generated() {
        let schemas = schema_jsons().expect("schemas");
        for expected in [
            "terminal-session.schema.json",
            "terminal-event.schema.json",
            "terminal-command-block.schema.json",
            "terminal-suggestion-request.schema.json",
            "terminal-suggestion.schema.json",
            "terminal-agent-turn.schema.json",
            "terminal-risk-decision.schema.json",
            "terminal-mcp-tool-descriptor.schema.json",
        ] {
            assert!(
                schemas.iter().any(|(name, _)| *name == expected),
                "missing generated schema {expected}"
            );
        }
    }

    #[test]
    fn data_plane_manifest_roundtrips_shared_and_local_paths() {
        let manifest = DataPlaneManifest {
            schema: DATA_PLANE_MANIFEST_SCHEMA.to_string(),
            version: "2026-06-21.p0".to_string(),
            managed_by: "opensks-contracts".to_string(),
            default_gitignore_block_ref: ".gitignore#OpenSKS managed local state".to_string(),
            shared_paths: vec![DataPlanePathRule {
                path: ".opensks/wiki/records/".to_string(),
                plane: DataPlane::SharedDurable,
                git_tracking: GitTrackingPolicy::Track,
                retention: "keep_until_superseded".to_string(),
                contains_secrets: false,
                allows_machine_absolute_paths: false,
                allows_raw_provider_responses: false,
                notes: "Merge-friendly shared project memory shards.".to_string(),
            }],
            local_paths: vec![DataPlanePathRule {
                path: ".opensks/runtime/".to_string(),
                plane: DataPlane::EphemeralLocal,
                git_tracking: GitTrackingPolicy::Ignore,
                retention: "gc_safe_after_inactive".to_string(),
                contains_secrets: false,
                allows_machine_absolute_paths: true,
                allows_raw_provider_responses: false,
                notes: "Engine database, leases, and local process state.".to_string(),
            }],
            invariants: vec!["secret_local_paths_never_tracked".to_string()],
        };
        let json = serde_json::to_string(&manifest).expect("serialize data plane manifest");
        assert!(json.contains("\"schema\":\"opensks.data-plane-manifest.v1\""));
        assert!(json.contains("\"plane\":\"shared_durable\""));
        assert!(json.contains("\"git_tracking\":\"ignore\""));
        let decoded: DataPlaneManifest =
            serde_json::from_str(&json).expect("decode data plane manifest");
        assert_eq!(
            decoded.shared_paths[0].git_tracking,
            GitTrackingPolicy::Track
        );
        assert_eq!(decoded.local_paths[0].plane, DataPlane::EphemeralLocal);
    }

    #[test]
    fn provider_connection_and_receipts_are_secretless() {
        let codex_lb_kind = serde_json::to_string(&ProviderKind::CodexLb)
            .expect("serialize codex-lb provider kind");
        assert_eq!(codex_lb_kind, "\"codex_lb\"");
        let decoded_codex_lb: ProviderKind =
            serde_json::from_str("\"codex_lb\"").expect("decode codex-lb provider kind");
        assert_eq!(decoded_codex_lb, ProviderKind::CodexLb);

        let secret_ref =
            SecretRef::macos_keychain("ai.opensks.provider.openrouter", "provider-1", 3);
        let connection = ProviderConnection {
            schema: PROVIDER_CONNECTION_SCHEMA.to_string(),
            id: "provider-1".to_string(),
            kind: ProviderKind::OpenRouter,
            display_name: "OpenRouter".to_string(),
            enabled: true,
            endpoint: ProviderEndpoint {
                base_url: "https://openrouter.ai/api/v1".to_string(),
                allow_insecure_http: false,
            },
            auth: secret_ref.clone(),
            organization_ref: None,
            project_ref: None,
            health: ProviderHealthSnapshot::unknown(),
            concurrency: ProviderConcurrencyPolicy {
                max_concurrent_requests: 4,
                requests_per_minute: Some(60),
                tokens_per_minute: None,
            },
            created_at_ms: 10,
            updated_at_ms: 20,
            revision: 1,
        };
        let json = serde_json::to_string(&connection).expect("provider connection json");
        assert!(json.contains("\"store\":\"macos_keychain\""));
        assert!(json.contains("\"service\":\"ai.opensks.provider.openrouter\""));
        assert!(!json.contains("sk-"));
        assert!(!json.contains("api_key"));
        let decoded: ProviderConnection =
            serde_json::from_str(&json).expect("decode provider connection");
        assert_eq!(decoded.auth.version, 3);
        assert_eq!(decoded.health.reason_code, "not_probed");

        let receipt = ProviderMutationReceipt {
            schema: PROVIDER_MUTATION_SCHEMA.to_string(),
            provider_id: "provider-1".to_string(),
            mutation: ProviderMutationKind::CredentialReplaced,
            revision: 2,
            secret_ref: Some(secret_ref),
            secret_value_exposed: false,
            occurred_at_ms: 30,
            reason_code: "keychain_secret_ref_replaced".to_string(),
        };
        let receipt_json = serde_json::to_string(&receipt).expect("provider mutation receipt");
        assert!(receipt_json.contains("\"secret_value_exposed\":false"));
        assert!(!receipt_json.contains("plaintext"));
    }

    #[test]
    fn provider_probe_and_catalog_roundtrip_without_raw_response() {
        let mut role_scores = BTreeMap::new();
        role_scores.insert(
            ModelRole::Code,
            RoleScore {
                score: 0.91,
                evidence_refs: vec!["catalog-sync:model-card".to_string()],
            },
        );
        let entry = ModelCatalogEntry {
            schema: MODEL_CATALOG_ENTRY_SCHEMA.to_string(),
            id: "provider-1/openai-compatible-code".to_string(),
            provider_id: "provider-1".to_string(),
            remote_model_id: "openai-compatible-code".to_string(),
            display_name: "OpenAI Compatible Code".to_string(),
            enabled: true,
            capabilities: ModelCapabilities::text_code(),
            limits: ModelLimits {
                max_input_tokens: Some(128_000),
                max_output_tokens: Some(16_000),
                requests_per_minute: Some(120),
                tokens_per_minute: None,
                max_concurrency: Some(4),
            },
            pricing: None,
            health: HealthState::Healthy,
            role_scores,
            catalog_revision: "catalog-rev-1".to_string(),
        };
        let entry_json = serde_json::to_string(&entry).expect("catalog entry");
        let decoded_entry: ModelCatalogEntry =
            serde_json::from_str(&entry_json).expect("decode catalog entry");
        assert_eq!(decoded_entry.provider_id, "provider-1");
        assert!(decoded_entry.capabilities.code);

        let receipt = ProviderProbeReceipt {
            schema: PROVIDER_PROBE_RECEIPT_SCHEMA.to_string(),
            provider_id: "provider-1".to_string(),
            endpoint_host_redacted: "openrouter.ai".to_string(),
            http_category: ProviderProbeHttpCategory::Success,
            latency_bucket: LatencyBucket::Under1S,
            auth_accepted: true,
            model_list_available: true,
            catalog_count: Some(42),
            occurred_at_ms: 40,
            reason_code: "probe_ok".to_string(),
            diagnostic_ref: Some("provider-diagnostic/provider-1/40".to_string()),
        };
        let receipt_json = serde_json::to_string(&receipt).expect("probe receipt");
        assert!(receipt_json.contains("\"http_category\":\"success\""));
        assert!(!receipt_json.contains("raw_response"));
        let decoded_receipt: ProviderProbeReceipt =
            serde_json::from_str(&receipt_json).expect("decode probe receipt");
        assert_eq!(decoded_receipt.catalog_count, Some(42));
    }

    #[test]
    fn release_proof_decodes_remediation_actions() {
        let proof_json = serde_json::json!({
            "schema": RELEASE_PROOF_SCHEMA,
            "version": "0.1.0",
            "blockers": [{
                "code": "signed_app_missing",
                "message": "release proof requires production app signing evidence"
            }],
            "remediation_actions": [{
                "blocker": "signed_app_missing",
                "action": "Build and sign the macOS app with a production Developer ID Application identity, then rerun release proof.",
                "scope": "release_signing"
            }],
            "signed_app": false,
            "notarized": false,
            "rollback_plan_ref": ".opensks/updater/rollback-plan.json",
            "fresh_install_checked": true,
            "fresh_clone_checked": true,
            "upgrade_checked": true,
            "status": "not_verified"
        });
        let proof: ReleaseProof = serde_json::from_value(proof_json).expect("decode release proof");
        assert_eq!(proof.remediation_actions.len(), 1);
        assert_eq!(proof.remediation_actions[0].blocker, "signed_app_missing");
        assert_eq!(proof.remediation_actions[0].scope, "release_signing");

        let legacy_json = serde_json::json!({
            "schema": RELEASE_PROOF_SCHEMA,
            "version": "0.1.0",
            "signed_app": false,
            "notarized": false,
            "rollback_plan_ref": ".opensks/updater/rollback-plan.json",
            "fresh_install_checked": true,
            "fresh_clone_checked": true,
            "upgrade_checked": true,
            "status": "not_verified"
        });
        let legacy: ReleaseProof =
            serde_json::from_value(legacy_json).expect("decode legacy release proof");
        assert!(legacy.remediation_actions.is_empty());
    }

    #[test]
    fn execution_event_schema_is_generated() {
        let schemas = schema_jsons().expect("schemas");
        let execution = schemas
            .iter()
            .find(|(name, _)| *name == "execution-event.schema.json")
            .expect("execution schema");
        assert!(execution.1.contains("trust_status"));
        assert!(execution.1.contains("reason_code"));
    }

    #[test]
    fn graph_and_model_schemas_cover_next_runtime_contracts() {
        let schemas = schema_jsons().expect("schemas");
        let graph = schemas
            .iter()
            .find(|(name, _)| *name == "pipeline-graph.schema.json")
            .expect("graph schema");
        assert!(graph.1.contains("final_seal_required"));
        assert!(graph.1.contains("entry_nodes"));

        let objective_request = schemas
            .iter()
            .find(|(name, _)| *name == "objective-plan-request.schema.json")
            .expect("objective plan request schema");
        assert!(objective_request.1.contains("max_parallelism"));
        assert!(objective_request.1.contains("require_git_worktree"));

        let objective_receipt = schemas
            .iter()
            .find(|(name, _)| *name == "objective-plan-receipt.schema.json")
            .expect("objective plan receipt schema");
        assert!(objective_receipt.1.contains("graph_hash"));
        assert!(objective_receipt.1.contains("compiled_plan_ref"));

        let model = schemas
            .iter()
            .find(|(name, _)| *name == "model-profile.schema.json")
            .expect("model schema");
        assert!(model.1.contains("structured_output"));
        assert!(model.1.contains("config_ref"));

        let provider_connection = schemas
            .iter()
            .find(|(name, _)| *name == "provider-connection.schema.json")
            .expect("provider connection schema");
        assert!(provider_connection.1.contains("SecretRef"));
        assert!(provider_connection.1.contains("revision"));

        let probe = schemas
            .iter()
            .find(|(name, _)| *name == "provider-probe-receipt.schema.json")
            .expect("provider probe schema");
        assert!(probe.1.contains("endpoint_host_redacted"));
        assert!(probe.1.contains("latency_bucket"));

        let data_plane = schemas
            .iter()
            .find(|(name, _)| *name == "data-plane-manifest.schema.json")
            .expect("data plane schema");
        assert!(data_plane.1.contains("shared_paths"));
        assert!(data_plane.1.contains("local_paths"));
    }
}
