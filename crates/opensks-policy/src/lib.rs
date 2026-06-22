use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use opensks_contracts::{
    DATA_PLANE_MANIFEST_SCHEMA, DataPlane, DataPlaneManifest, DataPlanePathRule, GitTrackingPolicy,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PermissionScope {
    ReadWorkspace,
    WriteWorkspace,
    RunCommand,
    GitWrite,
    GitPush,
    ExternalNetwork,
    ProviderCall,
    SecretRead,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionDecision {
    pub allowed: bool,
    pub reason_code: String,
    pub approval_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct PermissionPolicy {
    pub allow_external_network: bool,
    pub allow_git_push_without_approval: bool,
    pub allow_destructive_without_approval: bool,
}

impl PermissionPolicy {
    pub fn decide(&self, scope: PermissionScope) -> PermissionDecision {
        match scope {
            PermissionScope::ExternalNetwork if !self.allow_external_network => {
                PermissionDecision {
                    allowed: false,
                    reason_code: "blocked_external_network_requires_policy".to_string(),
                    approval_required: true,
                }
            }
            PermissionScope::GitPush if !self.allow_git_push_without_approval => {
                PermissionDecision {
                    allowed: false,
                    reason_code: "blocked_git_push_requires_outbox_approval".to_string(),
                    approval_required: true,
                }
            }
            PermissionScope::GitWrite | PermissionScope::WriteWorkspace => PermissionDecision {
                allowed: true,
                reason_code: "allowed_workspace_mutation_with_path_policy".to_string(),
                approval_required: false,
            },
            PermissionScope::SecretRead => PermissionDecision {
                allowed: false,
                reason_code: "blocked_secret_value_read".to_string(),
                approval_required: true,
            },
            _ => PermissionDecision {
                allowed: true,
                reason_code: "allowed_by_default_local_policy".to_string(),
                approval_required: false,
            },
        }
    }
}

/// A single least-privilege capability the workspace may be granted. The set is
/// closed and deny-by-default: a capability that is not present in a
/// [`WorkspaceCapabilities`] grant is denied. `FilesystemWorkspace` only ever
/// authorizes paths that canonicalize *inside* the workspace root — it never
/// implies access to arbitrary machine paths.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Read/write files whose canonical path stays under the workspace root.
    FilesystemWorkspace,
    /// Reach the network outside the local machine (provider calls, fetches).
    ExternalNetwork,
    /// Push commits to a remote (an irreversible external side effect).
    GitPush,
    /// Run destructive operations (history rewrite, recursive delete, etc.).
    DestructiveOps,
}

impl Capability {
    /// Stable wire/reason-code token for this capability.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FilesystemWorkspace => "filesystem_workspace",
            Self::ExternalNetwork => "external_network",
            Self::GitPush => "git_push",
            Self::DestructiveOps => "destructive_ops",
        }
    }
}

/// Why a capability or filesystem check was rejected. Carries a stable
/// `reason_code` so callers can branch and so audits stay deterministic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CapabilityDenied {
    /// The requested capability was never granted (deny-by-default).
    NotGranted { capability: Capability },
    /// A filesystem path escaped the canonical workspace root.
    PathEscape { reason_code: String },
}

impl CapabilityDenied {
    pub fn reason_code(&self) -> String {
        match self {
            Self::NotGranted { capability } => {
                format!("blocked_capability_not_granted:{}", capability.as_str())
            }
            Self::PathEscape { reason_code } => reason_code.clone(),
        }
    }
}

impl core::fmt::Display for CapabilityDenied {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.reason_code())
    }
}

impl std::error::Error for CapabilityDenied {}

/// The typed, least-privilege capability grant for a single workspace. Empty by
/// construction: nothing is allowed until it is explicitly added. The workspace
/// root is canonicalized once so every filesystem check compares against a real,
/// symlink-resolved anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceCapabilities {
    /// Canonical (symlink-resolved) workspace root. All authorized filesystem
    /// paths must canonicalize to a descendant of this directory.
    pub workspace_root: PathBuf,
    /// The set of granted capabilities. Anything absent is denied by default.
    pub granted: BTreeSet<Capability>,
}

