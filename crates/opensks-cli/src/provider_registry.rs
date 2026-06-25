use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use crate::{CliError, CliOutput, now_unix_millis, provider_usage};

#[derive(Debug, Default)]
struct ProviderRegistryOptions {
    workspace: Option<PathBuf>,
    provider_id: Option<String>,
    model_id: Option<String>,
    connection_json: Option<String>,
    models_json: Option<String>,
    receipt_json: Option<String>,
    keychain_command: Option<PathBuf>,
    expected_revision: Option<u64>,
    enabled: Option<bool>,
}

pub fn is_registry_subcommand(value: &str) -> bool {
    matches!(
        value,
        "registry-list"
            | "registry-upsert"
            | "registry-delete"
            | "registry-set-enabled"
            | "registry-probe"
            | "registry-sync-models"
            | "registry-set-model-enabled"
            | "registry-record-probe"
    )
}

pub fn run_provider_registry_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Err(CliError::Usage(provider_usage().to_string()));
    };
    if subcommand == "--help" || subcommand == "-h" || subcommand == "help" {
        return Ok(CliOutput {
            stdout: provider_usage().to_string(),
        });
    }
    let options = parse_provider_registry_options(&args[1..], provider_usage())?;
    let workspace = options.workspace.unwrap_or_else(|| cwd.to_path_buf());
    let repo = opensks_provider::ProviderRepository::open_workspace(&workspace)
        .map_err(|error| CliError::Invalid(error.to_string()))?;
    let now_ms = now_unix_millis()?;

    match subcommand {
        "registry-list" => {
            let providers = repo
                .list_connections()
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let mut models = Vec::new();
            let mut latest_probes = Vec::new();
            for provider in &providers {
                models.extend(
                    repo.list_models(&provider.id)
                        .map_err(|error| CliError::Invalid(error.to_string()))?,
                );
                if let Some(receipt) = repo
                    .latest_probe_receipt(&provider.id)
                    .map_err(|error| CliError::Invalid(error.to_string()))?
                {
                    latest_probes.push(receipt);
                }
            }
            json_output(&serde_json::json!({
                "schema": "opensks.provider-registry-state.v1",
                "providers": providers,
                "models": models,
                "latest_probes": latest_probes,
            }))
        }
        "registry-upsert" => {
            let raw = required(options.connection_json.as_deref())?;
            let connection: opensks_contracts::ProviderConnection = serde_json::from_str(raw)
                .map_err(|error| {
                    CliError::Invalid(format!("invalid provider connection json: {error}"))
                })?;
            let receipt = repo
                .upsert_connection(&connection, options.expected_revision, now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            json_output(&serde_json::json!({
                "schema": "opensks.provider-registry-command-result.v1",
                "receipt": receipt,
            }))
        }
        "registry-delete" => {
            let provider_id = required(options.provider_id.as_deref())?;
            let expected = options
                .expected_revision
                .ok_or_else(|| CliError::Usage(provider_usage().to_string()))?;
            let receipt = repo
                .delete_connection(provider_id, expected, now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            json_output(&serde_json::json!({
                "schema": "opensks.provider-registry-command-result.v1",
                "receipt": receipt,
            }))
        }
        "registry-set-enabled" => {
            let provider_id = required(options.provider_id.as_deref())?;
            let enabled = options
                .enabled
                .ok_or_else(|| CliError::Usage(provider_usage().to_string()))?;
            let expected = options
                .expected_revision
                .ok_or_else(|| CliError::Usage(provider_usage().to_string()))?;
            let receipt = repo
                .set_provider_enabled(provider_id, enabled, expected, now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            json_output(&serde_json::json!({
                "schema": "opensks.provider-registry-command-result.v1",
                "receipt": receipt,
            }))
        }
        "registry-probe" => {
            let provider_id = required(options.provider_id.as_deref())?;
            let connection = repo
                .get_connection(provider_id)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let credential =
                resolve_provider_secret(&connection, options.keychain_command.as_deref())?;
            let outcome = opensks_provider::probe_openai_compatible_provider(
                &connection,
                &credential,
                now_ms,
                Duration::from_secs(30),
            )
            .map_err(|error| CliError::Invalid(error.to_string()))?;
            repo.record_probe_receipt(&outcome.receipt)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let health = opensks_provider::health_snapshot_from_probe_receipt(&outcome.receipt);
            let provider = repo
                .set_provider_health(provider_id, health, now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let sync_receipt = if outcome.models.is_empty() {
                None
            } else {
                Some(
                    repo.sync_models(provider_id, &outcome.models, now_ms)
                        .map_err(|error| CliError::Invalid(error.to_string()))?,
                )
            };
            json_output(&serde_json::json!({
                "schema": "opensks.provider-registry-probe-result.v1",
                "provider": provider,
                "probe_receipt": outcome.receipt,
                "models": outcome.models,
                "sync_receipt": sync_receipt,
            }))
        }
        "registry-sync-models" => {
            let provider_id = required(options.provider_id.as_deref())?;
            let raw = required(options.models_json.as_deref())?;
            let models: Vec<opensks_contracts::ModelCatalogEntry> = serde_json::from_str(raw)
                .map_err(|error| {
                    CliError::Invalid(format!("invalid model catalog json: {error}"))
                })?;
            let receipt = repo
                .sync_models(provider_id, &models, now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            json_output(&serde_json::json!({
                "schema": "opensks.provider-registry-command-result.v1",
                "receipt": receipt,
            }))
        }
        "registry-set-model-enabled" => {
            let model_id = required(options.model_id.as_deref())?;
            let enabled = options
                .enabled
                .ok_or_else(|| CliError::Usage(provider_usage().to_string()))?;
            let model = repo
                .set_model_enabled(model_id, enabled, now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            json_output(&serde_json::json!({
                "schema": "opensks.provider-registry-model-result.v1",
                "model": model,
            }))
        }
        "registry-record-probe" => {
            let raw = required(options.receipt_json.as_deref())?;
            let receipt: opensks_contracts::ProviderProbeReceipt = serde_json::from_str(raw)
                .map_err(|error| {
                    CliError::Invalid(format!("invalid provider probe receipt json: {error}"))
                })?;
            repo.record_probe_receipt(&receipt)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            json_output(&serde_json::json!({
                "schema": "opensks.provider-registry-command-result.v1",
                "receipt": receipt,
            }))
        }
        other => Err(CliError::Usage(format!(
            "unknown provider registry subcommand `{other}`\n\n{}",
            provider_usage()
        ))),
    }
}

fn parse_provider_registry_options(
    args: &[String],
    usage: &str,
) -> Result<ProviderRegistryOptions, CliError> {
    let mut options = ProviderRegistryOptions::default();
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let mut value = || -> Result<String, CliError> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| CliError::Usage(format!("{flag} requires a value\n\n{usage}")))
        };
        match flag {
            "--workspace" => options.workspace = Some(PathBuf::from(value()?)),
            "--provider" => options.provider_id = Some(value()?),
            "--model" => options.model_id = Some(value()?),
            "--connection" => options.connection_json = Some(value()?),
            "--models" => options.models_json = Some(value()?),
            "--receipt" => options.receipt_json = Some(value()?),
            "--keychain-command" => options.keychain_command = Some(PathBuf::from(value()?)),
            "--expected-revision" => {
                let raw = value()?;
                options.expected_revision = Some(raw.parse().map_err(|_| {
                    CliError::Usage(format!("--expected-revision must be a u64\n\n{usage}"))
                })?);
            }
            "--enabled" => {
                let raw = value()?;
                options.enabled = Some(parse_cli_bool(&raw, "--enabled", usage)?);
            }
            "--help" | "-h" => return Err(CliError::Usage(usage.to_string())),
            other => {
                return Err(CliError::Usage(format!(
                    "unknown provider registry option `{other}`\n\n{usage}"
                )));
            }
        }
        index += 1;
    }
    Ok(options)
}

fn required(value: Option<&str>) -> Result<&str, CliError> {
    value.ok_or_else(|| CliError::Usage(provider_usage().to_string()))
}

fn resolve_provider_secret(
    connection: &opensks_contracts::ProviderConnection,
    keychain_command: Option<&Path>,
) -> Result<String, CliError> {
    match connection.auth.store {
        opensks_contracts::SecretStoreKind::MacosKeychain => {
            resolve_macos_keychain_secret(&connection.auth, keychain_command)
        }
        other => Err(CliError::Invalid(format!(
            "provider secret store `{other:?}` is not supported by registry-probe"
        ))),
    }
}

fn resolve_macos_keychain_secret(
    auth: &opensks_contracts::SecretRef,
    keychain_command: Option<&Path>,
) -> Result<String, CliError> {
    #[cfg(not(target_os = "macos"))]
    if keychain_command.is_none() {
        return Err(CliError::Invalid(
            "macOS Keychain provider probe is only available on macOS".to_string(),
        ));
    }

    let command = keychain_command
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("security"));
    let output = Command::new(command)
        .arg("find-generic-password")
        .arg("-s")
        .arg(&auth.service)
        .arg("-a")
        .arg(&auth.account)
        .arg("-w")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| CliError::Invalid(format!("keychain lookup failed: {error}")))?;
    if !output.status.success() {
        return Err(CliError::Invalid(
            "provider credential was not found in Keychain".to_string(),
        ));
    }
    let value = String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_string();
    if value.trim().is_empty() {
        return Err(CliError::Invalid(
            "provider credential resolved empty".to_string(),
        ));
    }
    Ok(value)
}

