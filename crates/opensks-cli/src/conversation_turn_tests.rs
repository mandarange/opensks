use super::*;
use std::fs;
use std::path::{Path, PathBuf};

fn temp_workspace(label: &str) -> PathBuf {
    let stamp = ClockStamp::now().expect("clock").compact_id();
    let root = std::env::temp_dir().join(format!("{label}-{stamp}"));
    fs::create_dir_all(&root).expect("temp workspace");
    root
}

fn conversation_json(args: &[&str], workspace: &Path) -> serde_json::Value {
    let ws = workspace.to_string_lossy().into_owned();
    let mut owned = vec![args[0].to_string(), "--workspace".to_string(), ws];
    owned.extend(args[1..].iter().map(|value| value.to_string()));
    let output = run_conversation_command(&owned, workspace).expect("conversation command");
    serde_json::from_str(&output.stdout).expect("valid conversation json")
}

fn create_conversation_id(workspace: &Path) -> String {
    let created = conversation_json(&["create", "--title", "Turn Slice"], workspace);
    created["id"].as_str().expect("conversation id").to_string()
}

#[test]
fn turn_start_persists_user_and_assistant_messages() {
    let root = temp_workspace("opensks-cli-turn-start");
    let cid = create_conversation_id(&root);

    let turn = conversation_json(
        &[
            "turn-start",
            "--conversation",
            &cid,
            "--text",
            "ship the vertical slice",
        ],
        &root,
    );
    assert_eq!(turn["schema"], "opensks.conversation-turn.v1");
    assert_eq!(turn["reused"], false);
    assert_eq!(turn["run_state"], "failed");
    assert!(
        turn["settings_digest"]
            .as_str()
            .expect("settings digest")
            .starts_with("sha256:v1:")
    );
    assert_eq!(
        turn["model_routing_decision"]["schema"],
        "opensks.routing-decision.v1"
    );
    assert_eq!(
        turn["model_routing_decision"]["status"],
        "blocked_missing_capability"
    );
    assert_eq!(
        turn["model_routing_decision"]["reason_code"],
        "thread_settings_model_not_selected"
    );
    assert_eq!(
        turn["model_routing_decision"]["route_receipt"]["reason_code"],
        "thread_settings_model_not_selected"
    );
    assert_eq!(
        turn["model_routing_decision"]["route_receipt"]["requested_capabilities"]["code"],
        true
    );
    assert_eq!(
        turn["model_routing_decision"]["route_receipt"]["registry_revision"],
        turn["model_routing_decision"]["model_snapshot_hash"]
    );
    let user_message_id = turn["user_message_id"].as_str().expect("user id");
    let assistant_message_id = turn["assistant_message_id"].as_str().expect("assistant id");
    assert_ne!(user_message_id, assistant_message_id);
    assert!(
        turn["run_id"]
            .as_str()
            .expect("run id")
            .starts_with("turn-")
    );

    let messages = conversation_json(&["messages", "--conversation", &cid], &root);
    let listed = messages["messages"].as_array().expect("messages array");
    assert_eq!(listed.len(), 2, "user + assistant message persisted");
    assert_eq!(listed[0]["role"], "user");
    assert_eq!(listed[1]["role"], "assistant");
    assert_eq!(listed[1]["state"], "failed");
    let assistant_content = listed[1]["content_redacted"]
        .as_str()
        .expect("assistant content");
    assert!(assistant_content.contains("Needs setup"));
    assert!(
        !assistant_content.contains("work items"),
        "leftover fake summary: {assistant_content}"
    );
    let timeline = conversation_json(&["timeline", "--conversation", &cid], &root);
    assert_eq!(timeline["schema"], "opensks.conversation-timeline.v1");
    let timeline_items = timeline["items"].as_array().expect("timeline items");
    assert!(
        timeline_items.len() >= 4,
        "message items plus event journal items are replayed"
    );
    assert_eq!(timeline_items[0]["kind"], "user_message");
    assert_eq!(timeline_items[0]["payload"]["role"], "user");
    assert_eq!(timeline_items[1]["kind"], "assistant_message");
    assert_eq!(timeline_items[1]["run_id"], turn["run_id"]);
    assert_eq!(timeline_items[1]["state"], "failed");
    assert!(
        timeline_items.iter().any(|item| {
            item["kind"] == "error"
                && item["payload"]["event_kind"] == "verification_failed"
                && item["payload"]["payload_redacted"]["agent_event_kind"] == "error"
                && item["payload"]["content_redacted"]
                    .as_str()
                    .is_some_and(|text| text.contains("Needs setup"))
        }),
        "setup-required event must be replayed as a durable timeline error: {timeline_items:#?}"
    );
    let run_id = turn["run_id"].as_str().expect("run id");
    let store = opensks_event_store::EventStore::open_workspace(&root).expect("event store");
    let events = store.replay(run_id).expect("replay setup events");
    assert_eq!(
        events.first().map(|event| &event.kind),
        Some(&opensks_contracts::EventKind::RunStarted)
    );
    assert!(
        events.iter().any(|event| {
            event.kind == opensks_contracts::EventKind::VerificationFailed
                && event.payload["agent_event_kind"] == "error"
                && event.payload["payload"]["code"] == "setup_required"
        }),
        "setup-required failure must be journaled: {events:#?}"
    );

    fs::remove_dir_all(root).ok();
}

