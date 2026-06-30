//! Conversation turn request/accept + durable thread settings + timeline items
//! (recovery release §5, §6, §11, §20).
//!
//! A conversation turn is the only primary entry point for agent-driven code
//! changes. The app submits a [`ConversationTurnStartRequest`] to the daemon and
//! immediately receives a [`ConversationTurnAccepted`] handle (a run id + a
//! stream id) — it never blocks for the run to finish. Per-thread defaults are
//! persisted as [`ConversationThreadSettings`]; the durable conversation
//! timeline is a sequence of typed [`TimelineItem`]s.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::projection::RunProjectionState;

pub const CONVERSATION_TURN_START_REQUEST_SCHEMA: &str =
    "opensks.conversation-turn-start-request.v1";
pub const CONVERSATION_TURN_ACCEPTED_SCHEMA: &str = "opensks.conversation-turn-accepted.v1";
pub const CONVERSATION_THREAD_SETTINGS_SCHEMA: &str = "opensks.thread-settings.v1";
pub const APPROVAL_POLICY_AUTOPILOT: &str = "autopilot";
pub const APPROVAL_POLICY_MAD_SKS: &str = "mad-sks";
pub const APPROVAL_POLICY_SAFE_INTERACTIVE: &str = "safe-interactive";
pub const TIMELINE_ITEM_SCHEMA: &str = "opensks.timeline-item.v1";

/// Whether the model is auto-routed or pinned by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelSelectionMode {
    Auto,
    Pinned,
}

/// Model choice for a turn/thread. A single configured model auto-selects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelSelection {
    pub mode: ModelSelectionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_model_ids: Vec<String>,
}

/// User-facing reasoning effort. Adapters map these to provider-specific knobs;
/// an adapter that cannot honour a value reports `unsupported` rather than
/// silently ignoring it (§5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Quick,
    Standard,
    Deep,
    Maximum,
}

/// Where a turn is allowed to write (§5.4). `Cloud` is reserved and hidden until
/// a real adapter exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Local,
    Worktree,
    ReadOnly,
    Cloud,
}

/// Effective settings applied to one turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationTurnSettings {
    pub model: ModelSelection,
    pub reasoning_effort: ReasoningEffort,
    pub execution_mode: ExecutionMode,
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
    pub cost_budget_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_model_id: Option<String>,
}

/// The user's message text plus any attachment/context artifact refs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UserMessageInput {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_refs: Vec<String>,
}

/// Context the user explicitly selected for this turn (files, ranges, symbols,
/// diffs, etc.), carried as opaque refs the context assembler resolves.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct TurnContextSelection {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<String>,
}

/// Submitted by the app to start a turn. The daemon validates and replies with
/// [`ConversationTurnAccepted`] without waiting for the run to complete.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationTurnStartRequest {
    pub schema: String,
    pub request_id: String,
    pub project_id: String,
    pub conversation_id: String,
    pub client_turn_id: String,
    pub message: UserMessageInput,
    /// Client-observed durable thread settings revision. The daemon uses this
    /// as a compare-and-snapshot guard before accepting a turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_settings_updated_at_ms: Option<u64>,
    /// Legacy compatibility echo from older clients. Runtime settings are
    /// always snapshotted from durable thread settings instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<ConversationTurnSettings>,
    #[serde(default)]
    pub context: TurnContextSelection,
    pub idempotency_key: String,
}

/// The immediate accept response: identifiers + a stream handle. `state` is
/// `queued` — the run has not finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationTurnAccepted {
    pub schema: String,
    pub request_id: String,
    pub turn_id: String,
    pub run_id: String,
    pub user_message_id: String,
    pub assistant_message_id: String,
    pub stream_id: String,
    pub settings_digest: String,
    pub state: RunProjectionState,
}

/// Durable per-thread settings (persisted in `conversation_settings`). Holds no
/// secrets — only references/ids.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationThreadSettings {
    pub schema: String,
    pub conversation_id: String,
    pub model_selection: ModelSelection,
    pub reasoning_effort: ReasoningEffort,
    pub execution_mode: ExecutionMode,
    pub pipeline_id: String,
    pub max_parallelism: u32,
    pub verifier_count: u32,
    pub tool_policy_id: String,
    pub approval_policy_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_budget_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_model_id: Option<String>,
    pub updated_at_ms: u64,
}

