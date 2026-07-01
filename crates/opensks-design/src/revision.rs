//! Design revisions (PR-040): propose / accept / reject / rollback a change to a
//! design package, with every revision linked to a stable **proof ref**.
//!
//! A revision is the unit of reviewed design change. It moves through a small
//! state machine:
//!
//! ```text
//! propose -> Proposed
//! Proposed --accept--> Accepted
//! Proposed --reject---> Rejected
//! (Accepted|Rejected) --rollback--> RolledBack  (restores the prior state)
//! ```
//!
//! Each revision carries a `proof_ref` — a stable evidence identifier that links
//! the revision to the audit/evidence that justifies it. The repo's evidence ids
//! use an `fnv1a64:` digest convention (see `registry::content_hash` and the
//! design-context pin); there is no pre-existing per-revision proof id scheme, so
//! we generate a stable one here: `design-proof:fnv1a64:<hex>` over the revision
//! identity (id + package). The proof ref is assigned at propose time and never
//! changes across transitions, so an accepted/rolled-back revision still points
//! at the same proof.
//!
//! Revisions persist as one JSON file per revision under
//! `.opensks/design/revisions/<id>.json`, written atomically (temp + rename).
//! `rollback` records the prior state so the transition is auditable and a rolled
//! back revision can report what it reverted from.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// `schema` value for a design-revision record / CLI output.
pub const DESIGN_REVISION_SCHEMA: &str = "opensks.design-revision.v1";

/// Subdirectory under `.opensks/design/` holding per-revision records.
const REVISIONS_DIR: &str = "revisions";

/// The lifecycle state of a revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionState {
    Proposed,
    Accepted,
    Rejected,
    RolledBack,
}

impl RevisionState {
    /// Stable wire string for the contract `state` field.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::RolledBack => "rolled_back",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "proposed" => Some(Self::Proposed),
            "accepted" => Some(Self::Accepted),
            "rejected" => Some(Self::Rejected),
            "rolled_back" => Some(Self::RolledBack),
            _ => None,
        }
    }
}

/// A persisted design revision: identity, the package it targets, its current
/// state, the proof it is linked to, and (when rolled back) the state it
/// reverted from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub revision_id: String,
    pub package_id: String,
    pub state: RevisionState,
    pub proof_ref: String,
    /// The state this revision held before a rollback, when applicable. Lets a
    /// rolled-back revision report what it reverted from.
    pub previous_state: Option<RevisionState>,
}

impl Revision {
    /// Render the `opensks.design-revision.v1` contract JSON.
    pub fn to_json(&self) -> String {
        format!(
            "{{\"schema\":{},\"revision_id\":{},\"package_id\":{},\"state\":{},\"proof_ref\":{}}}",
            json_str(DESIGN_REVISION_SCHEMA),
            json_str(&self.revision_id),
            json_str(&self.package_id),
            json_str(self.state.as_str()),
            json_str(&self.proof_ref),
        )
    }

    /// The persisted record JSON (a superset of the wire JSON: also records the
    /// prior state for rollback auditing).
    fn to_record_json(&self) -> String {
        format!(
            "{{\"schema\":{},\"revision_id\":{},\"package_id\":{},\"state\":{},\"proof_ref\":{},\"previous_state\":{}}}",
            json_str(DESIGN_REVISION_SCHEMA),
            json_str(&self.revision_id),
            json_str(&self.package_id),
            json_str(self.state.as_str()),
            json_str(&self.proof_ref),
            match &self.previous_state {
                Some(state) => json_str(state.as_str()),
                None => "null".to_string(),
            },
        )
    }

    fn from_record_json(text: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        Some(Self {
            revision_id: value.get("revision_id")?.as_str()?.to_string(),
            package_id: value.get("package_id")?.as_str()?.to_string(),
            state: RevisionState::from_str(value.get("state")?.as_str()?)?,
            proof_ref: value.get("proof_ref")?.as_str()?.to_string(),
            previous_state: value
                .get("previous_state")
                .and_then(|v| v.as_str())
                .and_then(RevisionState::from_str),
        })
    }
}