impl WorkspaceCapabilities {
    /// Build a deny-by-default grant anchored at `workspace_root`. The root is
    /// canonicalized so later containment checks resolve symlinks consistently;
    /// if it cannot be canonicalized (e.g. it does not exist) the path is used
    /// as-is and filesystem checks will then reject anything that cannot be
    /// canonicalized into it.
    pub fn deny_by_default(workspace_root: impl AsRef<Path>) -> Self {
        let root = workspace_root.as_ref();
        let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        Self {
            workspace_root: canonical,
            granted: BTreeSet::new(),
        }
    }

    /// Grant a capability (builder-style). Filesystem access still only ever
    /// authorizes in-workspace paths even when `FilesystemWorkspace` is granted.
    #[must_use]
    pub fn grant(mut self, capability: Capability) -> Self {
        self.granted.insert(capability);
        self
    }

    /// True iff `capability` was explicitly granted.
    pub fn is_granted(&self, capability: Capability) -> bool {
        self.granted.contains(&capability)
    }

    /// Deny-by-default capability check. Returns `Ok(())` only when the
    /// capability is in the grant set.
    pub fn check_capability(&self, capability: Capability) -> Result<(), CapabilityDenied> {
        if self.is_granted(capability) {
            Ok(())
        } else {
            Err(CapabilityDenied::NotGranted { capability })
        }
    }

    /// Authorize a filesystem path. This is the load-bearing escape check:
    ///
    /// 1. `FilesystemWorkspace` must be granted (deny-by-default), AND
    /// 2. the path must resolve *inside* the canonical workspace root.
    ///
    /// Resolution canonicalizes the deepest existing ancestor (so symlinks
    /// anywhere along the path are followed to their real target) and then
    /// re-appends the not-yet-created tail. A `..` that climbs out, an absolute
    /// path outside the root, and a symlink whose target lands outside the root
    /// are all rejected with a stable `reason_code`.
    pub fn check_path(&self, candidate: impl AsRef<Path>) -> Result<PathBuf, CapabilityDenied> {
        self.check_capability(Capability::FilesystemWorkspace)?;
        let resolved = self.resolve_under_root(candidate.as_ref())?;
        if resolved.starts_with(&self.workspace_root) {
            Ok(resolved)
        } else {
            Err(CapabilityDenied::PathEscape {
                reason_code: "blocked_path_escapes_workspace_root".to_string(),
            })
        }
    }

    /// Resolve `candidate` (absolute, or relative to the workspace root) into a
    /// canonical absolute path. Symlinks are followed by canonicalizing the
    /// longest existing prefix; the remaining components are normalized
    /// lexically. `..` is never allowed to pop above the workspace root once
    /// resolution begins, and a symlink that points outside is surfaced because
    /// its canonicalized prefix will no longer be under the root.
    fn resolve_under_root(&self, candidate: &Path) -> Result<PathBuf, CapabilityDenied> {
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.workspace_root.join(candidate)
        };

        // Split into the longest existing ancestor (canonicalized, resolving any
        // symlinks) plus the non-existent tail. Each popped segment is captured
        // as an owned `OsString` so we can keep mutating `existing`.
        let mut existing = joined.clone();
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        let canonical_existing = loop {
            match existing.canonicalize() {
                Ok(canonical) => break canonical,
                Err(_) => {
                    let name = match existing.file_name() {
                        Some(name) => name.to_os_string(),
                        None => return Ok(lexically_normalize(&joined)),
                    };
                    tail.push(name);
                    if !existing.pop() {
                        // Nothing of this path exists on disk; fall back to a
                        // lexical normalization rooted at the workspace.
                        return Ok(lexically_normalize(&joined));
                    }
                }
            }
        };

        let mut resolved = canonical_existing;
        for raw in tail.into_iter().rev() {
            match Path::new(&raw).components().next() {
                Some(Component::ParentDir) => {
                    if !resolved.pop() {
                        return Err(CapabilityDenied::PathEscape {
                            reason_code: "blocked_path_escapes_workspace_root".to_string(),
                        });
                    }
                }
                Some(Component::CurDir) => {}
                _ => resolved.push(&raw),
            }
        }
        Ok(resolved)
    }
}

