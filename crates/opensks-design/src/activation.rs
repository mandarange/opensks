//! Atomic design-package activation (PR-040).
//!
//! Exactly one design package may be "active" in a workspace at a time. The
//! active selection is recorded in a single marker file under
//! `.opensks/design/active.json`. Activation is **atomic** in two senses:
//!
//! 1. **Audit-gated.** [`activate_package`] runs the PR-040 audit first. If the
//!    audit blocks activation (any error finding), activation is refused and the
//!    *previously active* package is left exactly as it was — the marker is never
//!    touched on a failing audit.
//! 2. **Crash-safe write.** When the audit passes, the new marker is written to a
//!    temp file in the same directory and `rename(2)`d into place. A crash mid-
//!    write can only leave the old marker or the new one, never a torn/partial
//!    marker, so [`read_active`] always observes a consistent active selection.
//!
//! The marker also records the activated revision id (so an activation that
//! followed an accepted revision can be traced back to it). Reads tolerate a
//! missing marker (no package has ever been activated) by returning `None`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::audit::{DesignAuditError, DesignAuditReport, audit_package};
use crate::registry::DesignRegistry;

/// `schema` value for the active-package marker / `active-status` output.
pub const DESIGN_ACTIVE_SCHEMA: &str = "opensks.design-active.v1";

/// Directory under a workspace that holds Studio activation + revision state.
const DESIGN_STATE_DIR: &str = "design";
/// The active-package marker file name under the design state directory.
const ACTIVE_MARKER_FILE: &str = "active.json";

/// The active design selection for a workspace: which package (if any) is active
/// and which revision (if any) activated it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActiveMarker {
    pub active_package: Option<String>,
    pub activated_revision: Option<String>,
}

impl ActiveMarker {
    /// Render the `opensks.design-active.v1` contract JSON.
    pub fn to_json(&self) -> String {
        format!(
            "{{\"schema\":{},\"active_package\":{},\"activated_revision\":{}}}",
            json_str(DESIGN_ACTIVE_SCHEMA),
            json_opt(self.active_package.as_deref()),
            json_opt(self.activated_revision.as_deref()),
        )
    }

    /// Parse a persisted marker. Tolerant of field order; unknown fields are
    /// ignored. A `null` value maps to `None`.
    fn from_json(text: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        Some(Self {
            active_package: value
                .get("active_package")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            activated_revision: value
                .get("activated_revision")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        })
    }
}

/// Outcome of a successful [`activate_package`]: the now-active package, the
/// package that was active before (if any), and the audit report that passed.
#[derive(Debug, Clone)]
pub struct ActivationOutcome {
    pub package_id: String,
    pub previous_active: Option<String>,
    pub audit: DesignAuditReport,
}

/// Why an activation attempt failed without changing the active selection.
#[derive(Debug, Clone)]
pub enum ActivationError {
    /// The package could not be resolved/validated from the registry.
    PackageUnavailable { id: String, reason: String },
    /// The package's tokens could not be audited at all (malformed set).
    AuditUnrunnable {
        id: String,
        source: DesignAuditError,
    },
    /// The audit produced error findings. Activation is refused; the previous
    /// active package is unchanged. The full report is carried for the caller to
    /// surface the findings.
    AuditFailed { audit: Box<DesignAuditReport> },
    /// A filesystem error writing/reading the marker.
    Io { reason: String },
}

impl std::fmt::Display for ActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PackageUnavailable { id, reason } => {
                write!(f, "design_activate_package_unavailable: {id}: {reason}")
            }
            Self::AuditUnrunnable { id, source } => {
                write!(f, "design_activate_audit_unrunnable: {id}: {source}")
            }
            Self::AuditFailed { audit } => {
                write!(
                    f,
                    "design_activate_audit_failed: {} ({} findings)",
                    audit.package_id,
                    audit.findings.len()
                )
            }
            Self::Io { reason } => write!(f, "design_activate_io: {reason}"),
        }
    }
}

impl std::error::Error for ActivationError {}

impl From<io::Error> for ActivationError {
    fn from(value: io::Error) -> Self {
        Self::Io {
            reason: value.to_string(),
        }
    }
}

/// The `.opensks/design/` state directory under a workspace.
fn design_state_dir(workspace: &Path) -> PathBuf {
    workspace.join(".opensks").join(DESIGN_STATE_DIR)
}