/// Errors from the revision lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionError {
    /// The revision id is not a safe single path segment.
    InvalidRevisionId { id: String },
    /// No revision with this id exists.
    NotFound { id: String },
    /// The requested transition is not legal from the revision's current state.
    IllegalTransition {
        id: String,
        from: RevisionState,
        action: &'static str,
    },
    /// The persisted record could not be parsed.
    Corrupt { id: String },
    /// A revision id was claimed by a concurrent proposer before this one could
    /// persist; the caller should retry with a fresh id.
    IdCollision { id: String },
    /// A filesystem error.
    Io { reason: String },
}

impl std::fmt::Display for RevisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRevisionId { id } => {
                write!(f, "design_revision_invalid_id: {id}")
            }
            Self::NotFound { id } => write!(f, "design_revision_not_found: {id}"),
            Self::IllegalTransition { id, from, action } => write!(
                f,
                "design_revision_illegal_transition: {id} cannot {action} from {}",
                from.as_str()
            ),
            Self::Corrupt { id } => write!(f, "design_revision_corrupt: {id}"),
            Self::IdCollision { id } => write!(f, "design_revision_id_collision: {id}"),
            Self::Io { reason } => write!(f, "design_revision_io: {reason}"),
        }
    }
}

impl std::error::Error for RevisionError {}

impl From<io::Error> for RevisionError {
    fn from(value: io::Error) -> Self {
        Self::Io {
            reason: value.to_string(),
        }
    }
}

/// Propose a new revision for `package_id`. Generates a stable revision id and a
/// linked proof ref, persists the record in the `Proposed` state, and returns it.
pub fn propose_revision(workspace: &Path, package_id: &str) -> Result<Revision, RevisionError> {
    loop {
        let revision_id = next_revision_id(workspace, package_id)?;
        let proof_ref = proof_ref_for(&revision_id, package_id);
        let revision = Revision {
            revision_id,
            package_id: package_id.to_string(),
            state: RevisionState::Proposed,
            proof_ref,
            previous_state: None,
        };
        match persist(workspace, &revision, true) {
            Ok(()) => return Ok(revision),
            Err(RevisionError::IdCollision { .. }) => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Accept a proposed revision. Only a `Proposed` revision may be accepted.
pub fn accept_revision(workspace: &Path, revision_id: &str) -> Result<Revision, RevisionError> {
    transition(workspace, revision_id, "accept", |state| match state {
        RevisionState::Proposed => Ok(RevisionState::Accepted),
        _ => Err(state),
    })
}

/// Reject a proposed revision. Only a `Proposed` revision may be rejected.
pub fn reject_revision(workspace: &Path, revision_id: &str) -> Result<Revision, RevisionError> {
    transition(workspace, revision_id, "reject", |state| match state {
        RevisionState::Proposed => Ok(RevisionState::Rejected),
        _ => Err(state),
    })
}

/// Roll back an accepted or rejected revision, restoring it to `RolledBack` and
/// recording the state it reverted from. A revision already rolled back, or
/// still merely proposed, cannot be rolled back.
pub fn rollback_revision(workspace: &Path, revision_id: &str) -> Result<Revision, RevisionError> {
    transition(workspace, revision_id, "rollback", |state| match state {
        RevisionState::Accepted | RevisionState::Rejected => Ok(RevisionState::RolledBack),
        _ => Err(state),
    })
}

/// Load a single revision by id.
pub fn load_revision(workspace: &Path, revision_id: &str) -> Result<Revision, RevisionError> {
    if !is_safe_segment(revision_id) {
        return Err(RevisionError::InvalidRevisionId {
            id: revision_id.to_string(),
        });
    }
    let path = revision_path(workspace, revision_id);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(RevisionError::NotFound {
                id: revision_id.to_string(),
            });
        }
        Err(error) => return Err(error.into()),
    };
    Revision::from_record_json(&text).ok_or_else(|| RevisionError::Corrupt {
        id: revision_id.to_string(),
    })
}

/// Apply a state transition computed by `next` to a loaded revision, recording
/// the prior state and persisting the result atomically.
fn transition(
    workspace: &Path,
    revision_id: &str,
    action: &'static str,
    next: impl Fn(RevisionState) -> Result<RevisionState, RevisionState>,
) -> Result<Revision, RevisionError> {
    let mut revision = load_revision(workspace, revision_id)?;
    let from = revision.state;
    let to = next(from).map_err(|from| RevisionError::IllegalTransition {
        id: revision_id.to_string(),
        from,
        action,
    })?;
    revision.previous_state = Some(from);
    revision.state = to;
    persist(workspace, &revision, false)?;
    Ok(revision)
}

/// The `.opensks/design/revisions/` directory under a workspace.
fn revisions_dir(workspace: &Path) -> PathBuf {
    workspace
        .join(".opensks")
        .join("design")
        .join(REVISIONS_DIR)
}

/// The persisted record path for a revision id.
fn revision_path(workspace: &Path, revision_id: &str) -> PathBuf {
    revisions_dir(workspace).join(format!("{revision_id}.json"))
}

/// Persist a revision record atomically (temp + rename), so a crash can never
/// leave a torn record. When `create_new` is set, the final file is only
/// created if it does not already exist (surfacing a concurrent id collision
/// as `RevisionError::IdCollision` instead of silently overwriting); otherwise
/// the final file is overwritten in place, which is safe for transitions that
/// update a record already loaded by id.
fn persist(workspace: &Path, revision: &Revision, create_new: bool) -> Result<(), RevisionError> {
    let dir = revisions_dir(workspace);
    fs::create_dir_all(&dir)?;
    let final_path = dir.join(format!("{}.json", revision.revision_id));
    let tmp_path = dir.join(format!(
        ".{}.{}.tmp",
        revision.revision_id,
        std::process::id()
    ));
    fs::write(&tmp_path, revision.to_record_json())?;
    if create_new {
        match fs::hard_link(&tmp_path, &final_path) {
            Ok(()) => {
                let _ = fs::remove_file(&tmp_path);
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&tmp_path);
                Err(RevisionError::IdCollision {
                    id: revision.revision_id.clone(),
                })
            }
            Err(error) => {
                let _ = fs::remove_file(&tmp_path);
                Err(error.into())
            }
        }
    } else {
        fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }
}

