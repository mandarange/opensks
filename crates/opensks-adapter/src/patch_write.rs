use std::path::{Path, PathBuf};

use opensks_contracts::{FileOperation, PatchApplyResult};
use opensks_patch_engine::{PatchApplyContext, PatchEngine, PlannedPatchWrite};

use crate::{AgentAdapterError, AgentRunRequest};

/// A single planned file write with the pre-image hash it expects on disk.
#[derive(Debug, Clone)]
pub struct PlannedWrite {
    pub path: String,
    pub expected_before_hash: String,
    pub after_content: String,
    pub operation: FileOperation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchPathLease {
    pub lease_id: String,
    pub fence_token: String,
}

impl PatchPathLease {
    pub fn new(lease_id: impl Into<String>, fence_token: impl Into<String>) -> Self {
        Self {
            lease_id: lease_id.into(),
            fence_token: fence_token.into(),
        }
    }

    pub fn from_scheduler_lease(lease: &opensks_contracts::Lease) -> Self {
        Self::new(lease.id.clone(), lease.id.clone())
    }

    fn scoped_fallback(run_id: &str, proposal_id: &str) -> Self {
        Self::new(
            format!("patch-lease:{run_id}:{proposal_id}"),
            format!("patch-fence:{run_id}:{proposal_id}"),
        )
    }
}

pub(crate) fn patch_path_lease(request: &AgentRunRequest, proposal_id: &str) -> PatchPathLease {
    request
        .patch_lease
        .clone()
        .unwrap_or_else(|| PatchPathLease::scoped_fallback(&request.run_id, proposal_id))
}

/// Apply a set of file writes as a transaction (recovery directive §10.4/§10.5):
///
/// 1. **Validate** every target's on-disk pre-image against
///    `expected_before_hash`. If any file changed since it was read, nothing is
///    written and the conflicting paths are returned — no silent overwrite.
/// 2. **Apply with rollback**: snapshot each target's original content, then
///    write all of them. Any IO failure restores every target to its pre-image
///    so a multi-file patch never lands half-applied.
///
/// A path that escapes the workspace is a hard error (`PathEscape`).
pub fn apply_file_writes(
    workspace: &Path,
    proposal_id: &str,
    writes: &[PlannedWrite],
) -> Result<PatchApplyResult, AgentAdapterError> {
    apply_file_writes_inner(workspace, proposal_id, writes, None)
}

pub fn apply_file_writes_with_path_lease(
    workspace: &Path,
    proposal_id: &str,
    writes: &[PlannedWrite],
    lease: &PatchPathLease,
) -> Result<PatchApplyResult, AgentAdapterError> {
    apply_file_writes_inner(workspace, proposal_id, writes, Some(lease))
}

fn apply_file_writes_inner(
    workspace: &Path,
    proposal_id: &str,
    writes: &[PlannedWrite],
    lease: Option<&PatchPathLease>,
) -> Result<PatchApplyResult, AgentAdapterError> {
    let engine = PatchEngine::open(workspace).map_err(|error| match error {
        opensks_patch_engine::PatchEngineError::PathEscape(path)
        | opensks_patch_engine::PatchEngineError::SymlinkRejected(path) => {
            AgentAdapterError::PathEscape(path)
        }
        other => AgentAdapterError::InvalidInstruction(other.to_string()),
    })?;
    let planned: Vec<PlannedPatchWrite> = writes
        .iter()
        .map(|write| PlannedPatchWrite {
            path: write.path.clone(),
            expected_before_hash: write.expected_before_hash.clone(),
            after_content: write.after_content.clone(),
            operation: write.operation,
            rename_to: None,
        })
        .collect();
    let result = if let Some(lease) = lease {
        let leased_paths = writes.iter().map(|write| write.path.clone());
        let context = PatchApplyContext::new(
            lease.lease_id.clone(),
            lease.fence_token.clone(),
            leased_paths,
        );
        engine.apply_with_context(proposal_id, &planned, &context)
    } else {
        engine.apply(proposal_id, &planned)
    };
    result.map_err(|error| match error {
        opensks_patch_engine::PatchEngineError::PathEscape(path)
        | opensks_patch_engine::PatchEngineError::SymlinkRejected(path) => {
            AgentAdapterError::PathEscape(path)
        }
        other => AgentAdapterError::InvalidInstruction(other.to_string()),
    })
}

/// Resolve a workspace-relative path through the PatchEngine path guard.
pub(crate) fn resolve_in_workspace(
    workspace: &Path,
    rel: &str,
) -> Result<PathBuf, AgentAdapterError> {
    let engine = PatchEngine::open(workspace)
        .map_err(|error| AgentAdapterError::InvalidInstruction(error.to_string()))?;
    engine.resolve(rel).map_err(|error| match error {
        opensks_patch_engine::PatchEngineError::PathEscape(path)
        | opensks_patch_engine::PatchEngineError::SymlinkRejected(path) => {
            AgentAdapterError::PathEscape(path)
        }
        other => AgentAdapterError::InvalidInstruction(other.to_string()),
    })
}

pub(crate) fn content_hash(content: &str) -> String {
    opensks_patch_engine::content_hash(content)
}

pub(crate) fn minimal_unified_diff(path: &str, before: &str, after: &str) -> String {
    opensks_patch_engine::unified_diff(path, before, after)
}