/// The active-marker path under a workspace.
fn active_marker_path(workspace: &Path) -> PathBuf {
    design_state_dir(workspace).join(ACTIVE_MARKER_FILE)
}

/// Read the workspace's active selection. Returns the default (no active
/// package, no revision) when no marker has ever been written, so callers never
/// have to special-case a fresh workspace.
pub fn read_active(workspace: &Path) -> Result<ActiveMarker, ActivationError> {
    let path = active_marker_path(workspace);
    match fs::read_to_string(&path) {
        Ok(text) => Ok(ActiveMarker::from_json(&text).unwrap_or_default()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ActiveMarker::default()),
        Err(error) => Err(error.into()),
    }
}

/// Atomically activate `package_id` after a passing audit.
///
/// Resolves the package from `registry`, loads its tokens/components, and audits
/// them. If the audit blocks activation, returns [`ActivationError::AuditFailed`]
/// **without touching the marker** — the previously active package stays active.
/// On a passing audit, writes the new marker via temp-file + `rename` so the
/// switch is crash-safe. `activated_revision` records the revision that drove the
/// activation, when applicable.
pub fn activate_package(
    workspace: &Path,
    registry: &DesignRegistry,
    package_id: &str,
    activated_revision: Option<&str>,
) -> Result<ActivationOutcome, ActivationError> {
    let previous = read_active(workspace)?.active_package;

    let resolved =
        registry
            .resolve(package_id)
            .map_err(|error| ActivationError::PackageUnavailable {
                id: package_id.to_string(),
                reason: error.reason_code().to_string(),
            })?;
    let tokens = resolved
        .load_tokens()
        .map_err(|error| ActivationError::PackageUnavailable {
            id: package_id.to_string(),
            reason: error.reason_code().to_string(),
        })?;
    let components =
        resolved
            .load_components()
            .map_err(|error| ActivationError::PackageUnavailable {
                id: package_id.to_string(),
                reason: error.reason_code().to_string(),
            })?;

    let audit = audit_package(package_id, &tokens, components.as_ref()).map_err(|source| {
        ActivationError::AuditUnrunnable {
            id: package_id.to_string(),
            source,
        }
    })?;

    if audit.blocks_activation() {
        // ATOMIC GUARANTEE: refuse, and never touch the marker — the previously
        // active package remains active untouched.
        return Err(ActivationError::AuditFailed {
            audit: Box::new(audit),
        });
    }

    let marker = ActiveMarker {
        active_package: Some(package_id.to_string()),
        activated_revision: activated_revision.map(str::to_string),
    };
    write_marker_atomic(workspace, &marker)?;

    Ok(ActivationOutcome {
        package_id: package_id.to_string(),
        previous_active: previous,
        audit,
    })
}

