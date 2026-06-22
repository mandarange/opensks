use std::collections::BTreeMap;

use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};

pub const CONTRACT_VERSION: &str = "opensks.contracts.v1";
pub const ENGINE_REQUEST_SCHEMA: &str = "opensks.engine-request.v1";
pub const ENGINE_EVENT_SCHEMA: &str = "opensks.engine-event.v1";
pub const EXECUTION_EVENT_SCHEMA: &str = "opensks.execution-event.v1";
pub const EXECUTION_EVENT_ENVELOPE_SCHEMA: &str = "opensks.execution-event-envelope.v1";
pub const WORK_ITEM_SCHEMA: &str = "opensks.work-item.v1";
pub const MODEL_PROFILE_SCHEMA: &str = "opensks.model-profile.v1";
pub const PROVIDER_DESCRIPTOR_SCHEMA: &str = "opensks.provider-descriptor.v1";
pub const ROUTING_DECISION_SCHEMA: &str = "opensks.routing-decision.v1";
pub const SCHEDULER_WORK_ITEM_SCHEMA: &str = "opensks.scheduler-work-item.v1";
pub const CONCURRENCY_DECISION_SCHEMA: &str = "opensks.concurrency-decision.v1";
pub const PIPELINE_GRAPH_SCHEMA: &str = "opensks.pipeline-graph.v1";
pub const COMPILED_PLAN_SCHEMA: &str = "opensks.compiled-plan.v1";
pub const GIT_ISOLATION_SCHEMA: &str = "opensks.git-isolation.v1";
pub const PATCH_ENVELOPE_SCHEMA: &str = "opensks.patch-envelope.v1";
pub const COMPLETION_PROOF_SCHEMA: &str = "opensks.completion-proof.v1";
pub const HOOK_SPEC_SCHEMA: &str = "opensks.hook-spec.v1";
pub const HOOK_DECISION_SCHEMA: &str = "opensks.hook-decision.v1";
pub const CODEGRAPH_RECORD_SCHEMA: &str = "opensks.codegraph-record.v1";
pub const CODEGRAPH_INDEX_SCHEMA: &str = "opensks.codegraph-index.v1";
pub const TRIWIKI_RECORD_SCHEMA: &str = "opensks.triwiki-record.v1";
pub const CONTEXT_PACK_SCHEMA: &str = "opensks.context-pack.v1";
pub const IMAGE_ASSET_SCHEMA: &str = "opensks.image-asset.v1";
pub const IMAGE_LEDGER_SCHEMA: &str = "opensks.image-ledger.v1";
pub const REASONING_REPORT_SCHEMA: &str = "opensks.reasoning-report.v1";
pub const OUTBOX_ITEM_SCHEMA: &str = "opensks.outbox-item.v1";
pub const OUTBOX_DISPATCH_REPORT_SCHEMA: &str = "opensks.outbox-dispatch-report.v1";
pub const DATA_PLANE_MANIFEST_SCHEMA: &str = "opensks.data-plane-manifest.v1";
pub const RETENTION_PLAN_SCHEMA: &str = "opensks.retention-plan.v1";
pub const RELEASE_PROOF_SCHEMA: &str = "opensks.release-proof.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EngineRequestKind {
    Hello,
    Health,
    SubscribeEvents,
    RunStart,
    RunPause,
    RunResume,
    RunCancel,
    RunSteer,
    ApprovalRequest,
    ApprovalApprove,
    ApprovalDeny,
    OutboxDispatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EngineEventType {
    EngineHello,
    EngineHealth,
    ExecutionEvent,
    Error,
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
            "snapshot_written" => Self::SnapshotWritten,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EngineRequest {
    pub schema: String,
    pub id: String,
    pub kind: EngineRequestKind,
    #[serde(default)]
    pub protocol_version: String,
    #[serde(default)]
    pub params: EngineRequestParams,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct EngineRequestParams {
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
    Routed,
    BlockedMissingCapability,
    BlockedDisabled,
    BlockedPolicy,
    BlockedProviderHealth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelRejection {
    pub model_id: String,
    pub reason_code: String,
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
    pub model_selector: Option<String>,
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
    #[serde(default)]
    pub approval_points: Vec<ApprovalPoint>,
    pub proof_contract: CompletionContract,
    #[serde(default)]
    pub diagnostics: Vec<CompileDiagnostic>,
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
    pub final_seal_node_ids: Vec<String>,
    pub evidence_required: bool,
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
pub struct ContextPack {
    pub schema: String,
    pub id: String,
    pub token_budget: u32,
    pub estimated_tokens: u32,
    #[serde(default)]
    pub record_ids: Vec<String>,
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
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImageLedger {
    pub schema: String,
    #[serde(default)]
    pub assets: Vec<ImageAsset>,
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
pub struct ReleaseProof {
    pub schema: String,
    pub version: String,
    pub signed_app: bool,
    pub notarized: bool,
    pub rollback_plan_ref: String,
    pub fresh_install_checked: bool,
    pub fresh_clone_checked: bool,
    pub upgrade_checked: bool,
    pub status: TrustStatus,
}

pub mod conversation;
pub mod design;
pub mod file;
pub mod git;
pub mod git_mutation;
pub mod git_push;
pub mod project;
pub mod projection;
pub mod stream;
pub mod text_diff;

pub use conversation::{
    CONVERSATION_DIGEST_SCHEMA, CONVERSATION_MESSAGE_SCHEMA, CONVERSATION_SUMMARY_SCHEMA,
    ConversationDeleteCounts, ConversationDigest, ConversationFilter, ConversationMessage,
    ConversationRunRelation, ConversationStatus, ConversationSummary, MessageRole, MessageState,
    TitleSource,
};
pub use design::{
    DESIGN_PACKAGE_COMPONENTS_SCHEMA, DESIGN_PACKAGE_MANIFEST_SCHEMA, DESIGN_PACKAGE_TOKENS_SCHEMA,
    DesignContentHash, DesignPackageComponent, DesignPackageComponents, DesignPackageFiles,
    DesignPackageManifest, DesignPackageSecurity, DesignPackageSource, DesignPackageToken,
    DesignPackageTokens,
};
pub use file::{
    ConflictResolution, FileServiceError, LineEnding, OPEN_TEXT_REQUEST_SCHEMA, OpenTextRequest,
    SAVE_TEXT_REQUEST_SCHEMA, SAVE_TEXT_RESULT_SCHEMA, STAT_REQUEST_SCHEMA, SaveTextRequest,
    SaveTextResult, StatRequest, TEXT_DOCUMENT_SCHEMA, TextDocument, TextEncoding,
    WORKSPACE_ENTRY_SCHEMA, WorkspaceEntry,
};
pub use git::{
    GIT_BRANCHES_SCHEMA, GIT_DIFF_SCHEMA, GIT_STATUS_SCHEMA, GitBranchInfo, GitBranches, GitDiff,
    GitDiffFile, GitDiffHunk, GitStatus, GitStatusEntry, GitStatusKind,
};
pub use git_mutation::{
    GIT_COMMIT_PREVIEW_SCHEMA, GIT_COMMIT_SCHEMA, GIT_CREATE_BRANCH_SCHEMA, GIT_ERROR_SCHEMA,
    GIT_STAGE_SCHEMA, GIT_SWITCH_PREFLIGHT_SCHEMA, GIT_SWITCH_SCHEMA, GIT_UNSTAGE_SCHEMA,
    GitCommit, GitCommitPreview, GitCreateBranch, GitMutationError, GitMutationErrorBody,
    GitMutationErrorCode, GitStageRejectReason, GitStageRejection, GitStageResult, GitSwitch,
    GitSwitchBlocker, GitSwitchBlockerKind, GitSwitchPreflight, GitUnstageResult,
};
pub use git_push::{
    PUSH_APPROVAL_SCHEMA, PUSH_ERROR_SCHEMA, PUSH_INTENT_SCHEMA, PUSH_RECEIPT_SCHEMA,
    PUSH_STATUS_SCHEMA, PushApproval, PushError, PushErrorBody, PushErrorCode, PushIntent,
    PushReceipt, PushStatus,
};
pub use project::{PROJECT_SUMMARY_SCHEMA, ProjectSummary};
pub use projection::{
    NodeExecutionProjection, NodeProjectionState, PIPELINE_EXECUTION_PROJECTION_SCHEMA,
    PIPELINE_EXECUTION_PROJECTION_VERSION, PipelineExecutionProjection, RunMetrics,
    RunProjectionState,
};
pub use stream::{
    ENGINE_STREAM_FRAME_SCHEMA, EngineStreamFrame, PublicEngineError, STREAM_PROTOCOL_VERSION,
};
pub use text_diff::{DiffHunk, DiffHunkKind, TEXT_DIFF_SCHEMA, TextDiff};

pub fn schema_jsons() -> Result<Vec<(&'static str, String)>, serde_json::Error> {
    Ok(vec![
        (
            "project-summary.schema.json",
            serde_json::to_string_pretty(&schema_for!(ProjectSummary))?,
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
            "compiled-plan.schema.json",
            serde_json::to_string_pretty(&schema_for!(CompiledPlan))?,
        ),
        (
            "git-isolation.schema.json",
            serde_json::to_string_pretty(&schema_for!(GitIsolationReport))?,
        ),
        (
            "patch-envelope.schema.json",
            serde_json::to_string_pretty(&schema_for!(PatchEnvelope))?,
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
            "push-status.schema.json",
            serde_json::to_string_pretty(&schema_for!(PushStatus))?,
        ),
        (
            "push-error.schema.json",
            serde_json::to_string_pretty(&schema_for!(PushError))?,
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

        let model = schemas
            .iter()
            .find(|(name, _)| *name == "model-profile.schema.json")
            .expect("model schema");
        assert!(model.1.contains("structured_output"));
        assert!(model.1.contains("config_ref"));

        let data_plane = schemas
            .iter()
            .find(|(name, _)| *name == "data-plane-manifest.schema.json")
            .expect("data plane schema");
        assert!(data_plane.1.contains("shared_paths"));
        assert!(data_plane.1.contains("local_paths"));
    }
}
