//! Explicit streaming protocol v2 (PR-026).
//!
//! Every opened stream is framed: a `stream_opened`, zero or more
//! `event`/`snapshot`/`heartbeat` frames, and exactly one terminal frame
//! (`stream_completed` or `stream_failed`) — never silence-as-completion. Each
//! frame carries a monotonically increasing `cursor` within its stream so a
//! client can dedup and reconnect from the last accepted cursor.
//!
//! The `event` / `snapshot` payloads are opaque JSON here; the typed
//! `ExecutionEventEnvelope` v2 and `PipelineExecutionProjection` arrive in PR-029.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const ENGINE_STREAM_FRAME_SCHEMA: &str = "opensks.engine-stream-frame.v2";
pub const STREAM_PROTOCOL_VERSION: &str = "opensks.stream.v2";

/// A concise, safe error for the wire (no secrets / private paths).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PublicEngineError {
    pub schema: String,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(default)]
    pub remediation: Option<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub redacted: bool,
}

impl PublicEngineError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            schema: "opensks.public-engine-error.v1".to_string(),
            code: code.into(),
            message: message.into(),
            retryable,
            remediation: None,
            evidence_refs: Vec::new(),
            redacted: true,
        }
    }
}

/// The v2 stream wire envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "frame_type", rename_all = "snake_case")]
pub enum EngineStreamFrame {
    StreamOpened {
        schema: String,
        stream_id: String,
        request_id: String,
        project_id: String,
        conversation_id: String,
        run_id: Option<String>,
        protocol_version: String,
        cursor: u64,
    },
    Event {
        schema: String,
        stream_id: String,
        cursor: u64,
        event: serde_json::Value,
    },
    Snapshot {
        schema: String,
        stream_id: String,
        cursor: u64,
        projection: serde_json::Value,
    },
    Heartbeat {
        schema: String,
        stream_id: String,
        cursor: u64,
        server_time_ms: u64,
    },
    StreamCompleted {
        schema: String,
        stream_id: String,
        cursor: u64,
        reason_code: String,
    },
    StreamFailed {
        schema: String,
        stream_id: String,
        cursor: u64,
        error: PublicEngineError,
        resumable: bool,
    },
}

impl EngineStreamFrame {
    pub fn stream_id(&self) -> &str {
        match self {
            EngineStreamFrame::StreamOpened { stream_id, .. }
            | EngineStreamFrame::Event { stream_id, .. }
            | EngineStreamFrame::Snapshot { stream_id, .. }
            | EngineStreamFrame::Heartbeat { stream_id, .. }
            | EngineStreamFrame::StreamCompleted { stream_id, .. }
            | EngineStreamFrame::StreamFailed { stream_id, .. } => stream_id,
        }
    }

    pub fn cursor(&self) -> u64 {
        match self {
            EngineStreamFrame::StreamOpened { cursor, .. }
            | EngineStreamFrame::Event { cursor, .. }
            | EngineStreamFrame::Snapshot { cursor, .. }
            | EngineStreamFrame::Heartbeat { cursor, .. }
            | EngineStreamFrame::StreamCompleted { cursor, .. }
            | EngineStreamFrame::StreamFailed { cursor, .. } => *cursor,
        }
    }

    /// A terminal frame ends the stream. Exactly one terminal frame is emitted
    /// per stream unless the transport dies.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            EngineStreamFrame::StreamCompleted { .. } | EngineStreamFrame::StreamFailed { .. }
        )
    }
}
