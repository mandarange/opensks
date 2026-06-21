use opensks_contracts::{
    RELEASE_PROOF_SCHEMA, RETENTION_PLAN_SCHEMA, ReleaseProof, RetentionPlan, TrustStatus,
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
    let status = if signed_app
        && notarized
        && fresh_install_checked
        && fresh_clone_checked
        && upgrade_checked
    {
        TrustStatus::Verified
    } else {
        TrustStatus::NotVerified
    };
    ReleaseProof {
        schema: RELEASE_PROOF_SCHEMA.to_string(),
        version: version.into(),
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
    }
}