/// Lexically normalize a path (resolve `.` and `..` textually) without touching
/// the filesystem. Used only for paths whose components do not yet exist on
/// disk, so there are no symlinks to follow.
fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn shared_rule(path: &str, retention: &str, notes: &str) -> DataPlanePathRule {
    DataPlanePathRule {
        path: path.to_string(),
        plane: DataPlane::SharedDurable,
        git_tracking: GitTrackingPolicy::Track,
        retention: retention.to_string(),
        contains_secrets: false,
        allows_machine_absolute_paths: false,
        allows_raw_provider_responses: false,
        notes: notes.to_string(),
    }
}

fn local_rule(
    path: &str,
    plane: DataPlane,
    retention: &str,
    contains_secrets: bool,
    allows_machine_absolute_paths: bool,
    allows_raw_provider_responses: bool,
    notes: &str,
) -> DataPlanePathRule {
    DataPlanePathRule {
        path: path.to_string(),
        plane,
        git_tracking: GitTrackingPolicy::Ignore,
        retention: retention.to_string(),
        contains_secrets,
        allows_machine_absolute_paths,
        allows_raw_provider_responses,
        notes: notes.to_string(),
    }
}

pub fn default_data_plane_manifest() -> DataPlaneManifest {
    DataPlaneManifest {
        schema: DATA_PLANE_MANIFEST_SCHEMA.to_string(),
        version: "2026-06-21.p0".to_string(),
        managed_by: "OpenSKS managed local state block plus data-plane manifest".to_string(),
        default_gitignore_block_ref: ".gitignore#OpenSKS managed local state".to_string(),
        shared_paths: vec![
            shared_rule(
                ".opensks/data-plane-manifest.json",
                "keep_until_replaced_by_new_contract",
                "Machine-readable tracked/local path policy for first-sprint bootstrap.",
            ),
            shared_rule(
                ".opensks/design-systems/",
                "keep_until_superseded",
                "Portable, tracked design-system packages (manifest + tokens + DESIGN + components). Discovered by the design registry; must stay trackable.",
            ),
            shared_rule(
                ".opensks/wiki/records/",
                "keep_until_superseded",
                "Merge-friendly shared project memory shards.",
            ),
            shared_rule(
                ".opensks/history/runs/*/summary.json",
                "keep_compact_history",
                "Compact run summaries that are safe to share.",
            ),
            shared_rule(
                ".opensks/history/runs/*/proof.json",
                "keep_compact_history",
                "Completion proof envelopes without raw logs or secrets.",
            ),
            shared_rule(
                ".opensks/history/runs/*/events.digest.json",
                "keep_compact_history",
                "Digest-only event evidence for portable history.",
            ),
            shared_rule(
                ".opensks/pipelines/templates/",
                "keep_until_template_removed",
                "Default pipeline graph templates.",
            ),
            shared_rule(
                ".opensks/architecture/",
                "keep_until_superseded",
                "Architecture records visible in Project Intelligence.",
            ),
            shared_rule(
                ".opensks/glossary/",
                "keep_until_superseded",
                "Shared glossary records with freshness metadata.",
            ),
        ],
        local_paths: vec![
            local_rule(
                ".opensks/runtime/",
                DataPlane::EphemeralLocal,
                "gc_safe_after_inactive",
                false,
                true,
                false,
                "Engine database, leases, sockets, and process-local state.",
            ),
            local_rule(
                ".opensks/cache/",
                DataPlane::EphemeralLocal,
                "rebuildable",
                false,
                false,
                false,
                "Local caches and generated context shards.",
            ),
            local_rule(
                ".opensks/design-cache/",
                DataPlane::EphemeralLocal,
                "rebuildable",
                false,
                false,
                false,
                "Compiled design adapters and import temp scratch; rebuildable from tracked design-system packages.",
            ),
            local_rule(
                ".opensks/tmp/",
                DataPlane::EphemeralLocal,
                "delete_on_gc",
                false,
                true,
                true,
                "Temporary files and transient provider fragments.",
            ),
            local_rule(
                ".opensks/logs/",
                DataPlane::LocalDurable,
                "bounded_retention",
                false,
                true,
                true,
                "Local raw logs derived from structured events.",
            ),
            local_rule(
                ".opensks/secrets/",
                DataPlane::SecretLocal,
                "never_publish",
                true,
                true,
                false,
                "Secret material placeholders only; real secret values stay out of Git.",
            ),
            local_rule(
                ".opensks/worktrees/",
                DataPlane::EphemeralLocal,
                "delete_after_run_close",
                false,
                true,
                false,
                "Git worktree or snapshot isolation directories.",
            ),
            local_rule(
                ".opensks/workers/",
                DataPlane::EphemeralLocal,
                "delete_after_run_close",
                false,
                true,
                false,
                "Local worker runtime leases, heartbeats, bus, routing, and final state.",
            ),
            local_rule(
                ".opensks/macos/",
                DataPlane::LocalDurable,
                "rebuildable",
                false,
                true,
                false,
                "Local generated app bundle.",
            ),
            local_rule(
                ".opensks/providers/local/",
                DataPlane::LocalDurable,
                "keep_local_only",
                false,
                true,
                false,
                "Local provider probes and endpoint state.",
            ),
            local_rule(
                ".opensks/pipelines/compiled/",
                DataPlane::EphemeralLocal,
                "rebuildable",
                false,
                false,
                false,
                "Compiled pipeline plans generated from source graphs.",
            ),
            local_rule(
                ".opensks/wiki/indexes/",
                DataPlane::EphemeralLocal,
                "rebuildable",
                false,
                false,
                false,
                "Search indexes generated from shared wiki records.",
            ),
            local_rule(
                ".opensks/wiki/context-packs/generated/",
                DataPlane::EphemeralLocal,
                "rebuildable",
                false,
                false,
                false,
                "Generated context packs with token budgets.",
            ),
            local_rule(
                ".opensks/history/raw/",
                DataPlane::LocalDurable,
                "bounded_retention",
                false,
                true,
                true,
                "Raw local run evidence that is not safe as shared history.",
            ),
            local_rule(
                ".opensks/assets/candidates/",
                DataPlane::EphemeralLocal,
                "gc_after_selection",
                false,
                false,
                false,
                "Temporary image candidates before selection or proof capture.",
            ),
        ],
        invariants: vec![
            "secret_local_paths_never_tracked".to_string(),
            "shared_paths_must_not_contain_raw_provider_responses".to_string(),
            "shared_paths_must_not_contain_machine_absolute_paths".to_string(),
            "ephemeral_local_paths_must_be_rebuildable_or_gc_safe".to_string(),
            "broad_dot_opensks_ignore_is_not_allowed".to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a unique, real temp directory anchored under the OS temp dir and
    /// return its canonicalized path. Avoids adding a `tempfile` dependency.
    fn make_temp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "opensks-policy-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp root");
        dir.canonicalize().expect("canonicalize temp root")
    }

    #[test]
    fn capabilities_are_deny_by_default() {
        let caps = WorkspaceCapabilities::deny_by_default(std::env::temp_dir());
        assert!(caps.check_capability(Capability::ExternalNetwork).is_err());
        assert!(caps.check_capability(Capability::GitPush).is_err());
        assert!(caps.check_capability(Capability::DestructiveOps).is_err());
        assert!(
            caps.check_capability(Capability::FilesystemWorkspace)
                .is_err()
        );
    }

    #[test]
    fn ungranted_network_push_destructive_are_denied_until_granted() {
        let root = make_temp_root("ungranted");
        let caps = WorkspaceCapabilities::deny_by_default(&root)
            .grant(Capability::ExternalNetwork)
            .grant(Capability::GitPush);
        assert!(caps.check_capability(Capability::ExternalNetwork).is_ok());
        assert!(caps.check_capability(Capability::GitPush).is_ok());
        // Destructive was never granted.
        let denied = caps
            .check_capability(Capability::DestructiveOps)
            .expect_err("destructive must stay denied");
        assert_eq!(
            denied.reason_code(),
            "blocked_capability_not_granted:destructive_ops"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn in_workspace_path_is_allowed() {
        let root = make_temp_root("inworkspace");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(root.join("src/main.rs"), b"// ok").expect("write file");
        let caps =
            WorkspaceCapabilities::deny_by_default(&root).grant(Capability::FilesystemWorkspace);

        // Existing in-workspace file resolves inside the root.
        let resolved = caps
            .check_path("src/main.rs")
            .expect("in-workspace allowed");
        assert!(resolved.starts_with(&root));
        // A not-yet-created in-workspace path is also allowed.
        let new_file = caps
            .check_path("src/generated/out.json")
            .expect("new in-workspace path allowed");
        assert!(new_file.starts_with(&root));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn filesystem_path_denied_without_capability() {
        let root = make_temp_root("nofscap");
        let caps = WorkspaceCapabilities::deny_by_default(&root);
        let denied = caps
            .check_path("src/main.rs")
            .expect_err("filesystem must be denied without the capability");
        assert_eq!(
            denied.reason_code(),
            "blocked_capability_not_granted:filesystem_workspace"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn parent_dir_escape_is_denied() {
        let root = make_temp_root("escape");
        let caps =
            WorkspaceCapabilities::deny_by_default(&root).grant(Capability::FilesystemWorkspace);
        let denied = caps
            .check_path("../outside.txt")
            .expect_err("../ escape must be denied");
        assert_eq!(denied.reason_code(), "blocked_path_escapes_workspace_root");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn absolute_outside_path_is_denied() {
        let root = make_temp_root("absolute");
        let caps =
            WorkspaceCapabilities::deny_by_default(&root).grant(Capability::FilesystemWorkspace);
        // An existing absolute path outside the root (the OS temp dir parent).
        let denied = caps
            .check_path("/etc")
            .expect_err("absolute-outside must be denied");
        assert_eq!(denied.reason_code(), "blocked_path_escapes_workspace_root");
        fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_outside_is_denied() {
        use std::os::unix::fs::symlink;

        let root = make_temp_root("symlink-in");
        let outside = make_temp_root("symlink-out");
        fs::write(outside.join("secret.txt"), b"secret").expect("write outside secret");

        // A symlink that lives inside the workspace but points outside it.
        let link = root.join("escape-link");
        symlink(&outside, &link).expect("create symlink");

        let caps =
            WorkspaceCapabilities::deny_by_default(&root).grant(Capability::FilesystemWorkspace);
        // Reading through the symlink must be denied because canonicalization
        // resolves the link to its real (outside) target.
        let denied = caps
            .check_path("escape-link/secret.txt")
            .expect_err("symlink-to-outside must be denied");
        assert_eq!(denied.reason_code(), "blocked_path_escapes_workspace_root");

        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn git_push_requires_approval_by_default() {
        let decision = PermissionPolicy::default().decide(PermissionScope::GitPush);
        assert!(!decision.allowed);
        assert!(decision.approval_required);
        assert_eq!(
            decision.reason_code,
            "blocked_git_push_requires_outbox_approval"
        );
    }

    #[test]
    fn default_data_plane_manifest_separates_shared_and_local_state() {
        let manifest = default_data_plane_manifest();
        assert_eq!(manifest.schema, DATA_PLANE_MANIFEST_SCHEMA);
        assert!(manifest.shared_paths.iter().any(|rule| {
            rule.path == ".opensks/wiki/records/"
                && rule.plane == DataPlane::SharedDurable
                && rule.git_tracking == GitTrackingPolicy::Track
        }));
        assert!(manifest.local_paths.iter().any(|rule| {
            rule.path == ".opensks/secrets/"
                && rule.plane == DataPlane::SecretLocal
                && rule.git_tracking == GitTrackingPolicy::Ignore
                && rule.contains_secrets
        }));
        assert!(manifest.local_paths.iter().any(|rule| {
            rule.path == ".opensks/workers/"
                && rule.plane == DataPlane::EphemeralLocal
                && rule.git_tracking == GitTrackingPolicy::Ignore
        }));
        assert!(
            manifest
                .shared_paths
                .iter()
                .all(|rule| !rule.allows_raw_provider_responses)
        );
        assert!(
            manifest
                .invariants
                .contains(&"broad_dot_opensks_ignore_is_not_allowed".to_string())
        );
    }
}
