//! Project identity contracts (PR-024).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `schema` marker for a project summary.
pub const PROJECT_SUMMARY_SCHEMA: &str = "opensks.project-summary.v1";

/// A workspace-scoped project. `id` is a stable opaque identifier; `workspace_key`
/// is the local registration key for an opened workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectSummary {
    pub schema: String,
    pub id: String,
    pub workspace_key: String,
    pub display_name: String,
    pub last_conversation_id: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}
