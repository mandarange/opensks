use std::fs;
use std::io;
use std::path::Path;
use std::process;

use crate::{
    CliError, CliOutput, require_exact_subcommand_cli, sha256_bytes_v1, write_text_atomic,
};

pub(crate) const RELEASE_PROOF_REQUIRED_ARTIFACTS: &[(&str, &str)] = &[
    (
        "runtime_truth_matrix",
        "docs/runtime-truth-matrix.generated.md",
    ),
    (
        "release_requirement_matrix",
        "docs/release/requirement-matrix.md",
    ),
    (
        "dependency_advisories",
        "docs/security/dependency-advisories.md",
    ),
    ("core_ci_workflow", ".github/workflows/ci-core.yml"),
    ("security_ci_workflow", ".github/workflows/ci-security.yml"),
    (
        "macos_app_ci_workflow",
        ".github/workflows/ci-macos-app.yml",
    ),
    ("codeql_workflow", ".github/workflows/codeql.yml"),
    (
        "engine_request_schema",
        "schemas/engine-request.schema.json",
    ),
    (
        "provider_adapter_check_schema",
        "schemas/provider-adapter-check.schema.json",
    ),
    ("release_proof_schema", "schemas/release-proof.schema.json"),
    ("cargo_lock", "Cargo.lock"),
];

pub fn run_gc_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    require_exact_subcommand_cli(args, "plan", gc_usage())?;
    let plan = opensks_retention::plan_gc(
        &[
            ".opensks/runtime/worktrees/run-active/worker".to_string(),
            ".opensks/runtime/worktrees/run-old/worker".to_string(),
            ".opensks/wiki/records/ar.jsonl".to_string(),
        ],
        "run-active",
    );
    let dir = cwd.join(".opensks").join("gc");
    fs::create_dir_all(&dir)?;
    write_text_atomic(
        &dir.join("gc-plan.json"),
        &(serde_json::to_string_pretty(&plan)
            .map_err(|error| CliError::Invalid(format!("serialize gc plan: {error}")))?
            + "\n"),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote retention GC plan\ndelete: {}\nblocked: {}\nartifact: {}\n",
            plan.delete_paths.len(),
            plan.blocked_paths.len(),
            dir.join("gc-plan.json").display()
        ),
    })
}

pub fn gc_usage() -> &'static str {
    "usage: opensks gc plan\n"
}

pub fn run_release_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    require_exact_subcommand_cli(args, "proof", release_usage())?;
    let (source_commit_sha, mut blockers) = release_source_commit_sha(cwd);
    let (workspace_dirty, status_blockers) = release_workspace_dirty(cwd);
    blockers.extend(status_blockers);
    let artifact_digests = collect_release_artifact_digests(cwd, source_commit_sha.as_deref())?;
    let upgrade_checked = release_upgrade_checked(cwd);
    let signing_evidence = release_signing_evidence(cwd);
    let signed_app = signing_evidence.production_signed;
    let notarized = signing_evidence.notarized;
    let proof = opensks_retention::release_proof_with_artifacts(
        "0.1.0",
        signed_app,
        notarized,
        true,
        true,
        upgrade_checked,
        source_commit_sha,
        workspace_dirty,
        artifact_digests,
        blockers,
        Some(signing_evidence),
    );
    let dir = cwd.join(".opensks").join("release");
    fs::create_dir_all(&dir)?;
    write_text_atomic(
        &dir.join("release-proof.json"),
        &(serde_json::to_string_pretty(&proof)
            .map_err(|error| CliError::Invalid(format!("serialize release proof: {error}")))?
            + "\n"),
    )?;
    let blocker_lines = release_blocker_stdout_lines(&proof.blockers);
    Ok(CliOutput {
        stdout: format!(
            "wrote release hardening proof\nstatus: {:?}\nsigned_app: {}\nartifact_digest_gate_passed: {}\nsame_sha_artifact_binding: {}\nblockers: {}\n{}artifact: {}\n",
            proof.status,
            proof.signed_app,
            proof.artifact_digest_gate_passed,
            proof.same_sha_artifact_binding,
            proof.blockers.len(),
            blocker_lines,
            dir.join("release-proof.json").display()
        ),
    })
}

