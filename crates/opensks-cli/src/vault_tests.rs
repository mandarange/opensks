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

fn vault_json(args: &[&str], workspace: &Path) -> serde_json::Value {
    let mut owned = vec![
        args[0].to_string(),
        "--workspace".to_string(),
        workspace.to_string_lossy().into_owned(),
    ];
    owned.extend(args[1..].iter().map(|value| value.to_string()));
    let output = run_vault_command(&owned, workspace).expect("vault command");
    serde_json::from_str(&output.stdout).expect("valid vault json")
}

fn seed_vault_conversation(workspace: &Path) -> String {
    let created = conversation_json(&["create", "--title", "Vault slice"], workspace);
    let cid = created["id"].as_str().expect("conversation id").to_string();
    conversation_json(
        &[
            "append",
            "--conversation",
            &cid,
            "--role",
            "user",
            "--text",
            "Decision: adopt the age crate for the encrypted vault",
        ],
        workspace,
    );
    conversation_json(
        &[
            "append",
            "--conversation",
            &cid,
            "--role",
            "assistant",
            "--text",
            "raw assistant body that should never appear in a summary",
        ],
        workspace,
    );
    cid
}

#[test]
fn vault_export_summary_is_redacted_and_tracks_no_transcript() {
    let root = temp_workspace("opensks-cli-vault-summary");
    let cid = seed_vault_conversation(&root);

    let result = vault_json(&["export-summary", "--conversation", &cid], &root);
    assert_eq!(result["schema"], "opensks.vault-summary.v1");
    assert_eq!(result["conversation_id"], cid);
    assert_eq!(result["contains_raw_transcript"], false);
    assert_eq!(result["redacted"], true);

    let summary_path = result["summary_path"].as_str().expect("summary path");
    let on_disk = fs::read_to_string(summary_path).expect("summary on disk");
    assert!(on_disk.contains("adopt the age crate"));
    assert!(
        !on_disk.contains("raw assistant body"),
        "summary leaked raw transcript: {on_disk}"
    );

    fs::remove_dir_all(&root).ok();
}

#[test]
fn vault_encrypt_decrypt_roundtrips_and_bad_recipient_writes_nothing() {
    let root = temp_workspace("opensks-cli-vault-encrypt");
    let cid = seed_vault_conversation(&root);

    let identity = age::x25519::Identity::generate();
    let recipient = identity.to_public().to_string();
    let identity_path = root.join("identity.txt");
    {
        use age::secrecy::ExposeSecret;
        fs::write(
            &identity_path,
            format!("{}\n", identity.to_string().expose_secret()),
        )
        .expect("write identity");
    }

    let encrypt = vault_json(
        &["encrypt", "--conversation", &cid, "--recipient", &recipient],
        &root,
    );
    assert_eq!(encrypt["schema"], "opensks.vault-encrypt.v1");
    let vault_path = encrypt["vault_path"].as_str().expect("vault path");
    assert!(Path::new(vault_path).exists());

    let decrypt = vault_json(
        &[
            "decrypt",
            "--vault",
            vault_path,
            "--identity-file",
            identity_path.to_str().unwrap(),
        ],
        &root,
    );
    assert_eq!(decrypt["schema"], "opensks.vault-decrypt.v1");
    assert_eq!(decrypt["conversation_id"], cid);

    let bad = run_vault_command(
        &[
            "encrypt".to_string(),
            "--workspace".to_string(),
            root.to_string_lossy().into_owned(),
            "--conversation".to_string(),
            cid.clone(),
            "--recipient".to_string(),
            "totally-not-an-age-key".to_string(),
        ],
        &root,
    )
    .expect_err("bad recipient must fail");
    let parsed: serde_json::Value =
        serde_json::from_str(&bad.to_string()).expect("vault error is JSON");
    assert_eq!(parsed["schema"], "opensks.vault-error.v1");
    assert_eq!(parsed["error"]["code"], "bad_recipient");

    let status = vault_json(&["status"], &root);
    assert_eq!(status["schema"], "opensks.vault-status.v1");
    assert_eq!(status["vaults"].as_array().unwrap().len(), 1);

    fs::remove_dir_all(&root).ok();
}

#[test]
fn vault_decrypt_with_wrong_identity_fails_closed() {
    let root = temp_workspace("opensks-cli-vault-wrongkey");
    let cid = seed_vault_conversation(&root);

    let right = age::x25519::Identity::generate();
    let encrypt = vault_json(
        &[
            "encrypt",
            "--conversation",
            &cid,
            "--recipient",
            &right.to_public().to_string(),
        ],
        &root,
    );
    let vault_path = encrypt["vault_path"]
        .as_str()
        .expect("vault path")
        .to_string();

    let wrong = age::x25519::Identity::generate();
    let wrong_path = root.join("wrong.txt");
    {
        use age::secrecy::ExposeSecret;
        fs::write(
            &wrong_path,
            format!("{}\n", wrong.to_string().expose_secret()),
        )
        .expect("write wrong identity");
    }

    let err = run_vault_command(
        &[
            "decrypt".to_string(),
            "--workspace".to_string(),
            root.to_string_lossy().into_owned(),
            "--vault".to_string(),
            vault_path,
            "--identity-file".to_string(),
            wrong_path.to_string_lossy().into_owned(),
        ],
        &root,
    )
    .expect_err("wrong identity must fail");
    let parsed: serde_json::Value =
        serde_json::from_str(&err.to_string()).expect("vault error JSON");
    assert_eq!(parsed["error"]["code"], "decrypt_failed");

    fs::remove_dir_all(&root).ok();
}
