use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use opensks_contracts::{
    EngineRequest, GitCommit, GitCommitPreview, IntegrationApplyReceipt, PushApproval, PushIntent,
    PushReceipt, PushStatus,
};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(name: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut dir = std::env::temp_dir();
    dir.push(format!("opensks-pr099-{name}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    dir.canonicalize().expect("canonicalize temp dir")
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed in {dir:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_workspace_with_local_bare_remote(label: &str, branch: &str) -> (PathBuf, PathBuf) {
    let root = temp_dir(label);
    let workspace = root.join("source");
    let bare = root.join("remote.git");
    fs::create_dir_all(&workspace).expect("workspace dir");
    run_git(&workspace, &["init"]);
    run_git(
        &workspace,
        &["config", "user.email", "opensks@example.test"],
    );
    run_git(&workspace, &["config", "user.name", "OpenSKS Test"]);
    run_git(&workspace, &["config", "commit.gpgsign", "false"]);
    run_git(&workspace, &["checkout", "-B", branch]);
    fs::write(workspace.join("NOTE.md"), "before\n").expect("seed note");
    run_git(&workspace, &["add", "NOTE.md"]);
    run_git(&workspace, &["commit", "-m", "initial"]);
    run_git(&workspace, &["init", "--bare", bare.to_str().unwrap()]);
    run_git(
        &workspace,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    (workspace, bare)
}

fn write_candidate_fixture(workspace: &Path, run_id: &str, target: &str, patch: &str) {
    let candidate_dir = workspace
        .join(".opensks")
        .join("runtime")
        .join("integration-candidates")
        .join(run_id);
    fs::create_dir_all(&candidate_dir).expect("candidate dir");
    let head = run_git(workspace, &["rev-parse", "HEAD"]);
    let candidate = serde_json::json!({
        "schema": "opensks.integration-candidate.v1",
        "id": format!("integration-candidate-{run_id}"),
        "run_id": run_id,
        "turn_id": "turn-pr099-e2e",
        "conversation_id": "conversation-pr099-e2e",
        "project_id": "project-pr099-e2e",
        "worker_id": "turn-supervisor",
        "state": "candidate_ready",
        "reason_code": "isolated_patch_candidate_ready",
        "source_isolation_id": format!("isolation-{run_id}-turn-supervisor"),
        "source_isolation_mode": "git_worktree",
        "source_base_commit": head,
        "source_git_available": true,
        "planned_verifier_count": 1,
        "target_paths": [target],
        "patch_count": 1,
        "apply_result_count": 1,
        "applied_files": [target],
        "receipt_ref": format!("artifact://.opensks/runtime/integration-candidates/{run_id}/candidate.json"),
        "patch_ref": format!("artifact://.opensks/runtime/integration-candidates/{run_id}/candidate.patch"),
        "main_workspace_modified": false,
        "integration_required": true,
        "approval_required": true,
        "path_redacted": true,
        "content_redacted": true,
        "generated_at_ms": 1_000,
        "evidence_refs": [
            "git:isolation-prepared",
            "patch-engine:atomic-apply",
            "integration:candidate-ready"
        ]
    });
    fs::write(
        candidate_dir.join("candidate.json"),
        serde_json::to_string_pretty(&candidate).expect("candidate json"),
    )
    .expect("write candidate");
    fs::write(candidate_dir.join("candidate.patch"), patch).expect("write patch");
}

fn integration_approval_request(run_id: &str) -> EngineRequest {
    let approval_id = format!("approval-integration-{run_id}");
    let mut request = EngineRequest::approval_decision(
        format!("req-approve-{run_id}"),
        run_id,
        approval_id,
        true,
    );
    request.params.scope = Some("integration_apply".to_string());
    request
}

fn integration_apply_receipts(output: &str) -> Vec<IntegrationApplyReceipt> {
    output
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| {
            value.get("schema").and_then(serde_json::Value::as_str)
                == Some(opensks_contracts::INTEGRATION_APPLY_RECEIPT_SCHEMA)
        })
        .map(|value| serde_json::from_value(value).expect("integration apply receipt"))
        .collect()
}

fn git_cli_json(workspace: &Path, args: &[&str]) -> serde_json::Value {
    let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    let output = opensks_cli::run_git_command(&args, workspace).expect("git cli command");
    serde_json::from_str(&output.stdout).expect("decode git cli json")
}

fn bare_ref_oid(bare: &Path, branch: &str) -> Option<String> {
    let out = run_git(bare, &["for-each-ref", &format!("refs/heads/{branch}")]);
    if out.is_empty() {
        return None;
    }
    out.split_whitespace().next().map(str::to_string)
}

#[test]
fn integration_apply_stage_commit_push_receipts_are_linked_end_to_end() {
    let branch = "feature/pr099-e2e";
    let run_id = "run-pr099-receipt-e2e";
    let (workspace, bare) = init_workspace_with_local_bare_remote("receipt-e2e", branch);
    let workspace_arg = workspace.to_string_lossy().into_owned();
    let patch = "\
diff --git a/NOTE.md b/NOTE.md
--- a/NOTE.md
+++ b/NOTE.md
@@ -1 +1 @@
-before
+after
";
    write_candidate_fixture(&workspace, run_id, "NOTE.md", patch);

    let approval = integration_approval_request(run_id);
    let request = EngineRequest::integration_candidate_apply(
        "req-pr099-integration-apply",
        run_id,
        format!("approval-integration-{run_id}"),
    );
    let daemon_input = format!(
        "{}\n{}\n",
        serde_json::to_string(&approval).expect("approval json"),
        serde_json::to_string(&request).expect("request json")
    );
    let daemon_output = opensks_daemon::run_stdio(
        &daemon_input,
        &opensks_daemon::DaemonOptions {
            workspace: workspace.clone(),
        },
    )
    .expect("daemon integration apply");
    let integration_receipts = integration_apply_receipts(&daemon_output);
    assert_eq!(integration_receipts.len(), 1);
    let integration = &integration_receipts[0];
    assert_eq!(integration.state, "integrated");
    assert_eq!(
        integration.reason_code,
        "candidate_applied_to_main_workspace"
    );
    assert_eq!(integration.target_paths, vec!["NOTE.md".to_string()]);
    assert_eq!(
        fs::read_to_string(workspace.join("NOTE.md")).expect("note"),
        "after\n"
    );

    let _: serde_json::Value = git_cli_json(
        &workspace,
        &["stage", "--workspace", &workspace_arg, "--path", "NOTE.md"],
    );
    let preview: GitCommitPreview = serde_json::from_value(git_cli_json(
        &workspace,
        &["commit-preview", "--workspace", &workspace_arg],
    ))
    .expect("commit preview");
    assert!(preview.has_staged);
    assert_eq!(preview.staged_paths, vec!["NOTE.md".to_string()]);
    assert_eq!(
        preview.integration_final_diff_ref.as_deref(),
        Some(integration.final_diff_ref.as_str())
    );
    assert_eq!(preview.integration_run_id.as_deref(), Some(run_id));
    assert_eq!(
        preview.integration_candidate_id.as_deref(),
        Some(integration.candidate_id.as_str())
    );
    assert_eq!(
        preview.integration_final_diff_hash, preview.staged_diff_hash,
        "the reviewed staged diff must be the integration final.diff"
    );

    let commit: GitCommit = serde_json::from_value(git_cli_json(
        &workspace,
        &[
            "commit",
            "--workspace",
            &workspace_arg,
            "--message",
            "Integrate PR-099 candidate",
            "--expected-index-hash",
            &preview.index_hash,
        ],
    ))
    .expect("commit receipt");
    assert!(commit.committed);
    assert_eq!(commit.paths, vec!["NOTE.md".to_string()]);
    assert_eq!(
        commit.integration_final_diff_ref,
        preview.integration_final_diff_ref
    );
    assert_eq!(commit.integration_run_id.as_deref(), Some(run_id));
    assert_eq!(
        commit.integration_candidate_id.as_deref(),
        Some(integration.candidate_id.as_str())
    );
    assert_eq!(
        commit.reviewed_staged_diff_hash, preview.staged_diff_hash,
        "the commit receipt must echo the reviewed staged diff"
    );

    let intent: PushIntent = serde_json::from_value(git_cli_json(
        &workspace,
        &[
            "push-enqueue",
            "--workspace",
            &workspace_arg,
            "--remote",
            "origin",
            "--ref",
            branch,
            "--intent",
            "intent-pr099-e2e",
        ],
    ))
    .expect("push intent");
    assert_eq!(intent.local_oid, commit.commit);
    assert_eq!(intent.remote_expected_oid, None);
    assert!(!intent.protected);

    let approval: PushApproval = serde_json::from_value(git_cli_json(
        &workspace,
        &[
            "push-approve",
            "--workspace",
            &workspace_arg,
            "--intent",
            &intent.intent_id,
            "--effect-digest",
            &intent.effect_digest,
        ],
    ))
    .expect("push approval");
    assert!(approval.matched);
    assert_eq!(approval.intent_id, intent.intent_id);

    let push: PushReceipt = serde_json::from_value(git_cli_json(
        &workspace,
        &[
            "push-execute",
            "--workspace",
            &workspace_arg,
            "--intent",
            &intent.intent_id,
        ],
    ))
    .expect("push receipt");
    assert!(push.pushed);
    assert!(!push.already_done);
    assert_eq!(push.remote_oid, commit.commit);
    assert_eq!(
        bare_ref_oid(&bare, branch).as_deref(),
        Some(commit.commit.as_str())
    );

    let status: PushStatus = serde_json::from_value(git_cli_json(
        &workspace,
        &["push-status", "--workspace", &workspace_arg],
    ))
    .expect("push status");
    assert!(status.pending.is_empty());
    assert!(status.approved.is_empty());
    assert!(status.failures.is_empty());
    assert_eq!(status.completed.len(), 1);
    assert_eq!(status.completed[0].remote_oid, commit.commit);

    let repeat: PushReceipt = serde_json::from_value(git_cli_json(
        &workspace,
        &[
            "push-execute",
            "--workspace",
            &workspace_arg,
            "--intent",
            &intent.intent_id,
        ],
    ))
    .expect("repeat push receipt");
    assert!(repeat.already_done);
    assert_eq!(repeat.remote_oid, commit.commit);

    fs::remove_dir_all(workspace.parent().unwrap()).ok();
}