pub fn release_usage() -> &'static str {
    "usage: opensks release proof\n"
}

fn release_source_commit_sha(
    cwd: &Path,
) -> (Option<String>, Vec<opensks_contracts::ReleaseProofBlocker>) {
    let output = match process::Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(cwd)
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            return (
                None,
                vec![opensks_contracts::ReleaseProofBlocker {
                    code: "git_unavailable".to_string(),
                    message: format!("failed to execute git rev-parse: {error}"),
                }],
            );
        }
    };
    if output.status.success() {
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if sha.len() == 40 && sha.chars().all(|char| char.is_ascii_hexdigit()) {
            return (Some(sha), Vec::new());
        }
        return (
            None,
            vec![opensks_contracts::ReleaseProofBlocker {
                code: "git_head_malformed".to_string(),
                message: "git rev-parse HEAD did not return a 40-character SHA".to_string(),
            }],
        );
    }
    (
        None,
        vec![opensks_contracts::ReleaseProofBlocker {
            code: "git_head_unavailable".to_string(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        }],
    )
}

fn release_workspace_dirty(cwd: &Path) -> (bool, Vec<opensks_contracts::ReleaseProofBlocker>) {
    let output = match process::Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=no"])
        .current_dir(cwd)
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            return (
                false,
                vec![opensks_contracts::ReleaseProofBlocker {
                    code: "git_status_unavailable".to_string(),
                    message: format!("failed to execute git status: {error}"),
                }],
            );
        }
    };
    if output.status.success() {
        let dirty_paths = tracked_dirty_paths_from_porcelain(&output.stdout);
        if dirty_paths.is_empty() {
            return (false, Vec::new());
        }
        let sample_paths = dirty_paths
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let remaining = dirty_paths.len().saturating_sub(5);
        let suffix = if remaining > 0 {
            format!("; plus {remaining} more")
        } else {
            String::new()
        };
        return (
            true,
            vec![opensks_contracts::ReleaseProofBlocker {
                code: "workspace_dirty".to_string(),
                message: format!(
                    "tracked workspace changes prevent same-SHA release artifact binding ({} tracked paths: {}{})",
                    dirty_paths.len(),
                    sample_paths,
                    suffix
                ),
            }],
        );
    }
    (
        false,
        vec![opensks_contracts::ReleaseProofBlocker {
            code: "git_status_unavailable".to_string(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        }],
    )
}

fn tracked_dirty_paths_from_porcelain(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| {
            let path = line.get(3..)?.trim();
            if path.is_empty() {
                return None;
            }
            Some(path.rsplit(" -> ").next().unwrap_or(path).to_string())
        })
        .collect()
}

fn release_blocker_stdout_lines(blockers: &[opensks_contracts::ReleaseProofBlocker]) -> String {
    if blockers.is_empty() {
        return String::new();
    }
    blockers
        .iter()
        .map(|blocker| format!("blocker: {} - {}\n", blocker.code, blocker.message))
        .collect()
}