/// Write the active marker via temp file + atomic rename within the design state
/// directory, so a crash mid-write cannot leave a torn marker.
fn write_marker_atomic(workspace: &Path, marker: &ActiveMarker) -> Result<(), ActivationError> {
    let dir = design_state_dir(workspace);
    fs::create_dir_all(&dir)?;
    let final_path = dir.join(ACTIVE_MARKER_FILE);
    let tmp_path = dir.join(format!(".{ACTIVE_MARKER_FILE}.{}.tmp", std::process::id()));
    fs::write(&tmp_path, marker.to_json())?;
    // rename(2) is atomic within a directory: readers see old-or-new, never torn.
    fs::rename(&tmp_path, &final_path)?;
    Ok(())
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

/// Render `Some(s)` as a JSON string and `None` as `null`.
fn json_opt(value: Option<&str>) -> String {
    match value {
        Some(s) => json_str(s),
        None => "null".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::content_hash;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A self-cleaning temp workspace.
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
                "opensks-design-activation-{name}-{}-{stamp}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("create temp workspace");
            Self {
                root: root.canonicalize().expect("canonicalize"),
            }
        }

        fn registry(&self) -> DesignRegistry {
            DesignRegistry::with_default_order(&self.root, None)
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// Write a package under `.opensks/design-systems/<id>/`. `clean` toggles a
    /// passing vs audit-failing token set (the failing one has a 1pt primary
    /// hit target, an unsatisfiable error).
    fn write_package(ws: &TempWorkspace, id: &str, clean: bool) {
        let hit = if clean { 44 } else { 1 };
        let tokens = format!(
            "{{\"schema\":\"opensks.design-token-set.v1\",\"design_system_id\":\"{id}\",\"revision\":1,\"tokens\":[\
            {{\"path\":\"color.canvas\",\"type\":\"color\",\"value\":\"#0E1015\"}},\
            {{\"path\":\"size.hit_target.primary\",\"type\":\"dimension\",\"value\":{hit},\"unit\":\"pt\",\"semantic_role\":\"minimum-primary-hit-target\"}}\
            ]}}"
        );
        let design = "# Title\n\n## Section\n\nBody\n";
        let tokens_hash = content_hash(tokens.as_bytes());
        let design_hash = content_hash(design.as_bytes());
        let manifest = format!(
            "{{\"schema\":\"opensks.design-package.v1\",\"id\":\"{id}\",\"name\":\"{id}\",\"version\":\"1.0.0\",\"license\":\"MIT\",\"description\":\"d\",\"package_schema_version\":1,\"files\":{{\"design\":\"DESIGN.md\",\"tokens\":\"tokens.json\"}},\"content_hashes\":[{{\"path\":\"tokens.json\",\"hash\":\"{tokens_hash}\"}},{{\"path\":\"DESIGN.md\",\"hash\":\"{design_hash}\"}}],\"platforms\":[\"macos-swiftui\"]}}"
        );
        let base = ws.root.join(".opensks/design-systems").join(id);
        fs::create_dir_all(&base).expect("mkdir package");
        fs::write(base.join("manifest.json"), manifest).expect("write manifest");
        fs::write(base.join("tokens.json"), tokens).expect("write tokens");
        fs::write(base.join("DESIGN.md"), design).expect("write design");
    }

    #[test]
    fn fresh_workspace_has_no_active_package() {
        let ws = TempWorkspace::new("fresh");
        let active = read_active(&ws.root).expect("read active");
        assert_eq!(active.active_package, None);
        assert_eq!(active.activated_revision, None);
    }

    #[test]
    fn clean_package_activates_and_status_reflects_it() {
        let ws = TempWorkspace::new("clean-activate");
        write_package(&ws, "good", true);
        let outcome = activate_package(&ws.root, &ws.registry(), "good", None)
            .expect("clean package activates");
        assert_eq!(outcome.package_id, "good");
        assert_eq!(outcome.previous_active, None);
        let active = read_active(&ws.root).expect("read active");
        assert_eq!(active.active_package.as_deref(), Some("good"));
    }

    #[test]
    fn failing_audit_blocks_activation_and_previous_remains() {
        // First, cleanly activate "good".
        let ws = TempWorkspace::new("atomic-previous-remains");
        write_package(&ws, "good", true);
        write_package(&ws, "bad", false);
        activate_package(&ws.root, &ws.registry(), "good", None).expect("activate good");
        assert_eq!(
            read_active(&ws.root).unwrap().active_package.as_deref(),
            Some("good")
        );

        // Now attempt to activate "bad" (a failing audit). Activation must be
        // refused AND the previously active package ("good") must remain.
        let error = activate_package(&ws.root, &ws.registry(), "bad", None)
            .expect_err("failing audit must block activation");
        match error {
            ActivationError::AuditFailed { audit } => {
                assert!(audit.blocks_activation());
                assert!(!audit.findings.is_empty());
            }
            other => panic!("expected AuditFailed, got {other}"),
        }
        // ATOMICITY PROOF: the active package is still "good", untouched.
        assert_eq!(
            read_active(&ws.root).unwrap().active_package.as_deref(),
            Some("good"),
            "previous active package must remain after a failing activation"
        );
    }

    #[test]
    fn activation_with_revision_records_revision_id() {
        let ws = TempWorkspace::new("with-revision");
        write_package(&ws, "good", true);
        activate_package(&ws.root, &ws.registry(), "good", Some("rev-123"))
            .expect("activate with revision");
        let active = read_active(&ws.root).expect("read active");
        assert_eq!(active.active_package.as_deref(), Some("good"));
        assert_eq!(active.activated_revision.as_deref(), Some("rev-123"));
    }

    #[test]
    fn marker_json_roundtrips() {
        let marker = ActiveMarker {
            active_package: Some("pkg".to_string()),
            activated_revision: Some("rev-1".to_string()),
        };
        let json = marker.to_json();
        assert!(json.contains("\"schema\":\"opensks.design-active.v1\""));
        let parsed = ActiveMarker::from_json(&json).expect("parse");
        assert_eq!(parsed, marker);

        let empty = ActiveMarker::default();
        assert!(empty.to_json().contains("\"active_package\":null"));
    }
}
