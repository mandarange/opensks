use super::*;

use std::{
    fs,
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

fn temp_workspace(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{label}-{}-{nanos}", process::id()));
    fs::create_dir_all(&dir).expect("create temp workspace");
    dir
}

fn conversation_args(parts: &[&str], workspace: &Path) -> Vec<String> {
    let mut args = vec![
        parts[0].to_string(),
        "--workspace".to_string(),
        workspace.to_string_lossy().into_owned(),
    ];
    args.extend(parts[1..].iter().map(|part| part.to_string()));
    args
}

fn conversation_json(args: &[&str], workspace: &Path) -> serde_json::Value {
    let output = run_conversation_command(&conversation_args(args, workspace), workspace)
        .expect("conversation command");
    serde_json::from_str(&output.stdout).expect("valid conversation json")
}

fn create_conversation_id(workspace: &Path) -> String {
    let created = conversation_json(&["create", "--title", "Provider Dispatch"], workspace);
    created["id"].as_str().expect("conversation id").to_string()
}

#[test]
fn cli_provider_dispatch_parts_keep_provider_and_remote_model_separate() {
    let root = temp_workspace("opensks-cli-provider-model-dispatch");
    let repo =
        opensks_provider::ProviderRepository::open_workspace(&root).expect("provider registry");
    let connection = opensks_contracts::ProviderConnection {
        schema: opensks_contracts::PROVIDER_CONNECTION_SCHEMA.to_string(),
        id: "provider-1".to_string(),
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
            account: "provider-1".to_string(),
            version: 1,
        },
        organization_ref: None,
        project_ref: None,
        health: opensks_contracts::ProviderHealthSnapshot::unknown(),
        concurrency: opensks_contracts::ProviderConcurrencyPolicy {
            max_concurrent_requests: 4,
            requests_per_minute: None,
            tokens_per_minute: None,
        },
        created_at_ms: 1,
        updated_at_ms: 1,
        revision: 1,
    };
    repo.upsert_connection(&connection, None, 1)
        .expect("connection saved");

    let mut role_scores = std::collections::BTreeMap::new();
    role_scores.insert(
        opensks_contracts::ModelRole::Code,
        opensks_contracts::RoleScore {
            score: 0.91,
            evidence_refs: vec!["test-catalog".to_string()],
        },
    );
    let model = opensks_contracts::ModelCatalogEntry {
        schema: opensks_contracts::MODEL_CATALOG_ENTRY_SCHEMA.to_string(),
        id: "provider-1/gpt-5.5".to_string(),
        provider_id: "provider-1".to_string(),
        remote_model_id: "openai/gpt-5.5".to_string(),
        display_name: "GPT-5.5".to_string(),
        enabled: true,
        capabilities: opensks_contracts::ModelCapabilities::text_code(),
        limits: opensks_contracts::ModelLimits {
            max_input_tokens: Some(400_000),
            max_output_tokens: Some(777),
            requests_per_minute: None,
            tokens_per_minute: None,
            max_concurrency: Some(4),
        },
        pricing: None,
        health: opensks_contracts::HealthState::Healthy,
        role_scores,
        catalog_revision: "catalog-rev-1".to_string(),
    };
    repo.sync_models("provider-1", &[model], 2)
        .expect("models synced");

    let (resolved_connection, resolved_model) =
        cli_provider_dispatch_parts_for_model(&repo, "provider-1/gpt-5.5").expect("dispatch parts");
    assert_eq!(resolved_connection.id, "provider-1");
    assert_eq!(
        resolved_connection.kind,
        opensks_contracts::ProviderKind::CodexLb
    );
    assert_eq!(resolved_model.id, "provider-1/gpt-5.5");
    assert_eq!(resolved_model.remote_model_id, "openai/gpt-5.5");
    assert_eq!(max_output_tokens_for_provider_model(&resolved_model), 777);

    fs::remove_dir_all(root).ok();
}

struct CliDispatchTestCompleter;

impl opensks_adapter::ChatCompleter for CliDispatchTestCompleter {
    fn complete(
        &self,
        _body: &serde_json::Value,
    ) -> Result<serde_json::Value, opensks_adapter::AgentAdapterError> {
        Ok(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "ok"
                }
            }]
        }))
    }
}