fn release_signing_evidence(cwd: &Path) -> opensks_contracts::ReleaseSigningEvidence {
    let relative_app_path = ".opensks/macos/OpenSKS.app";
    let app_path = cwd.join(relative_app_path);
    if !app_path.exists() {
        return opensks_contracts::ReleaseSigningEvidence {
            checked: false,
            app_bundle_path: relative_app_path.to_string(),
            identifier: None,
            signature: None,
            team_identifier: None,
            cd_hash: None,
            production_signed: false,
            notarized: false,
            codesign_status: None,
            notarization_status: None,
            diagnostic: "app bundle candidate is missing".to_string(),
        };
    }

    let codesign = run_release_probe(cwd, "codesign", &["-dv", "--verbose=4", relative_app_path]);
    let notarization = run_release_probe(
        cwd,
        "spctl",
        &["-a", "-vvv", "-t", "install", relative_app_path],
    );
    let signature = parse_release_probe_field(&codesign.output, "Signature");
    let team_identifier = parse_release_probe_field(&codesign.output, "TeamIdentifier");
    let identifier = parse_release_probe_field(&codesign.output, "Identifier");
    let cd_hash = parse_release_probe_field(&codesign.output, "CDHash");
    let production_signed = codesign.status == Some(0)
        && signature.as_deref().is_some_and(|value| value != "adhoc")
        && team_identifier
            .as_deref()
            .is_some_and(|value| value != "not set" && !value.is_empty());
    let notarized = notarization.status == Some(0)
        && (notarization.output.contains("accepted")
            || notarization
                .output
                .contains("source=Notarized Developer ID"));
    let diagnostic = format!(
        "codesign_status={:?}; signature={}; team_identifier={}; notarization_status={:?}; notarization_summary={}",
        codesign.status,
        signature.as_deref().unwrap_or("unknown"),
        team_identifier.as_deref().unwrap_or("unknown"),
        notarization.status,
        first_non_empty_line(&notarization.output).unwrap_or("no notarization output")
    );

    opensks_contracts::ReleaseSigningEvidence {
        checked: true,
        app_bundle_path: relative_app_path.to_string(),
        identifier,
        signature,
        team_identifier,
        cd_hash,
        production_signed,
        notarized,
        codesign_status: codesign.status,
        notarization_status: notarization.status,
        diagnostic,
    }
}

struct ReleaseProbeOutput {
    status: Option<i32>,
    output: String,
}

fn run_release_probe(cwd: &Path, command: &str, args: &[&str]) -> ReleaseProbeOutput {
    match process::Command::new(command)
        .args(args)
        .current_dir(cwd)
        .output()
    {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            ReleaseProbeOutput {
                status: output.status.code(),
                output: text,
            }
        }
        Err(error) => ReleaseProbeOutput {
            status: None,
            output: format!("failed to execute {command}: {error}"),
        },
    }
}

