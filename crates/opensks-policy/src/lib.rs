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
