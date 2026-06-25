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
    let proof = opensks_retention::release_proof_with_artifacts(
        "0.1.0",
        false,
        false,
        true,
        true,
        false,
        source_commit_sha,
        workspace_dirty,
        artifact_digests,
        blockers,
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

        let release = run_release_command(&["proof".to_string()], &root).expect("release proof");
        assert!(release.stdout.contains("artifact_digest_gate_passed: true"));
        assert!(release.stdout.contains("same_sha_artifact_binding: true"));
        let release_proof = read_json(
            &root
                .join(".opensks")
                .join("release")
                .join("release-proof.json"),
        );
        assert_eq!(release_proof["status"], "not_verified");
        assert_eq!(release_proof["artifact_digest_gate_passed"], true);
        assert_eq!(release_proof["same_sha_artifact_binding"], true);
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
        assert!(
            release_proof["blockers"]
                .as_array()
                .expect("release blockers")
                .is_empty()
        );
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
        assert!(release.stdout.contains("blockers: 1"));
        assert!(release.stdout.contains("blocker: workspace_dirty - "));
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
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0]["code"], "workspace_dirty");
        let message = blockers[0]["message"].as_str().expect("blocker message");
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
}
