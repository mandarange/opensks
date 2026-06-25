use super::*;

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

#[test]
fn supervisor_tick_claims_one_queued_accepted_turn() {
    let root = temp_workspace("opensks-cli-supervisor-tick");
    let created = conversation_json(&["create", "--title", "Supervisor Tick"], &root);
    let cid = created["id"].as_str().expect("conversation id").to_string();
    let pid = created["project_id"]
        .as_str()
        .expect("project id")
        .to_string();
    let thread_settings = opensks_contracts::ConversationThreadSettings::default_for(&cid, 2_000);
    let repo = opensks_conversation::ConversationRepository::open_workspace(&root).expect("repo");
    let accepted = repo
        .accept_conversation_turn(
            &opensks_contracts::ConversationTurnStartRequest {
                schema: opensks_contracts::CONVERSATION_TURN_START_REQUEST_SCHEMA.to_string(),
                request_id: "req-supervisor-cli".to_string(),
                project_id: pid,
                conversation_id: cid.clone(),
                client_turn_id: "client-supervisor-cli".to_string(),
                message: opensks_contracts::UserMessageInput {
                    text: "claim this queued turn".to_string(),
                    attachment_refs: vec![],
                },
                thread_settings_updated_at_ms: None,
                settings: Some(turn_settings_from_thread(&thread_settings)),
                context: opensks_contracts::TurnContextSelection::default(),
                idempotency_key: "idem-supervisor-cli".to_string(),
            },
            2_000,
        )
        .unwrap();
    drop(repo);

    let tick = conversation_json(
        &[
            "supervisor-tick",
            "--supervisor-id",
            "supervisor-cli",
            "--lease-ttl-ms",
            "500",
        ],
        &root,
    );
    assert_eq!(tick["schema"], "opensks.turn-supervisor-tick.v1");
    assert_eq!(tick["supervisor_id"], "supervisor-cli");
    assert_eq!(
        tick["claimed"]["turn_id"].as_str(),
        Some(accepted.turn_id.as_str())
    );
    assert_eq!(
        tick["claimed"]["run_id"].as_str(),
        Some(accepted.run_id.as_str())
    );
    assert_eq!(tick["claimed"]["lease_owner"], "supervisor-cli");

    let second = conversation_json(
        &["supervisor-tick", "--supervisor-id", "supervisor-cli"],
        &root,
    );
    assert!(second["claimed"].is_null());

    let runs = conversation_json(&["runs", "--conversation", &cid], &root);
    assert_eq!(runs["runs"][0]["run_state"], "running");

    fs::remove_dir_all(root).ok();
}