impl ConversationThreadSettings {
    /// A safe default thread settings record for a fresh conversation.
    pub fn default_for(conversation_id: impl Into<String>, now_ms: u64) -> Self {
        Self {
            schema: CONVERSATION_THREAD_SETTINGS_SCHEMA.to_string(),
            conversation_id: conversation_id.into(),
            model_selection: ModelSelection {
                mode: ModelSelectionMode::Auto,
                model_id: None,
                fallback_model_ids: Vec::new(),
            },
            reasoning_effort: ReasoningEffort::Standard,
            execution_mode: ExecutionMode::Worktree,
            pipeline_id: "auto".to_string(),
            max_parallelism: 16,
            verifier_count: 1,
            tool_policy_id: "project-default".to_string(),
            approval_policy_id: APPROVAL_POLICY_AUTOPILOT.to_string(),
            token_budget: None,
            cost_budget_usd: None,
            timeout_ms: None,
            image_model_id: None,
            updated_at_ms: now_ms,
        }
    }
}

/// The kind of a durable conversation timeline item (§11.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimelineItemKind {
    UserMessage,
    AssistantMessage,
    Plan,
    ToolCall,
    Worker,
    Patch,
    Verification,
    Approval,
    CommitReceipt,
    PushReceipt,
    ImageArtifact,
    Warning,
    Error,
}

/// One durable timeline entry. `payload` is already secret-redacted; the DB
/// column is `payload_redacted_json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TimelineItem {
    pub schema: String,
    pub id: String,
    pub project_id: String,
    pub conversation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub sequence: i64,
    pub kind: TimelineItemKind,
    pub state: String,
    pub payload: serde_json::Value,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> ConversationTurnStartRequest {
        ConversationTurnStartRequest {
            schema: CONVERSATION_TURN_START_REQUEST_SCHEMA.to_string(),
            request_id: "req-1".to_string(),
            project_id: "proj-1".to_string(),
            conversation_id: "conv-1".to_string(),
            client_turn_id: "ct-1".to_string(),
            message: UserMessageInput {
                text: "add a test".to_string(),
                attachment_refs: vec![],
            },
            thread_settings_updated_at_ms: Some(42),
            settings: Some(ConversationTurnSettings {
                model: ModelSelection {
                    mode: ModelSelectionMode::Auto,
                    model_id: None,
                    fallback_model_ids: vec![],
                },
                reasoning_effort: ReasoningEffort::Deep,
                execution_mode: ExecutionMode::Worktree,
                pipeline_id: "parallel-build".to_string(),
                graph_revision: None,
                max_parallelism: 8,
                verifier_count: 2,
                tool_policy_id: "project-default".to_string(),
                approval_policy_id: "safe-interactive".to_string(),
                token_budget: Some(100_000),
                cost_budget_usd: Some(1.5),
                timeout_ms: Some(600_000),
                image_model_id: None,
            }),
            context: TurnContextSelection::default(),
            idempotency_key: "idem-1".to_string(),
        }
    }

    #[test]
    fn turn_request_round_trips() {
        let req = sample_request();
        let json = serde_json::to_string(&req).unwrap();
        let parsed: ConversationTurnStartRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, parsed);
    }

    #[test]
    fn turn_request_decodes_without_legacy_settings_echo() {
        let json = r#"{
            "schema":"opensks.conversation-turn-start-request.v1",
            "request_id":"req-1",
            "project_id":"proj-1",
            "conversation_id":"conv-1",
            "client_turn_id":"ct-1",
            "message":{"text":"add a test","attachment_refs":[]},
            "thread_settings_updated_at_ms":42,
            "context":{"refs":[]},
            "idempotency_key":"idem-1"
        }"#;
        let parsed: ConversationTurnStartRequest = serde_json::from_str(json).unwrap();
        assert!(parsed.settings.is_none());
    }

    #[test]
    fn thread_settings_default_to_autopilot_policy() {
        let settings = ConversationThreadSettings::default_for("conv-1", 7);
        assert_eq!(settings.approval_policy_id, APPROVAL_POLICY_AUTOPILOT);
    }

    #[test]
    fn accepted_state_is_not_terminal() {
        let accepted = ConversationTurnAccepted {
            schema: CONVERSATION_TURN_ACCEPTED_SCHEMA.to_string(),
            request_id: "req-1".to_string(),
            turn_id: "turn-1".to_string(),
            run_id: "run-1".to_string(),
            user_message_id: "um-1".to_string(),
            assistant_message_id: "am-1".to_string(),
            stream_id: "stream-1".to_string(),
            settings_digest: "sha256:v1:accepted-settings".to_string(),
            state: RunProjectionState::Queued,
        };
        assert!(!accepted.state.is_terminal());
    }

    #[test]
    fn default_thread_settings_are_auto_and_safe() {
        let settings = ConversationThreadSettings::default_for("conv-1", 42);
        assert_eq!(settings.model_selection.mode, ModelSelectionMode::Auto);
        assert_eq!(settings.execution_mode, ExecutionMode::Worktree);
        assert_eq!(settings.max_parallelism, 16);
        let json = serde_json::to_string(&settings).unwrap();
        let parsed: ConversationThreadSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(settings, parsed);
    }
}
