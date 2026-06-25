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
