//! Project Intelligence + Freshness contracts (PR-041).
//!
//! These wire shapes back the `opensks intel` verb. They expose a deterministic
//! freshness stamp (the triple of HEAD / working-tree / codegraph-index hashes),
//! a staleness comparison whose paramount invariant is *stale is never reported
//! as fresh*, a paged codegraph query (so a large graph never serializes whole),
//! and glossary / architecture readers that each carry the current freshness
//! stamp.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `schema` marker for [`FreshnessStamp`].
pub const INTEL_FRESHNESS_SCHEMA: &str = "opensks.intel-freshness.v1";
/// `schema` marker for [`FreshnessCheck`].
pub const INTEL_FRESHNESS_CHECK_SCHEMA: &str = "opensks.intel-freshness-check.v1";
/// `schema` marker for [`CodegraphQuery`].
pub const INTEL_CODEGRAPH_SCHEMA: &str = "opensks.intel-codegraph.v1";
/// `schema` marker for [`Glossary`].
pub const INTEL_GLOSSARY_SCHEMA: &str = "opensks.intel-glossary.v1";
/// `schema` marker for [`Architecture`].
pub const INTEL_ARCHITECTURE_SCHEMA: &str = "opensks.intel-architecture.v1";

/// A deterministic freshness stamp for a workspace at one instant.
///
/// - `head_hash` is the committed `HEAD` object id, or `None` when the workspace
///   is not a Git repository (or has an unborn HEAD).
/// - `worktree_hash` is a content-addressed digest over the tracked working-tree
///   state, so any in-place edit flips it.
/// - `index_hash` is a digest of the codegraph index (the staged knowledge of
///   symbols), so a re-index flips it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FreshnessStamp {
    pub schema: String,
    pub head_hash: Option<String>,
    pub worktree_hash: String,
    pub index_hash: String,
    pub in_repo: bool,
}

/// The reason a stamped freshness diverged from the current stamp. The first
/// divergence found (in HEAD → worktree → index order) is reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StaleReason {
    HeadChanged,
    WorktreeChanged,
    IndexChanged,
}

/// The current stamp values surfaced alongside a [`FreshnessCheck`] so a caller
/// can re-stamp without a second round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FreshnessCurrent {
    pub head_hash: Option<String>,
    pub worktree_hash: String,
    pub index_hash: String,
}

/// The result of comparing a previously *stamped* freshness against the current
/// workspace state.
///
/// `fresh` is `true` **only** when every provided stamp equals the current
/// value. Any divergence — or any provided stamp that cannot be matched, or an
/// empty comparison — yields `fresh: false`. The invariant "stale is never
/// reported as fresh" is paramount: on any doubt this defaults to `false`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FreshnessCheck {
    pub schema: String,
    pub fresh: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<StaleReason>,
    pub current: FreshnessCurrent,
}

/// One paged codegraph hit: a symbol/import/test located at a path and line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodegraphRecordView {
    pub path: String,
    pub symbol: String,
    pub kind: String,
    pub line: u32,
}

/// A paged codegraph query result. `total` is the true match count across all
/// pages; `records` is the `[offset, offset + limit)` slice. The current
/// freshness stamp is attached so the caller can reason about staleness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodegraphQuery {
    pub schema: String,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
    pub records: Vec<CodegraphRecordView>,
    pub freshness: FreshnessStamp,
}

/// One glossary term with its definition and supporting references.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GlossaryTerm {
    pub term: String,
    pub definition: String,
    #[serde(default)]
    pub refs: Vec<String>,
}

/// The workspace glossary, with the current freshness stamp attached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Glossary {
    pub schema: String,
    pub terms: Vec<GlossaryTerm>,
    pub freshness: FreshnessStamp,
}

/// One architecture record: a titled component/decision with detail and refs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArchitectureRecord {
    pub id: String,
    pub title: String,
    pub detail: String,
    #[serde(default)]
    pub refs: Vec<String>,
}

/// The workspace architecture records, with the current freshness stamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Architecture {
    pub schema: String,
    pub records: Vec<ArchitectureRecord>,
    pub freshness: FreshnessStamp,
}
