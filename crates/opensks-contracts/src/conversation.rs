//! Project-scoped conversation/message identity contracts (PR-024).
//!
//! These are durable identity + summary DTOs. Engine dispatch (turn → run) is
//! intentionally NOT part of this module; it lands in PR-027.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const CONVERSATION_SUMMARY_SCHEMA: &str = "opensks.conversation-summary.v1";
pub const CONVERSATION_MESSAGE_SCHEMA: &str = "opensks.conversation-message.v1";
pub const CONVERSATION_DIGEST_SCHEMA: &str = "opensks.conversation-digest.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationStatus {
    Idle,
    Queued,
    Running,
    WaitingForInput,
    WaitingForApproval,
    Failed,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TitleSource {
    Generated,
    User,
    Agent,
    Imported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
    Event,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageState {
    Draft,
    Queued,
    Streaming,
    Complete,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationRunRelation {
    Primary,
    Child,
    Retry,
    Verification,
    Design,
    Image,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationSummary {
    pub schema: String,
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub title_source: TitleSource,
    pub status: ConversationStatus,
    pub pinned: bool,
    pub archived: bool,
    pub message_count: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub last_message_at_ms: Option<u64>,
}

/// A single message. `content_redacted` is the secret-scrubbed, locally
/// searchable copy; the original may be held encrypted out of band.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationMessage {
    pub schema: String,
    pub id: String,
    pub project_id: String,
    pub conversation_id: String,
    pub turn_id: String,
    pub role: MessageRole,
    pub state: MessageState,
    pub content_redacted: String,
    pub sequence: i64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// Sanitized conversation summary (the shared/portable record).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationDigest {
    pub schema: String,
    pub conversation_id: String,
    pub summary_redacted: String,
    pub source_message_sequence: i64,
    pub generated_at_ms: u64,
}

/// Counts returned by a destructive conversation delete (for the confirm UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationDeleteCounts {
    pub messages: u64,
    pub runs: u64,
}

/// Filter for listing conversations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationFilter {
    All,
    Running,
    Pinned,
    Archived,
}
