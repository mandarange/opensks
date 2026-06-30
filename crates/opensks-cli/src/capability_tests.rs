use super::run_capability_command;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn capability_report_emits_valid_json_and_matrix() {
    let cwd = std::env::temp_dir();
    let out = run_capability_command(&["report".to_string()], &cwd).expect("report");
    let report: opensks_contracts::RuntimeCapabilityReport =
        serde_json::from_str(&out.stdout).expect("valid json capability report");
    report.validate().expect("report internally valid");
    assert!(
        report.generated_for.as_deref().is_some_and(|value| {
            value.contains("workspace:") && value.contains("fixture:local")
        }),
        "runtime report must identify the current workspace/build fixture"
    );
    assert!(
        report
            .capabilities
            .iter()
            .any(|c| c.id == "agent.local_test_edit"),
        "report must include known capabilities"
    );
    assert!(
        report
            .tool_registry
            .descriptor("mcp.invoke")
            .is_some_and(|tool| {
                tool.availability == opensks_contracts::ToolAvailability::Available
                    && tool.reason_code == "local_mcp_broker_executable"
            }),
        "runtime report must expose available MCP tool truth"
    );
    assert!(
        report
            .tool_registry
            .descriptor("image.generate")
            .is_some_and(|tool| {
                tool.availability == opensks_contracts::ToolAvailability::Unavailable
                    && tool.reason_code == "provider_image_route_unavailable"
            }),
        "runtime report must disable image generation until an image route is enabled"
    );
    assert!(
        report
            .tool_registry
            .descriptor("image.inspect")
            .is_some_and(|tool| {
                tool.availability == opensks_contracts::ToolAvailability::Unavailable
                    && tool.reason_code == "provider_vision_route_unavailable"
            }),
        "runtime report must disable image inspection until a vision route is enabled"
    );
    let local_test = report
        .capabilities
        .iter()
        .find(|c| c.id == "agent.local_test_edit")
        .expect("agent.local_test_edit");
    if cfg!(feature = "simulation") {
        assert!(local_test.available);
        assert_eq!(
            local_test.reason_code,
            "explicit_local_test_adapter_real_file_io"
        );
    } else {
        assert!(!local_test.available);
        assert_eq!(
            local_test.reason_code,
            "simulation_feature_disabled_for_release_build"
        );
    }
    let code_edit = report
        .capabilities
        .iter()
        .find(|c| c.id == "agent.code_edit")
        .expect("agent.code_edit");
    assert_eq!(
        code_edit.reason_code,
        "agentic_loop_toolgateway_patch_engine_need_live_provider_credentials"
    );
    assert!(
        code_edit
            .evidence_refs
            .iter()
            .any(|e| e == "toolgateway:policy-enforced"),
        "agent.code_edit evidence must come from runtime ToolGateway state"
    );
    let image_generate = report
        .capabilities
        .iter()
        .find(|c| c.id == "image.generate")
        .expect("image.generate");
    assert_eq!(
        image_generate.maturity,
        opensks_contracts::CapabilityMaturity::Foundation
    );
    assert!(!image_generate.available);
    assert_eq!(
        image_generate.reason_code,
        "provider_image_lane_present_needs_enabled_image_route"
    );
    let stream = report
        .capabilities
        .iter()
        .find(|c| c.id == "stream.protocol")
        .expect("stream.protocol");
    assert_eq!(stream.maturity, opensks_contracts::CapabilityMaturity::Live);
    assert!(stream.available);
    assert_eq!(
        stream.reason_code,
        "daemon_stream_protocol_v2_explicit_terminal_frames"
    );
    assert!(
        !stream.reason_code.contains("quiet_window"),
        "runtime capability truth must not preserve stale quiet-window reason"
    );
    assert!(
        !stream.reason_code.contains("missing"),
        "runtime capability truth must not claim stream v2 is missing after v2 frames landed"
    );
    assert!(
        stream
            .evidence_refs
            .iter()
            .any(|e| e == "schema:engine-stream-frame"),
        "runtime capability truth must cite the v2 stream frame schema"
    );
    let matrix = run_capability_command(&["matrix".to_string()], &cwd).expect("matrix");
    assert!(matrix.stdout.contains("Runtime Truth Matrix"));
    assert!(matrix.stdout.contains("runtime capability report"));
    assert!(matrix.stdout.contains("Tool Registry"));
    assert!(matrix.stdout.contains("| `skill.invoke` |"));
    assert!(run_capability_command(&["nope".to_string()], &cwd).is_err());
}