#[test]
fn cli_turn_start_codex_lb_pinned_model_seeds_provider_registry() {
    let root = temp_workspace("opensks-cli-turn-codex-lb-sync");
    let mut thread_settings =
        opensks_contracts::ConversationThreadSettings::default_for("conversation-1", 1_000);
    thread_settings.model_selection = opensks_contracts::ModelSelection {
        mode: opensks_contracts::ModelSelectionMode::Pinned,
        model_id: Some(format!(
            "{}/gpt-5.4-nano",
            opensks_provider::CODEX_LB_PROVIDER_ID
        )),
        fallback_model_ids: Vec::new(),
    };
    let settings = turn_settings_from_thread(&thread_settings);

    sync_cli_external_provider_registry_for_settings_from_config(
        &root,
        &settings,
        1_100,
        true,
        Some("https://codex.example.test/backend-api/codex".to_string()),
    )
    .expect("sync codex-lb provider");

    let repo = opensks_provider::ProviderRepository::open_workspace(&root).expect("provider repo");
    let decision = opensks_provider::resolve_routing_decision_from_repository(
        &repo,
        "route-cli-sync",
        &settings,
    )
    .expect("routing decision");
    assert!(decision.status.has_resolved_model());
    assert_eq!(
        decision.selected_model_id.as_deref(),
        Some("provider-codex-lb-env/gpt-5.4-nano")
    );

    fs::remove_dir_all(root).ok();
}

#[test]
fn cli_turn_start_codex_lb_auto_mode_seeds_empty_provider_registry() {
    let root = temp_workspace("opensks-cli-turn-codex-lb-auto-sync");
    let thread_settings =
        opensks_contracts::ConversationThreadSettings::default_for("conversation-1", 1_000);
    let settings = turn_settings_from_thread(&thread_settings);

    sync_cli_external_provider_registry_for_settings_from_config(
        &root,
        &settings,
        1_100,
        true,
        Some("https://codex.example.test/backend-api/codex".to_string()),
    )
    .expect("sync codex-lb provider");

    let repo = opensks_provider::ProviderRepository::open_workspace(&root).expect("provider repo");
    let decision = opensks_provider::resolve_routing_decision_from_repository(
        &repo,
        "route-cli-auto-sync",
        &settings,
    )
    .expect("routing decision");
    assert!(decision.status.has_resolved_model());
    assert!(
        decision
            .selected_model_id
            .as_deref()
            .is_some_and(|model_id| model_id.starts_with("provider-codex-lb-env/")),
        "auto mode should resolve a codex-lb env model: {:?}",
        decision.selected_model_id
    );

    fs::remove_dir_all(root).ok();
}

#[test]
fn cli_auto_model_selection_prepares_codex_lb_dispatch_target() {
    let root = temp_workspace("opensks-cli-turn-codex-lb-auto-dispatch");
    let thread_settings =
        opensks_contracts::ConversationThreadSettings::default_for("conversation-1", 1_000);
    let settings = turn_settings_from_thread(&thread_settings);

    sync_cli_external_provider_registry_for_settings_from_config(
        &root,
        &settings,
        1_100,
        true,
        Some("https://codex.example.test/backend-api/codex".to_string()),
    )
    .expect("sync codex-lb provider");
    unsafe {
        std::env::set_var("CODEX_LB_API_KEY", "test-codex-lb-key");
    }

    let target = prepare_cli_provider_dispatch(&root, &settings)
        .expect("prepare dispatch")
        .expect("auto mode dispatch target");

    unsafe {
        std::env::remove_var("CODEX_LB_API_KEY");
    }
    assert_eq!(target.connection.id, opensks_provider::CODEX_LB_PROVIDER_ID);
    assert!(
        target
            .routing_decision
            .selected_model_id
            .as_deref()
            .is_some_and(|model_id| model_id.starts_with("provider-codex-lb-env/")),
        "auto dispatch should resolve a codex-lb model: {:?}",
        target.routing_decision.selected_model_id
    );
    assert!(target.routing_decision.status.has_resolved_model());

    fs::remove_dir_all(root).ok();
}

#[test]
fn cli_turn_start_provider_sync_ignores_non_codex_model() {
    let root = temp_workspace("opensks-cli-turn-codex-lb-ignore");
    let mut thread_settings =
        opensks_contracts::ConversationThreadSettings::default_for("conversation-1", 1_000);
    thread_settings.model_selection = opensks_contracts::ModelSelection {
        mode: opensks_contracts::ModelSelectionMode::Pinned,
        model_id: Some("provider-other/code-model".to_string()),
        fallback_model_ids: Vec::new(),
    };
    let settings = turn_settings_from_thread(&thread_settings);

    sync_cli_external_provider_registry_for_settings_from_config(
        &root,
        &settings,
        1_100,
        true,
        Some("https://codex.example.test/backend-api/codex".to_string()),
    )
    .expect("sync should no-op for non-codex model");

    let repo = opensks_provider::ProviderRepository::open_workspace(&root).expect("provider repo");
    assert!(
        repo.list_connections()
            .expect("connections")
            .into_iter()
            .all(|connection| connection.id != opensks_provider::CODEX_LB_PROVIDER_ID),
        "non-codex pinned model must not seed codex-lb"
    );

    fs::remove_dir_all(root).ok();
}