fn parse_cli_bool(raw: &str, flag: &str, usage: &str) -> Result<bool, CliError> {
    match raw {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(CliError::Usage(format!(
            "{flag} must be true or false\n\n{usage}"
        ))),
    }
}

fn json_output(value: &serde_json::Value) -> Result<CliOutput, CliError> {
    Ok(CliOutput {
        stdout: serde_json::to_string_pretty(value).map_err(|error| {
            CliError::Invalid(format!("serialize provider registry json: {error}"))
        })? + "\n",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::{
        fs,
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    #[test]
    fn registry_commands_roundtrip_secretless_state() {
        let root = temp_workspace("opensks-cli-provider-registry");
        let workspace = root.to_string_lossy().into_owned();
        let connection = serde_json::json!({
            "schema": "opensks.provider-connection.v1",
            "id": "provider-1",
            "kind": "open_router",
            "display_name": "OpenRouter",
            "enabled": true,
            "endpoint": {"base_url": "https://openrouter.ai/api/v1", "allow_insecure_http": false},
            "auth": {
                "schema": "opensks.secret-ref.v1",
                "store": "macos_keychain",
                "service": "ai.opensks.provider.openrouter",
                "account": "provider-1",
                "version": 7
            },
            "health": {"state": "unknown", "circuit_open": false, "reason_code": "not_probed"},
            "concurrency": {"max_concurrent_requests": 4, "requests_per_minute": 60},
            "created_at_ms": 10,
            "updated_at_ms": 10,
            "revision": 1
        })
        .to_string();
        let saved = run_provider_registry_command(
            &[
                "registry-upsert".into(),
                "--workspace".into(),
                workspace.clone(),
                "--connection".into(),
                connection,
            ],
            &root,
        )
        .expect("save provider");
        let saved_json: serde_json::Value = serde_json::from_str(&saved.stdout).expect("json");
        assert_eq!(saved_json["receipt"]["secret_value_exposed"], false);
        assert!(!saved.stdout.contains("sk-test-secret"));

        let models = serde_json::json!([{
            "schema": "opensks.model-catalog-entry.v1",
            "id": "provider-1/code-model",
            "provider_id": "provider-1",
            "remote_model_id": "code-model",
            "display_name": "Code Model",
            "enabled": true,
            "capabilities": {
                "text": true,
                "code": true,
                "vision_input": false,
                "image_output": false,
                "image_edit": false,
                "tool_use": true,
                "structured_output": true,
                "long_context": true,
                "streaming": true
            },
            "limits": {"max_input_tokens": 128000, "max_output_tokens": 16000},
            "health": "healthy",
            "role_scores": {"code": {"score": 0.9, "evidence_refs": ["cli-test"]}},
            "catalog_revision": "catalog-1"
        }])
        .to_string();
        run_provider_registry_command(
            &[
                "registry-sync-models".into(),
                "--workspace".into(),
                workspace.clone(),
                "--provider".into(),
                "provider-1".into(),
                "--models".into(),
                models,
            ],
            &root,
        )
        .expect("sync models");
        let disabled = run_provider_registry_command(
            &[
                "registry-set-model-enabled".into(),
                "--workspace".into(),
                workspace.clone(),
                "--model".into(),
                "provider-1/code-model".into(),
                "--enabled".into(),
                "false".into(),
            ],
            &root,
        )
        .expect("disable model");
        let disabled_json: serde_json::Value =
            serde_json::from_str(&disabled.stdout).expect("json");
        assert_eq!(disabled_json["model"]["enabled"], false);

        let probe = serde_json::json!({
            "schema": "opensks.provider-probe-receipt.v1",
            "provider_id": "provider-1",
            "endpoint_host_redacted": "openrouter.ai",
            "http_category": "success",
            "latency_bucket": "under1_s",
            "auth_accepted": true,
            "model_list_available": true,
            "catalog_count": 1,
            "occurred_at_ms": 20,
            "reason_code": "probe_ok"
        })
        .to_string();
        run_provider_registry_command(
            &[
                "registry-record-probe".into(),
                "--workspace".into(),
                workspace.clone(),
                "--receipt".into(),
                probe,
            ],
            &root,
        )
        .expect("record probe");

        let listed = run_provider_registry_command(
            &[
                "registry-list".into(),
                "--workspace".into(),
                workspace.clone(),
            ],
            &root,
        )
        .expect("list");
        let listed_json: serde_json::Value = serde_json::from_str(&listed.stdout).expect("json");
        assert_eq!(listed_json["providers"][0]["id"], "provider-1");
        assert_eq!(listed_json["models"][0]["enabled"], false);
        assert_eq!(listed_json["latest_probes"][0]["reason_code"], "probe_ok");
        assert!(!listed.stdout.contains("sk-test-secret"));

        let deleted = run_provider_registry_command(
            &[
                "registry-delete".into(),
                "--workspace".into(),
                workspace.clone(),
                "--provider".into(),
                "provider-1".into(),
                "--expected-revision".into(),
                "1".into(),
            ],
            &root,
        )
        .expect("delete");
        let deleted_json: serde_json::Value = serde_json::from_str(&deleted.stdout).expect("json");
        assert_eq!(deleted_json["receipt"]["secret_value_exposed"], false);
        let relisted = run_provider_registry_command(
            &["registry-list".into(), "--workspace".into(), workspace],
            &root,
        )
        .expect("relist");
        let relisted_json: serde_json::Value =
            serde_json::from_str(&relisted.stdout).expect("json");
        assert!(
            relisted_json["providers"]
                .as_array()
                .expect("providers")
                .is_empty()
        );
        assert!(
            relisted_json["models"]
                .as_array()
                .expect("models")
                .is_empty()
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    #[cfg(unix)]
    fn registry_probe_resolves_keychain_ref_and_syncs_models_secretlessly() {
        let root = temp_workspace("opensks-cli-provider-probe");
        let workspace = root.to_string_lossy().into_owned();
        let endpoint = spawn_models_server(
            200,
            r#"{"data":[{"id":"code-model","name":"Code Model","context_length":64000,"supported_parameters":["tools","response_format"]}]}"#,
            Some("fixture-credential"),
        );
        let connection = serde_json::json!({
            "schema": "opensks.provider-connection.v1",
            "id": "provider-1",
            "kind": "open_ai_compatible",
            "display_name": "Local OpenAI Compatible",
            "enabled": true,
            "endpoint": {"base_url": endpoint, "allow_insecure_http": true},
            "auth": {
                "schema": "opensks.secret-ref.v1",
                "store": "macos_keychain",
                "service": "ai.opensks.provider.test",
                "account": "provider-1",
                "version": 1
            },
            "health": {"state": "unknown", "circuit_open": false, "reason_code": "not_probed"},
            "concurrency": {"max_concurrent_requests": 2},
            "created_at_ms": 10,
            "updated_at_ms": 10,
            "revision": 1
        })
        .to_string();
        run_provider_registry_command(
            &[
                "registry-upsert".into(),
                "--workspace".into(),
                workspace.clone(),
                "--connection".into(),
                connection,
            ],
            &root,
        )
        .expect("save provider");
        let keychain_command = write_mock_keychain_command(&root, "fixture-credential");

        let probed = run_provider_registry_command(
            &[
                "registry-probe".into(),
                "--workspace".into(),
                workspace.clone(),
                "--provider".into(),
                "provider-1".into(),
                "--keychain-command".into(),
                keychain_command.to_string_lossy().into_owned(),
            ],
            &root,
        )
        .expect("probe provider");
        assert!(!probed.stdout.contains("fixture-credential"));
        let probed_json: serde_json::Value = serde_json::from_str(&probed.stdout).expect("json");
        assert_eq!(probed_json["provider"]["health"]["state"], "healthy");
        assert_eq!(probed_json["probe_receipt"]["http_category"], "success");
        assert_eq!(probed_json["probe_receipt"]["auth_accepted"], true);
        assert_eq!(probed_json["models"][0]["id"], "provider-1/code-model");
        assert_eq!(probed_json["models"][0]["health"], "healthy");

        let listed = run_provider_registry_command(
            &["registry-list".into(), "--workspace".into(), workspace],
            &root,
        )
        .expect("list");
        assert!(!listed.stdout.contains("fixture-credential"));
        let listed_json: serde_json::Value = serde_json::from_str(&listed.stdout).expect("json");
        assert_eq!(listed_json["providers"][0]["health"]["state"], "healthy");
        assert_eq!(listed_json["models"][0]["remote_model_id"], "code-model");
        assert_eq!(listed_json["latest_probes"][0]["reason_code"], "probe_ok");
        fs::remove_dir_all(root).ok();
    }

    fn temp_workspace(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("{label}-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).expect("temp workspace");
        root
    }

    #[cfg(unix)]
    fn write_mock_keychain_command(root: &Path, value: &str) -> PathBuf {
        let path = root.join("mock-security.sh");
        fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", value))
            .expect("write mock keychain command");
        let mut permissions = fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("chmod mock keychain command");
        path
    }

    fn spawn_models_server(status: u16, body: &str, expected_bearer: Option<&str>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("local addr");
        let body = body.to_string();
        let expected_bearer = expected_bearer.map(str::to_string);
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = [0_u8; 4096];
            let bytes = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(request.starts_with("GET /v1/models "));
            if let Some(secret) = expected_bearer {
                let lower_request = request.to_ascii_lowercase();
                assert!(
                    lower_request
                        .contains(&format!("authorization: bearer {secret}").to_ascii_lowercase())
                );
            }
            let reason = if status == 200 { "OK" } else { "Unauthorized" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });
        format!("http://{address}/v1")
    }
}