#[test]
fn capability_report_does_not_materialize_missing_provider_registry() {
    let root = temp_workspace("capability-no-provider-db");
    let provider_db = root.join(opensks_provider::PROVIDER_DB_RELATIVE_PATH);
    assert!(!provider_db.exists());

    let out = run_capability_command(&["report".to_string()], &root).expect("report");
    let report: opensks_contracts::RuntimeCapabilityReport =
        serde_json::from_str(&out.stdout).expect("valid json capability report");

    assert!(
        !provider_db.exists(),
        "capability report must inspect existing provider registry state without creating a DB"
    );
    let model_dispatch = report
        .capabilities
        .iter()
        .find(|cap| cap.id == "model.dispatch")
        .expect("model.dispatch");
    assert!(
        model_dispatch
            .evidence_refs
            .iter()
            .any(|item| item == "provider-registry:not-materialized")
    );
    fs::remove_dir_all(root).ok();
}

#[test]
fn capability_report_prefers_provider_registry_code_route_truth() {
    let root = temp_workspace("capability-provider-registry-route");
    let repo = opensks_provider::ProviderRepository::open_workspace(&root).expect("provider repo");
    let connection = sample_connection();
    repo.upsert_connection(&connection, None, 10)
        .expect("connection saved");
    repo.sync_models(&connection.id, &[sample_code_model(&connection.id)], 20)
        .expect("models synced");

    let out = run_capability_command(&["report".to_string()], &root).expect("report");
    let report: opensks_contracts::RuntimeCapabilityReport =
        serde_json::from_str(&out.stdout).expect("valid json capability report");

    let chat = report
        .capabilities
        .iter()
        .find(|cap| cap.id == "chat.answer")
        .expect("chat.answer");
    assert_eq!(
        chat.reason_code,
        "provider_registry_code_route_present_live_chat_probe_required"
    );
    assert!(
        chat.evidence_refs
            .iter()
            .any(|item| item == "provider-registry:enabled-code-model")
    );
    assert!(
        chat.evidence_refs
            .iter()
            .any(|item| item == "adapter:openai-compatible-native-http")
    );
    assert!(
        !chat
            .evidence_refs
            .iter()
            .any(|item| item == "adapter:openrouter-native-http"),
        "codex-lb registry route must not be reported as OpenRouter-only evidence"
    );
    assert!(
        chat.actions
            .iter()
            .any(|item| item == "run_provider_adapter_check")
    );

    let dispatch = report
        .capabilities
        .iter()
        .find(|cap| cap.id == "model.dispatch")
        .expect("model.dispatch");
    assert_eq!(
        dispatch.reason_code,
        "provider_registry_code_route_present_dispatch_probe_required"
    );
    assert!(
        dispatch
            .evidence_refs
            .iter()
            .any(|item| item == "provider-registry:secret-ref-only")
    );
    assert!(
        dispatch
            .evidence_refs
            .iter()
            .any(|item| item == "provider:openai-compatible-native-reqwest")
    );
    assert!(
        !dispatch
            .evidence_refs
            .iter()
            .any(|item| item == "provider:openrouter-native-reqwest"),
        "codex-lb registry route must not be reported as OpenRouter-only transport"
    );

    let code_edit = report
        .capabilities
        .iter()
        .find(|cap| cap.id == "agent.code_edit")
        .expect("agent.code_edit");
    assert_eq!(
        code_edit.reason_code,
        "agentic_loop_provider_registry_code_route_present_live_dispatch_unverified"
    );
    assert!(code_edit.actions.iter().any(|item| item == "connect_model"));
    assert!(
        code_edit
            .actions
            .iter()
            .any(|item| item == "review_patch_policy")
    );
    assert!(
        code_edit
            .evidence_refs
            .iter()
            .any(|item| item == "driver:openai-compatible-tools")
    );
    assert!(
        !code_edit
            .evidence_refs
            .iter()
            .any(|item| item == "driver:openrouter-tools"),
        "codex-lb registry route must not be reported as OpenRouter-only tool driver"
    );

    assert!(!out.stdout.contains("sk-"));
    assert!(!out.stdout.contains("registry-secret-value"));
    fs::remove_dir_all(root).ok();
}

