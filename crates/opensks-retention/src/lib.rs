use opensks_contracts::{
    RELEASE_PROOF_SCHEMA, RETENTION_PLAN_SCHEMA, ReleaseArtifactDigest, ReleaseProof,
    ReleaseProofBlocker, RetentionPlan, TrustStatus,
};

pub fn plan_gc(paths: &[String], active_run_id: &str) -> RetentionPlan {
    let mut delete_paths = Vec::new();
    let mut keep_paths = Vec::new();
    let mut blocked_paths = Vec::new();
    for path in paths {
        if path.contains(active_run_id) {
            blocked_paths.push(path.clone());
        } else if path.contains("/runtime/") || path.contains("/tmp/") || path.contains("/logs/") {
            delete_paths.push(path.clone());
        } else {
            keep_paths.push(path.clone());
        }
    }
    RetentionPlan {
        schema: RETENTION_PLAN_SCHEMA.to_string(),
        delete_paths,
        keep_paths,
        blocked_paths,
        active_run_protected: true,
    }
}

pub fn release_proof(
    version: impl Into<String>,
    signed_app: bool,
    notarized: bool,
    fresh_install_checked: bool,
    fresh_clone_checked: bool,
    upgrade_checked: bool,
) -> ReleaseProof {
    release_proof_with_artifacts(
        version,
        signed_app,
        notarized,
        fresh_install_checked,
        fresh_clone_checked,
        upgrade_checked,
        None,
        false,
        Vec::new(),
        vec![ReleaseProofBlocker {
            code: "artifact_digest_gate_missing".to_string(),
            message: "release proof was created without artifact digest evidence".to_string(),
        }],
    )
}

#[allow(clippy::too_many_arguments)]
pub fn release_proof_with_artifacts(
    version: impl Into<String>,
    signed_app: bool,
    notarized: bool,
    fresh_install_checked: bool,
    fresh_clone_checked: bool,
    upgrade_checked: bool,
    source_commit_sha: Option<String>,
    workspace_dirty: bool,
    artifact_digests: Vec<ReleaseArtifactDigest>,
    mut blockers: Vec<ReleaseProofBlocker>,
) -> ReleaseProof {
    let missing_artifacts: Vec<String> = artifact_digests
        .iter()
        .filter(|artifact| artifact.required && !artifact.present)
        .map(|artifact| artifact.path.clone())
        .collect();
    let required_artifacts: Vec<&ReleaseArtifactDigest> = artifact_digests
        .iter()
        .filter(|artifact| artifact.required)
        .collect();
    let required_artifacts_complete = !required_artifacts.is_empty()
        && required_artifacts
            .iter()
            .all(|artifact| artifact.present && artifact.digest.is_some());
    let same_sha_artifact_binding = source_commit_sha.as_ref().is_some_and(|commit| {
        required_artifacts_complete
            && !workspace_dirty
            && required_artifacts.iter().all(|artifact| {
                artifact
                    .source_commit_sha
                    .as_ref()
                    .is_some_and(|artifact_commit| artifact_commit == commit)
            })
    });
    if source_commit_sha.is_none() {
        blockers.push(ReleaseProofBlocker {
            code: "source_commit_unavailable".to_string(),
            message: "release proof could not bind artifacts to a git HEAD commit".to_string(),
        });
    }
    if workspace_dirty {
        blockers.push(ReleaseProofBlocker {
            code: "workspace_dirty".to_string(),
            message: "tracked workspace changes prevent same-SHA release artifact binding"
                .to_string(),
        });
    }
    for path in &missing_artifacts {
        blockers.push(ReleaseProofBlocker {
            code: "missing_required_artifact".to_string(),
            message: format!("required release artifact is missing: {path}"),
        });
    }
    if required_artifacts.is_empty() {
        blockers.push(ReleaseProofBlocker {
            code: "no_required_artifacts".to_string(),
            message: "release proof did not name any required artifacts".to_string(),
        });
    }
    let artifact_digest_gate_passed =
        required_artifacts_complete && same_sha_artifact_binding && blockers.is_empty();
    let status = if signed_app
        && notarized
        && fresh_install_checked
        && fresh_clone_checked
        && upgrade_checked
        && artifact_digest_gate_passed
    {
        TrustStatus::Verified
    } else {
        TrustStatus::NotVerified
    };
    ReleaseProof {
        schema: RELEASE_PROOF_SCHEMA.to_string(),
        version: version.into(),
        source_commit_sha,
        workspace_dirty,
        artifact_digests,
        missing_artifacts,
        same_sha_artifact_binding,
        artifact_digest_gate_passed,
        blockers,
        signed_app,
        notarized,
        rollback_plan_ref: ".opensks/updater/rollback-plan.json".to_string(),
        fresh_install_checked,
        fresh_clone_checked,
        upgrade_checked,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gc_plan_keeps_active_run_and_shared_records() {
        let plan = plan_gc(
            &[
                ".opensks/runtime/worktrees/run-active/worker".to_string(),
                ".opensks/runtime/worktrees/run-old/worker".to_string(),
                ".opensks/wiki/records/ar.jsonl".to_string(),
            ],
            "run-active",
        );
        assert!(
            plan.blocked_paths
                .iter()
                .any(|path| path.contains("run-active"))
        );
        assert!(
            plan.delete_paths
                .iter()
                .any(|path| path.contains("run-old"))
        );
        assert!(
            plan.keep_paths
                .iter()
                .any(|path| path.contains("wiki/records"))
        );
    }

    #[test]
    fn unsigned_release_proof_is_not_verified() {
        let proof = release_proof("0.1.0", false, false, true, true, true);
        assert_eq!(proof.status, TrustStatus::NotVerified);
        assert!(!proof.artifact_digest_gate_passed);
        assert!(
            proof
                .blockers
                .iter()
                .any(|blocker| blocker.code == "artifact_digest_gate_missing")
        );
    }
}