fn parse_release_probe_field(output: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    output
        .lines()
        .find_map(|line| line.strip_prefix(&prefix).map(str::trim))
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn first_non_empty_line(output: &str) -> Option<&str> {
    output.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

fn release_upgrade_checked(cwd: &Path) -> bool {
    let updater_dir = cwd.join(".opensks").join("updater");
    let Ok(manifest) = read_json_value(&updater_dir.join("update-manifest.json")) else {
        return false;
    };
    let Ok(signature) = read_json_value(&updater_dir.join("update-signature.json")) else {
        return false;
    };
    let Ok(channels) = read_json_value(&updater_dir.join("update-channels.json")) else {
        return false;
    };
    let Ok(rollback) = read_json_value(&updater_dir.join("rollback-plan.json")) else {
        return false;
    };
    let Ok(boundary) = read_json_value(&updater_dir.join("update-boundary.json")) else {
        return false;
    };
    let Ok(final_state) = read_json_value(&updater_dir.join("updater-final-state.json")) else {
        return false;
    };
    let Ok(manifest_text) = fs::read_to_string(updater_dir.join("update-manifest.json")) else {
        return false;
    };

    let manifest_hash = local_stable_content_hash(&manifest_text);
    let expected_signature = local_update_signature(&manifest_hash);
    json_string(&manifest, "schema") == Some("opensks.update-manifest.v1")
        && json_string(&manifest, "current_version") == Some(env!("CARGO_PKG_VERSION"))
        && json_string(&manifest, "default_channel") == Some("stable")
        && json_bool(&manifest, "requires_signature") == Some(true)
        && json_bool(&manifest, "requires_rollback_plan") == Some(true)
        && json_bool(&manifest, "network_install_enabled") == Some(false)
        && json_array_contains(&manifest, "channels", &["stable", "latest"])
        && json_array_contains(
            &manifest,
            "artifacts",
            &["opensks-cli", "app-bundle-candidate", "manifest"],
        )
        && json_string(&signature, "schema") == Some("opensks.update-signature.v1")
        && json_string(&signature, "manifest_hash") == Some(manifest_hash.as_str())
        && json_string(&signature, "signature") == Some(expected_signature.as_str())
        && json_string(&signature, "trusted_signer_fingerprint")
            == Some("opensks-local-dev-trusted-signer-v1")
        && json_string(&signature, "algorithm")
            == Some("fnv1a64-local-dev-proof-not-production-crypto")
        && json_bool(&signature, "production_crypto_live") == Some(false)
        && json_string(&channels, "schema") == Some("opensks.update-channels.v1")
        && channel_gate_passed(&channels, "stable")
        && channel_gate_passed(&channels, "latest")
        && json_string(&rollback, "schema") == Some("opensks.rollback-plan.v1")
        && json_string(&rollback, "current_version") == Some(env!("CARGO_PKG_VERSION"))
        && json_array_contains(
            &rollback,
            "rollback_slots",
            &[
                "previous-stable",
                "previous-latest",
                "manual-operator-restore",
            ],
        )
        && json_string(&rollback, "restore_strategy")
            == Some("preserve_previous_manifest_and_binary_before_apply")
        && json_bool(&rollback, "apply_transaction_live") == Some(false)
        && json_string(&boundary, "schema") == Some("opensks.update-boundary.v1")
        && json_bool(&boundary, "auto_download") == Some(false)
        && json_bool(&boundary, "auto_apply") == Some(false)
        && json_bool(&boundary, "requires_operator_approval") == Some(true)
        && json_bool(&boundary, "requires_verified_signature") == Some(true)
        && json_bool(&boundary, "requires_rollback_plan") == Some(true)
        && json_string(&boundary, "signed_updater_live")
            == Some("local_manifest_signature_artifact_only")
        && json_string(&final_state, "schema") == Some("opensks.updater-final-state.v1")
        && json_string(&final_state, "status") == Some("verified_artifact_plan")
        && json_string(&final_state, "manifest_hash") == Some(manifest_hash.as_str())
        && json_bool(&final_state, "signature_verified") == Some(true)
        && json_array_contains(&final_state, "channels_present", &["stable", "latest"])
        && json_bool(&final_state, "rollback_plan_present") == Some(true)
        && json_bool(&final_state, "network_or_install_performed") == Some(false)
}

fn read_json_value(path: &Path) -> Result<serde_json::Value, CliError> {
    let text = fs::read_to_string(path)?;
    serde_json::from_str(&text)
        .map_err(|error| CliError::Invalid(format!("parse {}: {error}", path.display())))
}

fn json_string<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn json_bool(value: &serde_json::Value, key: &str) -> Option<bool> {
    value.get(key)?.as_bool()
}

fn json_array_contains(value: &serde_json::Value, key: &str, expected: &[&str]) -> bool {
    let Some(values) = value.get(key).and_then(|field| field.as_array()) else {
        return false;
    };
    expected.iter().all(|expected_value| {
        values
            .iter()
            .any(|value| value.as_str() == Some(*expected_value))
    })
}

fn channel_gate_passed(channels: &serde_json::Value, name: &str) -> bool {
    channels
        .get("channels")
        .and_then(|value| value.as_array())
        .is_some_and(|channels| {
            channels.iter().any(|channel| {
                json_string(channel, "name") == Some(name)
                    && json_bool(channel, "auto_apply") == Some(false)
                    && json_bool(channel, "requires_signature") == Some(true)
                    && json_bool(channel, "rollback_required") == Some(true)
            })
        })
}

fn local_update_signature(manifest_hash: &str) -> String {
    local_stable_content_hash(&format!(
        "{}:{}",
        "opensks-local-dev-trusted-signer-v1", manifest_hash
    ))
}

fn local_stable_content_hash(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

fn collect_release_artifact_digests(
    cwd: &Path,
    source_commit_sha: Option<&str>,
) -> Result<Vec<opensks_contracts::ReleaseArtifactDigest>, CliError> {
    let mut artifacts = Vec::with_capacity(RELEASE_PROOF_REQUIRED_ARTIFACTS.len());
    for (name, relative_path) in RELEASE_PROOF_REQUIRED_ARTIFACTS {
        let path = cwd.join(relative_path);
        match fs::read(&path) {
            Ok(bytes) => artifacts.push(opensks_contracts::ReleaseArtifactDigest {
                name: (*name).to_string(),
                path: (*relative_path).to_string(),
                required: true,
                present: true,
                digest: Some(sha256_bytes_v1(&bytes)),
                source_commit_sha: source_commit_sha.map(str::to_string),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                artifacts.push(opensks_contracts::ReleaseArtifactDigest {
                    name: (*name).to_string(),
                    path: (*relative_path).to_string(),
                    required: true,
                    present: false,
                    digest: None,
                    source_commit_sha: None,
                });
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(artifacts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn release_proof_binds_required_artifacts_to_clean_head() {
        let root = temp_workspace("opensks-cli-release-proof-digests");
        run_repo_git(&root, &["init"]);
        run_repo_git(&root, &["config", "user.email", "opensks@example.test"]);
        run_repo_git(&root, &["config", "user.name", "OpenSKS Test"]);
        fs::write(root.join("doc.txt"), "baseline\n").expect("write baseline");
        run_repo_git(&root, &["add", "doc.txt"]);
        run_repo_git(&root, &["commit", "-m", "initial"]);
        for (_, relative_path) in RELEASE_PROOF_REQUIRED_ARTIFACTS {
            let path = root.join(relative_path);
            fs::create_dir_all(path.parent().expect("artifact parent")).expect("artifact dir");
            fs::write(
                &path,
                format!("release artifact fixture: {relative_path}\n"),
            )
            .expect("write release artifact");
        }
        run_repo_git(&root, &["add", "."]);
        run_repo_git(&root, &["commit", "-m", "release artifacts"]);
        write_verified_updater_artifacts(&root);

        let release = run_release_command(&["proof".to_string()], &root).expect("release proof");
        assert!(release.stdout.contains("artifact_digest_gate_passed: true"));
        assert!(release.stdout.contains("same_sha_artifact_binding: true"));
        assert!(release.stdout.contains("blocker: signed_app_missing - "));
        assert!(release.stdout.contains("blocker: notarization_missing - "));
        assert!(!release.stdout.contains("blocker: upgrade_unverified - "));
        let release_proof = read_json(
            &root
                .join(".opensks")
                .join("release")
                .join("release-proof.json"),
        );
        assert_eq!(release_proof["status"], "not_verified");
        assert_eq!(release_proof["artifact_digest_gate_passed"], true);
        assert_eq!(release_proof["same_sha_artifact_binding"], true);
        assert_eq!(release_proof["signing_evidence"]["checked"], false);
        assert_eq!(
            release_proof["signing_evidence"]["app_bundle_path"],
            ".opensks/macos/OpenSKS.app"
        );
        assert_eq!(
            release_proof["signing_evidence"]["diagnostic"],
            "app bundle candidate is missing"
        );
        assert_eq!(
            release_proof["artifact_digests"]
                .as_array()
                .expect("artifact digests")
                .len(),
            RELEASE_PROOF_REQUIRED_ARTIFACTS.len()
        );
        assert!(
            release_proof["missing_artifacts"]
                .as_array()
                .expect("missing artifacts")
                .is_empty()
        );
        let blockers = release_proof["blockers"]
            .as_array()
            .expect("release blockers");
        assert_eq!(release_proof["upgrade_checked"], true);
        assert_eq!(blockers.len(), 2);
        assert!(
            blockers
                .iter()
                .any(|blocker| blocker["code"] == "signed_app_missing")
        );
        assert!(
            blockers
                .iter()
                .any(|blocker| blocker["code"] == "notarization_missing")
        );
        assert!(
            !blockers
                .iter()
                .any(|blocker| blocker["code"] == "upgrade_unverified")
        );
        let actions = release_proof["remediation_actions"]
            .as_array()
            .expect("release remediation actions");
        assert_eq!(actions.len(), blockers.len());
        assert!(actions.iter().any(|action| {
            action["blocker"] == "signed_app_missing"
                && action["scope"] == "release_signing"
                && action["action"]
                    .as_str()
                    .expect("signed action")
                    .contains("Developer ID Application")
        }));
        assert!(actions.iter().any(|action| {
            action["blocker"] == "notarization_missing"
                && action["scope"] == "release_signing"
                && action["action"]
                    .as_str()
                    .expect("notarization action")
                    .contains("Apple notarization")
        }));
        let source_commit = release_proof["source_commit_sha"]
            .as_str()
            .expect("source commit");
        assert_eq!(source_commit.len(), 40);
        for artifact in release_proof["artifact_digests"]
            .as_array()
            .expect("artifact digests")
        {
            assert_eq!(artifact["present"], true);
            assert!(
                artifact["digest"]
                    .as_str()
                    .expect("digest")
                    .starts_with("sha256:v1:")
            );
            assert_eq!(artifact["source_commit_sha"], source_commit);
        }

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn release_proof_dirty_workspace_blocker_names_tracked_path_samples() {
        let root = temp_workspace("opensks-cli-release-proof-dirty-samples");
        run_repo_git(&root, &["init"]);
        run_repo_git(&root, &["config", "user.email", "opensks@example.test"]);
        run_repo_git(&root, &["config", "user.name", "OpenSKS Test"]);
        for (_, relative_path) in RELEASE_PROOF_REQUIRED_ARTIFACTS {
            let path = root.join(relative_path);
            fs::create_dir_all(path.parent().expect("artifact parent")).expect("artifact dir");
            fs::write(
                &path,
                format!("release artifact fixture: {relative_path}\n"),
            )
            .expect("write release artifact");
        }
        run_repo_git(&root, &["add", "."]);
        run_repo_git(&root, &["commit", "-m", "release artifacts"]);
        fs::write(
            root.join("docs").join("runtime-truth-matrix.generated.md"),
            "dirty release artifact fixture\n",
        )
        .expect("dirty release artifact");

        let release = run_release_command(&["proof".to_string()], &root).expect("release proof");
        assert!(release.stdout.contains("blockers: 4"));
        assert!(release.stdout.contains("blocker: workspace_dirty - "));
        assert!(release.stdout.contains("blocker: signed_app_missing - "));
        assert!(release.stdout.contains("blocker: notarization_missing - "));
        assert!(release.stdout.contains("blocker: upgrade_unverified - "));
        assert!(
            release
                .stdout
                .contains("docs/runtime-truth-matrix.generated.md")
        );
        let release_proof = read_json(
            &root
                .join(".opensks")
                .join("release")
                .join("release-proof.json"),
        );
        let blockers = release_proof["blockers"]
            .as_array()
            .expect("release blockers");
        assert_eq!(blockers.len(), 4);
        let workspace_dirty = blockers
            .iter()
            .find(|blocker| blocker["code"] == "workspace_dirty")
            .expect("workspace dirty blocker");
        let message = workspace_dirty["message"]
            .as_str()
            .expect("blocker message");
        assert!(message.contains("1 tracked paths"));
        assert!(message.contains("docs/runtime-truth-matrix.generated.md"));

        fs::remove_dir_all(root).ok();
    }

    fn temp_workspace(label: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("{label}-{stamp}-{}", process::id()));
        fs::create_dir_all(&root).expect("temp workspace");
        root
    }

    fn run_repo_git(root: &Path, args: &[&str]) {
        let status = process::Command::new("git")
            .args(args)
            .current_dir(root)
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn read_json(path: &Path) -> serde_json::Value {
        let text = fs::read_to_string(path).expect("json artifact");
        assert!(text.ends_with('\n'));
        serde_json::from_str(&text).expect("valid json")
    }

    fn write_verified_updater_artifacts(root: &Path) {
        let updater_dir = root.join(".opensks").join("updater");
        fs::create_dir_all(&updater_dir).expect("updater dir");
        let manifest = concat!(
            "{\n",
            "  \"schema\": \"opensks.update-manifest.v1\",\n",
            "  \"current_version\": \"0.1.0\",\n",
            "  \"channels\": [\"stable\",\"latest\"],\n",
            "  \"default_channel\": \"stable\",\n",
            "  \"artifacts\": [\"opensks-cli\",\"app-bundle-candidate\",\"manifest\"],\n",
            "  \"requires_signature\": true,\n",
            "  \"requires_rollback_plan\": true,\n",
            "  \"network_install_enabled\": false\n",
            "}\n"
        );
        let manifest_hash = local_stable_content_hash(manifest);
        let signature = local_update_signature(&manifest_hash);
        fs::write(updater_dir.join("update-manifest.json"), manifest).expect("manifest");
        fs::write(
            updater_dir.join("update-signature.json"),
            format!(
                concat!(
                    "{{\n",
                    "  \"schema\": \"opensks.update-signature.v1\",\n",
                    "  \"manifest_hash\": \"{}\",\n",
                    "  \"trusted_signer_fingerprint\": \"opensks-local-dev-trusted-signer-v1\",\n",
                    "  \"signature\": \"{}\",\n",
                    "  \"algorithm\": \"fnv1a64-local-dev-proof-not-production-crypto\",\n",
                    "  \"production_crypto_live\": false\n",
                    "}}\n"
                ),
                manifest_hash, signature
            ),
        )
        .expect("signature");
        fs::write(
            updater_dir.join("update-channels.json"),
            concat!(
                "{\n",
                "  \"schema\": \"opensks.update-channels.v1\",\n",
                "  \"channels\": [\n",
                "    {\"name\":\"stable\",\"auto_apply\":false,\"requires_signature\":true,\"rollback_required\":true},\n",
                "    {\"name\":\"latest\",\"auto_apply\":false,\"requires_signature\":true,\"rollback_required\":true}\n",
                "  ]\n",
                "}\n"
            ),
        )
        .expect("channels");
        fs::write(
            updater_dir.join("rollback-plan.json"),
            concat!(
                "{\n",
                "  \"schema\": \"opensks.rollback-plan.v1\",\n",
                "  \"current_version\": \"0.1.0\",\n",
                "  \"rollback_slots\": [\"previous-stable\",\"previous-latest\",\"manual-operator-restore\"],\n",
                "  \"restore_strategy\": \"preserve_previous_manifest_and_binary_before_apply\",\n",
                "  \"apply_transaction_live\": false\n",
                "}\n"
            ),
        )
        .expect("rollback");
        fs::write(
            updater_dir.join("update-boundary.json"),
            concat!(
                "{\n",
                "  \"schema\": \"opensks.update-boundary.v1\",\n",
                "  \"auto_download\": false,\n",
                "  \"auto_apply\": false,\n",
                "  \"requires_operator_approval\": true,\n",
                "  \"requires_verified_signature\": true,\n",
                "  \"requires_rollback_plan\": true,\n",
                "  \"signed_updater_live\": \"local_manifest_signature_artifact_only\"\n",
                "}\n"
            ),
        )
        .expect("boundary");
        fs::write(
            updater_dir.join("updater-final-state.json"),
            format!(
                concat!(
                    "{{\n",
                    "  \"schema\": \"opensks.updater-final-state.v1\",\n",
                    "  \"status\": \"verified_artifact_plan\",\n",
                    "  \"manifest_hash\": \"{}\",\n",
                    "  \"signature_verified\": true,\n",
                    "  \"channels_present\": [\"stable\",\"latest\"],\n",
                    "  \"rollback_plan_present\": true,\n",
                    "  \"network_or_install_performed\": false\n",
                    "}}\n"
                ),
                manifest_hash
            ),
        )
        .expect("final state");
    }
}