#[test]
fn capability_report_uses_sealed_provider_backed_coding_execution_proof() {
    let root = temp_workspace("capability-live-coding-proof");
    write_live_coding_proof(&root, "turn-live-proof", true);

    let out = run_capability_command(&["report".to_string()], &root).expect("report");
    let report: opensks_contracts::RuntimeCapabilityReport =
        serde_json::from_str(&out.stdout).expect("valid json capability report");

    let dispatch = capability(&report, "model.dispatch");
    assert!(dispatch.available);
    assert_eq!(
        dispatch.maturity,
        opensks_contracts::CapabilityMaturity::Live
    );
    assert_eq!(dispatch.reason_code, "provider_model_dispatch_observed");
    assert!(
        dispatch
            .evidence_refs
            .iter()
            .any(|item| item == "provider:provider-codex-lb-env")
    );
    assert!(
        dispatch
            .evidence_refs
            .iter()
            .any(|item| item == "model:provider-codex-lb-env/gpt-5.4-nano")
    );

    let code_edit = capability(&report, "agent.code_edit");
    assert!(code_edit.available);
    assert_eq!(
        code_edit.maturity,
        opensks_contracts::CapabilityMaturity::Live
    );
    assert_eq!(
        code_edit.reason_code,
        "provider_backed_code_edit_integrated"
    );
    assert!(
        code_edit
            .evidence_refs
            .iter()
            .any(|item| item == "integration:main-workspace-apply-completed")
    );

    let parallel = capability(&report, "agent.parallel_build");
    assert!(parallel.available);
    assert_eq!(
        parallel.maturity,
        opensks_contracts::CapabilityMaturity::Degraded
    );
    assert_eq!(
        parallel.reason_code,
        "provider_backed_parallel_integration_observed"
    );
    assert!(
        parallel
            .evidence_refs
            .iter()
            .any(|item| item == "integration:aggregate-candidate-count-2")
    );

    assert!(
        code_edit
            .evidence_refs
            .iter()
            .any(|item| item.ends_with("/seal.json"))
    );
    assert!(
        !out.stdout.contains("sk-"),
        "capability report must cite redacted artifacts, not provider secrets"
    );
    fs::remove_dir_all(root).ok();
}

#[test]
fn capability_report_does_not_trust_unsealed_coding_candidate() {
    let root = temp_workspace("capability-unsealed-coding-proof");
    write_live_coding_proof(&root, "turn-unsealed-proof", false);

    let out = run_capability_command(&["report".to_string()], &root).expect("report");
    let report: opensks_contracts::RuntimeCapabilityReport =
        serde_json::from_str(&out.stdout).expect("valid json capability report");

    let dispatch = capability(&report, "model.dispatch");
    assert!(!dispatch.available);
    assert!(
        matches!(
            dispatch.reason_code.as_str(),
            "openrouter_secret_missing" | "openrouter_secret_present_runtime_probe_required"
        ),
        "an unsealed candidate must not become dispatch proof; got reason_code={}",
        dispatch.reason_code
    );
    let code_edit = capability(&report, "agent.code_edit");
    assert!(!code_edit.available);
    assert_eq!(
        code_edit.reason_code,
        "agentic_loop_toolgateway_patch_engine_need_live_provider_credentials"
    );
    let parallel = capability(&report, "agent.parallel_build");
    assert!(!parallel.available);
    fs::remove_dir_all(root).ok();
}