/// Compute the next monotonic revision id for a package: `rev-<package>-<n>`,
/// where `n` is one past the highest existing index for that package. Stable and
/// collision-free within a workspace.
fn next_revision_id(workspace: &Path, package_id: &str) -> Result<String, RevisionError> {
    if !is_safe_segment(package_id) {
        return Err(RevisionError::InvalidRevisionId {
            id: package_id.to_string(),
        });
    }
    let prefix = format!("rev-{package_id}-");
    let dir = revisions_dir(workspace);
    let mut max_index = 0_u64;
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            if let Some(index) = stem
                .strip_prefix(&prefix)
                .and_then(|n| n.parse::<u64>().ok())
            {
                max_index = max_index.max(index);
            }
        }
    }
    Ok(format!("{prefix}{}", max_index + 1))
}

/// Build a stable proof ref linking a revision to its evidence. Uses the repo's
/// `fnv1a64:` digest convention over the revision identity so the same
/// `(revision id, package)` always yields the same proof ref.
fn proof_ref_for(revision_id: &str, package_id: &str) -> String {
    let preimage = format!("{revision_id}\u{1f}{package_id}");
    format!("design-proof:{}", fnv1a64(preimage.as_bytes()))
}

/// Stable FNV-1a content hash matching the repo's `fnv1a64:` convention.
fn fnv1a64(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

/// A safe single-path-segment id: non-empty, not `.`/`..`, alnum + `-`/`_`.
fn is_safe_segment(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Minimal JSON string escaper.
fn json_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempWorkspace {
        root: PathBuf,
    }

    impl TempWorkspace {
        fn new(name: &str) -> Self {
            let mut root = std::env::temp_dir();
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            root.push(format!(
                "opensks-design-revision-{name}-{}-{stamp}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("create temp workspace");
            Self {
                root: root.canonicalize().expect("canonicalize"),
            }
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn propose_returns_proposed_with_proof_ref() {
        let ws = TempWorkspace::new("propose");
        let revision = propose_revision(&ws.root, "demo").expect("propose");
        assert_eq!(revision.state, RevisionState::Proposed);
        assert_eq!(revision.package_id, "demo");
        // Each revision is linked to a proof ref.
        assert!(revision.proof_ref.starts_with("design-proof:fnv1a64:"));
        assert!(!revision.proof_ref.is_empty());
        // The proof ref is stable for the same identity.
        assert_eq!(
            revision.proof_ref,
            proof_ref_for(&revision.revision_id, "demo")
        );
        // The wire JSON carries the proof ref.
        assert!(revision.to_json().contains("\"proof_ref\":"));
        assert!(revision.to_json().contains("\"state\":\"proposed\""));
    }

    #[test]
    fn accept_transitions_proposed_to_accepted() {
        let ws = TempWorkspace::new("accept");
        let proposed = propose_revision(&ws.root, "demo").expect("propose");
        let accepted = accept_revision(&ws.root, &proposed.revision_id).expect("accept");
        assert_eq!(accepted.state, RevisionState::Accepted);
        // The proof ref is preserved across the transition.
        assert_eq!(accepted.proof_ref, proposed.proof_ref);
        // Persisted state reflects the transition.
        let reloaded = load_revision(&ws.root, &proposed.revision_id).expect("reload");
        assert_eq!(reloaded.state, RevisionState::Accepted);
    }

    #[test]
    fn reject_transitions_proposed_to_rejected() {
        let ws = TempWorkspace::new("reject");
        let proposed = propose_revision(&ws.root, "demo").expect("propose");
        let rejected = reject_revision(&ws.root, &proposed.revision_id).expect("reject");
        assert_eq!(rejected.state, RevisionState::Rejected);
    }

    #[test]
    fn rollback_restores_prior_state_and_reports_it() {
        let ws = TempWorkspace::new("rollback");
        let proposed = propose_revision(&ws.root, "demo").expect("propose");
        let accepted = accept_revision(&ws.root, &proposed.revision_id).expect("accept");
        assert_eq!(accepted.state, RevisionState::Accepted);

        let rolled = rollback_revision(&ws.root, &proposed.revision_id).expect("rollback");
        assert_eq!(rolled.state, RevisionState::RolledBack);
        // Rollback restores to / records the prior state (Accepted).
        assert_eq!(rolled.previous_state, Some(RevisionState::Accepted));
        // The proof ref survives the whole lifecycle.
        assert_eq!(rolled.proof_ref, proposed.proof_ref);
    }

    #[test]
    fn cannot_accept_an_already_accepted_revision() {
        let ws = TempWorkspace::new("double-accept");
        let proposed = propose_revision(&ws.root, "demo").expect("propose");
        accept_revision(&ws.root, &proposed.revision_id).expect("accept");
        let error =
            accept_revision(&ws.root, &proposed.revision_id).expect_err("re-accept is illegal");
        assert!(matches!(error, RevisionError::IllegalTransition { .. }));
    }

    #[test]
    fn cannot_rollback_a_proposed_revision() {
        let ws = TempWorkspace::new("rollback-proposed");
        let proposed = propose_revision(&ws.root, "demo").expect("propose");
        let error = rollback_revision(&ws.root, &proposed.revision_id)
            .expect_err("rollback of proposed is illegal");
        assert!(matches!(error, RevisionError::IllegalTransition { .. }));
    }

    #[test]
    fn unknown_revision_is_not_found() {
        let ws = TempWorkspace::new("missing");
        let error = accept_revision(&ws.root, "rev-demo-999").expect_err("not found");
        assert!(matches!(error, RevisionError::NotFound { .. }));
    }

    #[test]
    fn revision_ids_are_monotonic_per_package() {
        let ws = TempWorkspace::new("monotonic");
        let first = propose_revision(&ws.root, "demo").expect("propose 1");
        let second = propose_revision(&ws.root, "demo").expect("propose 2");
        assert_ne!(first.revision_id, second.revision_id);
        assert_eq!(first.revision_id, "rev-demo-1");
        assert_eq!(second.revision_id, "rev-demo-2");
    }
}