fn sample_cli_routing_decision() -> opensks_contracts::RoutingDecision {
    opensks_contracts::RoutingDecision {
        schema: opensks_contracts::ROUTING_DECISION_SCHEMA.to_string(),
        id: "route-cli-dispatch".to_string(),
        status: opensks_contracts::RoutingStatus::Resolved,
        selected_model_id: Some("provider-1/gpt-5.5".to_string()),
        reason_code: "explicit_model_resolved".to_string(),
        eligible_model_ids: vec!["provider-1/gpt-5.5".to_string()],
        rejected_models: Vec::new(),
        model_snapshot_hash: "registry-rev-1".to_string(),
        route_receipt: Some(opensks_contracts::ModelRouteReceipt {
            provider_id: Some("provider-1".to_string()),
            model_id: Some("provider-1/gpt-5.5".to_string()),
            registry_revision: "registry-rev-1".to_string(),
            reason_code: "explicit_model_resolved".to_string(),
            requested_capabilities: opensks_contracts::CapabilityRequirements::code(),
            effective_limits: opensks_contracts::ModelLimits::default(),
            fallback_index: None,
        }),
    }
}

#[test]
fn cli_dispatch_recorder_persists_dispatch_ready_and_dispatched_status() {
    let root = temp_workspace("opensks-cli-provider-dispatch-recorder");
    let cid = create_conversation_id(&root);
    let turn = conversation_json(
        &[
            "turn-start",
            "--conversation",
            &cid,
            "--text",
            "{\"local_test\":{\"op\":\"append_line\",\"path\":\"NOTE.md\",\"value\":\"seed\"}}",
        ],
        &root,
    );
    let turn_id = turn["turn_id"].as_str().expect("turn id").to_string();
    let repo = opensks_conversation::ConversationRepository::open_workspace(&root).expect("repo");

    let dispatch_ready = persist_cli_routing_status(
        &repo,
        &turn_id,
        sample_cli_routing_decision(),
        opensks_contracts::RoutingStatus::DispatchReady,
        "provider_dispatch_ready",
        2_000,
    )
    .expect("persist dispatch ready");
    let ready_raw = repo
        .turn_model_routing_decision_json(&turn_id)
        .expect("ready lookup")
        .expect("ready routing decision");
    let ready: opensks_contracts::RoutingDecision =
        serde_json::from_str(&ready_raw).expect("ready json");
    assert_eq!(
        ready.status,
        opensks_contracts::RoutingStatus::DispatchReady
    );
    assert_eq!(ready.reason_code, "provider_dispatch_ready");
    assert_eq!(
        ready
            .route_receipt
            .as_ref()
            .expect("ready receipt")
            .reason_code,
        "provider_dispatch_ready"
    );

    let completer = CliDispatchRecordingCompleter::new(
        CliDispatchTestCompleter,
        root.clone(),
        turn_id.clone(),
        dispatch_ready,
    );
    let response = opensks_adapter::ChatCompleter::complete(
        &completer,
        &serde_json::json!({"model": "openai/gpt-5.5"}),
    )
    .expect("completer response");
    assert_eq!(response["choices"][0]["message"]["content"], "ok");
    let dispatched_raw = repo
        .turn_model_routing_decision_json(&turn_id)
        .expect("dispatched lookup")
        .expect("dispatched routing decision");
    let dispatched: opensks_contracts::RoutingDecision =
        serde_json::from_str(&dispatched_raw).expect("dispatched json");
    assert_eq!(
        dispatched.status,
        opensks_contracts::RoutingStatus::Dispatched
    );
    assert_eq!(dispatched.reason_code, "provider_request_dispatched");
    assert_eq!(
        dispatched
            .route_receipt
            .as_ref()
            .expect("dispatched receipt")
            .reason_code,
        "provider_request_dispatched"
    );

    opensks_adapter::ChatCompleter::complete(
        &completer,
        &serde_json::json!({"model": "openai/gpt-5.5"}),
    )
    .expect("second completer response");
    let second_raw = repo
        .turn_model_routing_decision_json(&turn_id)
        .expect("second lookup")
        .expect("second routing decision");
    assert_eq!(second_raw, dispatched_raw);

    fs::remove_dir_all(root).ok();
}