#[test]
fn capability_report_does_not_trust_mismatched_coding_proof_refs() {
    let root = temp_workspace("capability-mismatched-coding-proof");
    let turn_dir_name = "turn-mismatched-proof";
    write_live_coding_proof(&root, turn_dir_name, true);
    let seal_path = root
        .join(".opensks/runtime/integration-candidates")
        .join(turn_dir_name)
        .join("seal.json");
    let mut seal: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&seal_path).expect("seal json"))
            .expect("seal value");
    seal["candidate_ref"] = serde_json::json!(
        "artifact://.opensks/runtime/integration-candidates/turn-other/candidate.json"
    );
    fs::write(
        &seal_path,
        serde_json::to_vec_pretty(&seal).expect("mutated seal"),
    )
    .expect("write mutated seal");

    let out = run_capability_command(&["report".to_string()], &root).expect("report");
    let report: opensks_contracts::RuntimeCapabilityReport =
        serde_json::from_str(&out.stdout).expect("valid json capability report");

    assert!(!capability(&report, "model.dispatch").available);
    assert!(!capability(&report, "agent.code_edit").available);
    assert!(!capability(&report, "agent.parallel_build").available);
    fs::remove_dir_all(root).ok();
}

fn sample_connection() -> opensks_contracts::ProviderConnection {
    opensks_contracts::ProviderConnection {
        schema: opensks_contracts::PROVIDER_CONNECTION_SCHEMA.to_string(),
        id: "provider-capability".to_string(),
        kind: opensks_contracts::ProviderKind::CodexLb,
        display_name: "codex-lb".to_string(),
        enabled: true,
        endpoint: opensks_contracts::ProviderEndpoint {
            base_url: "https://codex.hyper-lab.xyz/backend-api/codex".to_string(),
            allow_insecure_http: false,
        },
        auth: opensks_contracts::SecretRef {
            schema: opensks_contracts::SECRET_REF_SCHEMA.to_string(),
            store: opensks_contracts::SecretStoreKind::MacosKeychain,
            service: "ai.opensks.provider.codex_lb".to_string(),
            account: "provider-capability".to_string(),
            version: 1,
        },
        organization_ref: None,
        project_ref: None,
        health: opensks_contracts::ProviderHealthSnapshot::unknown(),
        concurrency: opensks_contracts::ProviderConcurrencyPolicy {
            max_concurrent_requests: 4,
            requests_per_minute: Some(60),
            tokens_per_minute: None,
        },
        created_at_ms: 1,
        updated_at_ms: 1,
        revision: 1,
    }
}

fn sample_code_model(provider_id: &str) -> opensks_contracts::ModelCatalogEntry {
    let mut role_scores = BTreeMap::new();
    role_scores.insert(
        opensks_contracts::ModelRole::Code,
        opensks_contracts::RoleScore {
            score: 0.9,
            evidence_refs: vec!["test-catalog".to_string()],
        },
    );
    opensks_contracts::ModelCatalogEntry {
        schema: opensks_contracts::MODEL_CATALOG_ENTRY_SCHEMA.to_string(),
        id: format!("{provider_id}/gpt-5.5"),
        provider_id: provider_id.to_string(),
        remote_model_id: "openai/gpt-5.5".to_string(),
        display_name: "GPT-5.5".to_string(),
        enabled: true,
        capabilities: opensks_contracts::ModelCapabilities::text_code(),
        limits: opensks_contracts::ModelLimits {
            max_input_tokens: Some(400_000),
            max_output_tokens: Some(16_000),
            requests_per_minute: None,
            tokens_per_minute: None,
            max_concurrency: Some(4),
        },
        pricing: None,
        health: opensks_contracts::HealthState::Healthy,
        role_scores,
        catalog_revision: "catalog-rev-1".to_string(),
    }
}

fn capability<'a>(
    report: &'a opensks_contracts::RuntimeCapabilityReport,
    id: &str,
) -> &'a opensks_contracts::RuntimeCapability {
    report
        .capabilities
        .iter()
        .find(|cap| cap.id == id)
        .unwrap_or_else(|| panic!("{id} capability"))
}

fn write_live_coding_proof(root: &Path, turn_dir_name: &str, sealed: bool) {
    let integration_dir = root
        .join(".opensks/runtime/integration-candidates")
        .join(turn_dir_name);
    let semantic_dir = root
        .join(".opensks/runtime/semantic-verifiers")
        .join(turn_dir_name)
        .join("turn-role-live-proof-2-verification");
    fs::create_dir_all(&integration_dir).expect("integration dir");
    fs::create_dir_all(&semantic_dir).expect("semantic dir");

    let run_id = turn_dir_name;
    let candidate_id = format!("integration-candidate-{run_id}");
    let candidate_ref =
        format!("artifact://.opensks/runtime/integration-candidates/{run_id}/candidate.json");
    let verification_ref =
        format!("artifact://.opensks/runtime/integration-candidates/{run_id}/verification.json");
    let integration_ref =
        format!("artifact://.opensks/runtime/integration-candidates/{run_id}/integration.json");
    let seal_ref = format!("artifact://.opensks/runtime/integration-candidates/{run_id}/seal.json");
    fs::write(
        integration_dir.join("candidate.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "opensks.integration-candidate.v1",
            "id": candidate_id.clone(),
            "run_id": run_id,
            "turn_id": run_id.strip_prefix("turn-").unwrap_or(run_id),
            "state": "candidate_ready",
            "reason_code": "aggregate_isolated_patch_candidate_ready",
            "source_isolation_mode": "git_worktree",
            "source_candidates": [
                {
                    "source": "turn_supervisor",
                    "role": "coordination",
                    "source_isolation_mode": "git_worktree"
                },
                {
                    "source": "role_subcontract",
                    "role": "code",
                    "source_isolation_mode": "git_worktree"
                }
            ],
            "aggregate_candidate_count": 2,
            "patch_count": 2,
            "apply_result_count": 2,
            "turn_settings": {
                "execution_mode": "worktree",
                "max_parallelism": 16
            },
            "evidence_refs": [
                "daemon:role-worker-code-candidate",
                "integration:role-candidate-aggregate",
                "integration:aggregate-candidate-ready"
            ],
            "generated_at_ms": 1000
        }))
        .expect("candidate json"),
    )
    .expect("write candidate");

    fs::write(
        semantic_dir.join("judgment.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "opensks.semantic-verifier-judgment.v1",
            "run_id": run_id,
            "state": "judgment_ready",
            "reason_code": "model_semantic_verifier_judgment_recorded",
            "provider_id": "provider-codex-lb-env",
            "model_id": "provider-codex-lb-env/gpt-5.4-nano",
            "evidence_refs": [
                "daemon:semantic-verifier-judgment",
                "daemon:role-worker-model-call",
                "provider:role-routing",
                "scheduler:role-plan-work-item"
            ]
        }))
        .expect("semantic json"),
    )
    .expect("write semantic judgment");

    if !sealed {
        return;
    }

    fs::write(
        integration_dir.join("verification.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "opensks.integration-verification-receipt.v1",
            "run_id": run_id,
            "candidate_id": candidate_id.clone(),
            "state": "passed",
            "reason_code": "candidate_verification_passed",
            "passed_gates": [
                "candidate_receipt_valid",
                "git_apply_check_passed",
                "read_only_verifier_lanes_passed"
            ],
            "failed_gates": [],
            "candidate_ref": candidate_ref.clone(),
            "verification_ref": verification_ref.clone(),
            "generated_at_ms": 1100
        }))
        .expect("verification json"),
    )
    .expect("write verification");
    fs::write(
        integration_dir.join("integration.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "opensks.integration-apply-receipt.v1",
            "run_id": run_id,
            "candidate_id": candidate_id.clone(),
            "state": "integrated",
            "reason_code": "candidate_applied_to_main_workspace",
            "main_workspace_modified": true,
            "verifier_passed": true,
            "candidate_ref": candidate_ref.clone(),
            "verification_ref": verification_ref.clone(),
            "integration_ref": integration_ref.clone(),
            "generated_at_ms": 1200
        }))
        .expect("integration json"),
    )
    .expect("write integration");
    fs::write(
        integration_dir.join("seal.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "opensks.integration-final-seal.v1",
            "run_id": run_id,
            "candidate_id": candidate_id.clone(),
            "state": "sealed",
            "reason_code": "integration_final_sealed",
            "passed_gates": [
                "candidate_receipt_valid",
                "verification_receipt_passed",
                "main_workspace_apply_completed",
                "final_diff_captured"
            ],
            "failed_gates": [],
            "candidate_ref": candidate_ref,
            "verification_ref": verification_ref,
            "integration_ref": integration_ref,
            "seal_ref": seal_ref,
            "generated_at_ms": 1300
        }))
        .expect("seal json"),
    )
    .expect("write seal");
}

fn temp_workspace(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{label}-{}-{nanos}", process::id()));
    fs::create_dir_all(&dir).expect("create temp workspace");
    dir
}
