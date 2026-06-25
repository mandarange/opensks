use std::collections::HashMap;
use std::fs;
use std::path::Path;

const OPEN_SKSDIR: &str = ".opensks";

#[derive(Debug, Clone)]
pub struct NativeCollaborationEvidence {
    available: bool,
    native_agent_provenance_verified: bool,
    mission_id: String,
    agent_session_ref: String,
    agent_session_hash: String,
    agent_consensus_ref: String,
    agent_consensus_hash: String,
    agent_proof_evidence_ref: String,
    agent_proof_evidence_hash: String,
    parallel_runtime_proof_ref: String,
    parallel_runtime_proof_hash: String,
    native_cli_session_proof_ref: String,
    native_cli_session_proof_hash: String,
    native_session_proof_kind: String,
    session_count: usize,
    completed_session_count: usize,
    worker_lane_count: usize,
    reviewer_lane_count: usize,
    mapper_lane_count: usize,
    roles: Vec<String>,
    status: String,
    reason: String,
}

#[derive(Debug, Clone, Copy)]
struct NativeCollaborationEventExpectations<'a> {
    source_mission_id: &'a str,
    native_session_count: usize,
    completed_session_count: usize,
    worker_lane_count: usize,
    reviewer_lane_count: usize,
    mapper_lane_count: usize,
    agent_consensus_ref: &'a str,
    agent_consensus_hash: &'a str,
}

pub fn discover_native_collaboration_evidence(cwd: &Path) -> NativeCollaborationEvidence {
    let unavailable = |reason: &str| NativeCollaborationEvidence {
        available: false,
        native_agent_provenance_verified: false,
        mission_id: String::new(),
        agent_session_ref: String::new(),
        agent_session_hash: String::new(),
        agent_consensus_ref: String::new(),
        agent_consensus_hash: String::new(),
        agent_proof_evidence_ref: String::new(),
        agent_proof_evidence_hash: String::new(),
        parallel_runtime_proof_ref: String::new(),
        parallel_runtime_proof_hash: String::new(),
        native_cli_session_proof_ref: String::new(),
        native_cli_session_proof_hash: String::new(),
        native_session_proof_kind: String::new(),
        session_count: 0,
        completed_session_count: 0,
        worker_lane_count: 0,
        reviewer_lane_count: 0,
        mapper_lane_count: 0,
        roles: Vec::new(),
        status: "native_session_evidence_missing".to_string(),
        reason: reason.to_string(),
    };

    let missions_dir = cwd.join(".sneakoscope").join("missions");
    let Ok(entries) = fs::read_dir(&missions_dir) else {
        return unavailable(".sneakoscope/missions is missing");
    };
    let mut mission_dirs = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    mission_dirs.sort();

    for mission_dir in mission_dirs.into_iter().rev() {
        let Some(mission_id) = mission_dir
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let agents_dir = mission_dir.join("agents");
        let sessions_path = agents_dir.join("agent-sessions.json");
        let consensus_path = agents_dir.join("agent-consensus.json");
        let agent_proof_path = agents_dir.join("agent-proof-evidence.json");
        let parallel_runtime_path = agents_dir.join("parallel-runtime-proof.json");
        let native_cli_proof_path = agents_dir.join("native-cli-session-proof.json");
        let codex_app_proof_path = agents_dir.join("codex-app-agent-session-proof.json");
        let Ok(sessions) = fs::read_to_string(&sessions_path) else {
            continue;
        };
        let Ok(consensus) = fs::read_to_string(&consensus_path) else {
            continue;
        };
        let Some((
            session_count,
            completed_session_count,
            worker_count,
            reviewer_count,
            mapper_count,
            roles,
        )) = native_agent_sessions_summary(&sessions, &mission_id)
        else {
            continue;
        };
        if !native_agent_consensus_valid(&consensus, &mission_id) {
            continue;
        }

        let agent_session_ref =
            format!(".sneakoscope/missions/{mission_id}/agents/agent-sessions.json");
        let agent_session_hash = stable_content_hash(&sessions);
        let agent_consensus_ref =
            format!(".sneakoscope/missions/{mission_id}/agents/agent-consensus.json");
        let agent_consensus_hash = stable_content_hash(&consensus);
        let agent_proof_evidence_ref =
            format!(".sneakoscope/missions/{mission_id}/agents/agent-proof-evidence.json");
        let parallel_runtime_proof_ref =
            format!(".sneakoscope/missions/{mission_id}/agents/parallel-runtime-proof.json");
        let native_cli_session_proof_ref =
            format!(".sneakoscope/missions/{mission_id}/agents/native-cli-session-proof.json");
        let codex_app_session_proof_ref =
            format!(".sneakoscope/missions/{mission_id}/agents/codex-app-agent-session-proof.json");

        let (
            native_agent_provenance_verified,
            agent_proof_evidence_hash,
            parallel_runtime_proof_hash,
            native_cli_session_proof_hash,
            selected_native_session_proof_ref,
        ) = if let (Ok(agent_proof), Ok(parallel_runtime)) = (
            fs::read_to_string(&agent_proof_path),
            fs::read_to_string(&parallel_runtime_path),
        ) {
            let agent_proof_evidence_hash = stable_content_hash(&agent_proof);
            let parallel_runtime_proof_hash = stable_content_hash(&parallel_runtime);
            let proof_candidates = [
                (&native_cli_proof_path, &native_cli_session_proof_ref),
                (&codex_app_proof_path, &codex_app_session_proof_ref),
            ];
            let mut first_hash = String::new();
            let mut first_ref = String::new();
            let mut verified = false;
            let mut verified_hash = String::new();
            let mut verified_ref = String::new();
            for (proof_path, proof_ref) in proof_candidates {
                let Ok(session_proof) = fs::read_to_string(proof_path) else {
                    continue;
                };
                let session_proof_hash = stable_content_hash(&session_proof);
                if first_hash.is_empty() {
                    first_hash = session_proof_hash.clone();
                    first_ref = proof_ref.clone();
                }
                let Some(session_proof_filename) = proof_ref
                    .rsplit('/')
                    .next()
                    .filter(|value| !value.is_empty())
                else {
                    continue;
                };
                let proof_expectations = NativeProvenanceProofExpectations {
                    mission_id: &mission_id,
                    agent_session_ref: &agent_session_ref,
                    agent_session_hash: &agent_session_hash,
                    agent_consensus_ref: &agent_consensus_ref,
                    agent_consensus_hash: &agent_consensus_hash,
                    agent_proof_evidence_ref: &agent_proof_evidence_ref,
                    agent_proof_evidence_hash: &agent_proof_evidence_hash,
                    parallel_runtime_proof_ref: &parallel_runtime_proof_ref,
                    parallel_runtime_proof_hash: &parallel_runtime_proof_hash,
                    native_cli_session_proof_ref: proof_ref,
                    native_session_proof_filename: session_proof_filename,
                    session_count,
                    completed_session_count,
                    worker_lane_count: worker_count,
                    reviewer_lane_count: reviewer_count,
                    mapper_lane_count: mapper_count,
                };
                if native_agent_provenance_valid(
                    &agent_proof,
                    &parallel_runtime,
                    &session_proof,
                    proof_expectations,
                ) {
                    verified = true;
                    verified_hash = session_proof_hash;
                    verified_ref = proof_ref.clone();
                    break;
                }
            }
            (
                verified,
                agent_proof_evidence_hash,
                parallel_runtime_proof_hash,
                if verified { verified_hash } else { first_hash },
                if verified { verified_ref } else { first_ref },
            )
        } else {
            (
                false,
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            )
        };

        let role_lane_count = [worker_count, reviewer_count, mapper_count]
            .into_iter()
            .filter(|count| *count > 0)
            .count();
        if session_count < 2 || completed_session_count < 2 || role_lane_count < 2 {
            continue;
        }

        let native_session_proof_kind =
            if selected_native_session_proof_ref.ends_with("codex-app-agent-session-proof.json") {
                "codex_app_multi_agent_v1".to_string()
            } else if selected_native_session_proof_ref.ends_with("native-cli-session-proof.json") {
                "native_cli_session".to_string()
            } else {
                String::new()
            };

        return NativeCollaborationEvidence {
            available: true,
            native_agent_provenance_verified,
            mission_id: mission_id.clone(),
            agent_session_ref,
            agent_session_hash,
            agent_consensus_ref,
            agent_consensus_hash,
            agent_proof_evidence_ref: if agent_proof_evidence_hash.is_empty() {
                String::new()
            } else {
                agent_proof_evidence_ref
            },
            agent_proof_evidence_hash,
            parallel_runtime_proof_ref: if parallel_runtime_proof_hash.is_empty() {
                String::new()
            } else {
                parallel_runtime_proof_ref
            },
            parallel_runtime_proof_hash,
            native_cli_session_proof_ref: if native_cli_session_proof_hash.is_empty() {
                String::new()
            } else {
                selected_native_session_proof_ref
            },
            native_cli_session_proof_hash,
            native_session_proof_kind,
            session_count,
            completed_session_count,
            worker_lane_count: worker_count,
            reviewer_lane_count: reviewer_count,
            mapper_lane_count: mapper_count,
            roles,
            status: "native_multi_session_collaboration_recorded".to_string(),
            reason: "native agent session and consensus artifacts prove multi-session collaboration; live remote multi-provider worker collaboration is not claimed".to_string(),
        };
    }

    unavailable("no valid native agent session plus consensus evidence found")
}

#[derive(Debug, Clone, Copy)]
struct NativeProvenanceProofExpectations<'a> {
    mission_id: &'a str,
    agent_session_ref: &'a str,
    agent_session_hash: &'a str,
    agent_consensus_ref: &'a str,
    agent_consensus_hash: &'a str,
    agent_proof_evidence_ref: &'a str,
    agent_proof_evidence_hash: &'a str,
    parallel_runtime_proof_ref: &'a str,
    parallel_runtime_proof_hash: &'a str,
    native_cli_session_proof_ref: &'a str,
    native_session_proof_filename: &'a str,
    session_count: usize,
    completed_session_count: usize,
    worker_lane_count: usize,
    reviewer_lane_count: usize,
    mapper_lane_count: usize,
}

fn native_agent_sessions_summary(
    sessions: &str,
    mission_id: &str,
) -> Option<(usize, usize, usize, usize, usize, Vec<String>)> {
    if !json_top_level_string_field_equals(sessions, "schema", "sks.agent-sessions.v1")
        || !json_top_level_string_field_equals(sessions, "mission_id", mission_id)
        || !json_top_level_bool_field_equals(sessions, "native_sessions_required", true)
    {
        return None;
    }
    let mut session_rows = extract_json_top_level_array_objects(sessions, "sessions");
    if session_rows.is_empty() {
        session_rows = extract_json_top_level_object_values(sessions, "sessions");
    }
    if session_rows.is_empty() {
        return None;
    }
    let mut completed = 0usize;
    let mut worker_count = 0usize;
    let mut reviewer_count = 0usize;
    let mut mapper_count = 0usize;
    let mut roles = Vec::new();
    for row in &session_rows {
        let role = extract_json_top_level_string_field(row, "role")?;
        let status = extract_json_top_level_string_field(row, "status")?;
        if status.starts_with("completed") {
            completed += 1;
        }
        if !roles.iter().any(|existing| existing == &role) {
            roles.push(role.clone());
        }
        match role.as_str() {
            "implementation_worker" | "worker" | "sks-implementer" => worker_count += 1,
            "qa_reviewer" | "reviewer" | "sks-release-verifier" => reviewer_count += 1,
            "native_agent" | "analysis_scout" | "explorer" | "sks-explorer" => mapper_count += 1,
            _ => {}
        }
    }
    roles.sort();
    Some((
        session_rows.len(),
        completed,
        worker_count,
        reviewer_count,
        mapper_count,
        roles,
    ))
}

fn native_agent_consensus_valid(consensus: &str, mission_id: &str) -> bool {
    json_top_level_string_field_equals(consensus, "schema", "sks.agent-consensus.v1")
        && json_top_level_string_field_equals(consensus, "mission_id", mission_id)
        && extract_json_top_level_string_field(consensus, "consensus")
            .is_some_and(|value| !value.trim().is_empty())
}

fn native_agent_provenance_valid(
    agent_proof: &str,
    parallel_runtime: &str,
    native_cli_proof: &str,
    expected: NativeProvenanceProofExpectations<'_>,
) -> bool {
    native_agent_proof_evidence_valid(agent_proof, expected)
        && native_parallel_runtime_proof_valid(parallel_runtime, expected)
        && native_cli_session_proof_valid(native_cli_proof, expected)
}

fn native_agent_proof_evidence_valid(
    proof: &str,
    expected: NativeProvenanceProofExpectations<'_>,
) -> bool {
    let Some(backend) = extract_json_top_level_string_field(proof, "backend") else {
        return false;
    };
    let backend = backend.trim().to_ascii_lowercase();
    let native_cli_counts_ok = json_top_level_string_field_equals(
        proof,
        "native_cli_session_proof",
        expected.native_session_proof_filename,
    ) && json_top_level_min_number_field(
        proof,
        "native_cli_worker_process_count",
        expected.session_count,
    ) && json_top_level_min_number_field(
        proof,
        "native_cli_max_observed_worker_process_count",
        expected.session_count,
    ) && json_top_level_min_number_field(
        proof,
        "native_cli_unique_worker_session_count",
        expected.session_count,
    );
    let codex_app_counts_ok =
        (json_top_level_string_field_equals(
            proof,
            "native_session_proof",
            expected.native_session_proof_filename,
        ) || json_top_level_string_field_equals(
            proof,
            "codex_app_agent_session_proof",
            expected.native_session_proof_filename,
        )) && json_top_level_min_number_field(
            proof,
            "codex_app_agent_session_count",
            expected.session_count,
        ) && json_top_level_min_number_field(
            proof,
            "codex_app_completed_agent_count",
            expected.completed_session_count,
        ) && json_top_level_min_number_field(
            proof,
            "codex_app_unique_agent_session_count",
            expected.session_count,
        ) && json_top_level_bool_field_equals(proof, "codex_app_agent_ids_hash_chain_ok", true);

    json_top_level_string_field_equals(proof, "schema", "sks.agent-proof-evidence.v1")
        && json_top_level_string_field_equals(proof, "mission_id", expected.mission_id)
        && json_top_level_bool_field_equals(proof, "ok", true)
        && json_top_level_string_field_equals(proof, "status", "passed")
        && !backend.is_empty()
        && !backend.contains("fake")
        && !backend.contains("mock")
        && json_top_level_field_absent(proof, "fake_backend_disclaimer")
        && json_top_level_string_field_equals(proof, "route_blackbox_kind", "actual_agent_command")
        && json_top_level_bool_field_equals(proof, "real_route_command_used", true)
        && json_top_level_bool_field_equals(proof, "real_parallel_claim", true)
        && (native_cli_counts_ok || codex_app_counts_ok)
        && json_top_level_string_field_equals(
            proof,
            "agent_session_ref",
            expected.agent_session_ref,
        )
        && json_top_level_string_field_equals(
            proof,
            "agent_session_hash",
            expected.agent_session_hash,
        )
        && json_top_level_string_field_equals(
            proof,
            "agent_consensus_ref",
            expected.agent_consensus_ref,
        )
        && json_top_level_string_field_equals(
            proof,
            "agent_consensus_hash",
            expected.agent_consensus_hash,
        )
        && json_top_level_string_field_equals(
            proof,
            "parallel_runtime_proof_ref",
            expected.parallel_runtime_proof_ref,
        )
        && json_top_level_string_field_equals(
            proof,
            "parallel_runtime_proof_hash",
            expected.parallel_runtime_proof_hash,
        )
        && json_top_level_string_field_equals(
            proof,
            "native_cli_session_proof_ref",
            expected.native_cli_session_proof_ref,
        )
        && json_top_level_bool_field_equals(proof, "all_sessions_closed", true)
        && json_top_level_bool_field_equals(proof, "terminal_sessions_closed", true)
        && json_top_level_bool_field_equals(proof, "ledger_hash_chain_ok", true)
        && json_top_level_bool_field_equals(proof, "consensus_ok", true)
        && json_top_level_empty_array_field_equals(proof, "blockers")
}

fn native_parallel_runtime_proof_valid(
    proof: &str,
    expected: NativeProvenanceProofExpectations<'_>,
) -> bool {
    let Some(proof_mode) = extract_json_top_level_string_field(proof, "proof_mode") else {
        return false;
    };
    let proof_mode = proof_mode.trim().to_ascii_lowercase();
    let native_process_runtime_ok =
        json_top_level_bool_field_equals(proof, "require_worker_pids", true)
            && json_top_level_min_number_field(
                proof,
                "max_observed_worker_processes",
                expected.session_count,
            )
            && json_top_level_min_number_field(proof, "unique_worker_pids", expected.session_count)
            && json_top_level_min_number_field(proof, "unique_model_call_ids", 1)
            && json_top_level_min_number_field(proof, "max_observed_model_calls", 1);
    let codex_app_runtime_ok =
        json_top_level_bool_field_equals(proof, "codex_app_multi_agent_sessions", true)
            && json_top_level_min_number_field(
                proof,
                "max_observed_agent_sessions",
                expected.session_count,
            )
            && json_top_level_min_number_field(
                proof,
                "unique_agent_session_ids",
                expected.session_count,
            )
            && json_top_level_min_number_field(
                proof,
                "completed_agent_sessions",
                expected.completed_session_count,
            );

    json_top_level_string_field_equals(proof, "schema", "sks.parallel-runtime-proof.v1")
        && json_top_level_string_field_equals(proof, "mission_id", expected.mission_id)
        && json_top_level_bool_field_equals(proof, "passed", true)
        && !proof_mode.contains("fake")
        && !proof_mode.contains("mock")
        && json_top_level_min_number_field(proof, "requested_workers", expected.session_count)
        && (native_process_runtime_ok || codex_app_runtime_ok)
        && extract_json_top_level_raw_field(proof, "utilization_proof_consistency")
            .is_some_and(|raw| json_top_level_bool_field_equals(&raw, "ok", true))
        && json_top_level_empty_array_field_equals(proof, "blockers")
}

fn native_cli_session_proof_valid(
    proof: &str,
    expected: NativeProvenanceProofExpectations<'_>,
) -> bool {
    if json_top_level_string_field_equals(proof, "schema", "sks.codex-app-agent-session-proof.v1") {
        return codex_app_agent_session_proof_valid(proof, expected);
    }

    let Some(backend) = extract_json_top_level_string_field(proof, "backend") else {
        return false;
    };
    let Some(proof_mode) = extract_json_top_level_string_field(proof, "proof_mode") else {
        return false;
    };
    let backend = backend.trim().to_ascii_lowercase();
    let proof_mode = proof_mode.trim().to_ascii_lowercase();
    let role_lane_count = [
        expected.worker_lane_count,
        expected.reviewer_lane_count,
        expected.mapper_lane_count,
    ]
    .into_iter()
    .filter(|count| *count > 0)
    .count();
    let exact_session_counts_match =
        json_top_level_number_field_equals(proof, "native_worker_count", expected.session_count)
            && json_top_level_number_field_equals(
                proof,
                "completed_native_worker_count",
                expected.completed_session_count,
            )
            && json_top_level_number_field_equals(
                proof,
                "worker_lane_count",
                expected.worker_lane_count,
            )
            && json_top_level_number_field_equals(
                proof,
                "reviewer_lane_count",
                expected.reviewer_lane_count,
            )
            && json_top_level_number_field_equals(
                proof,
                "mapper_lane_count",
                expected.mapper_lane_count,
            );
    let process_session_counts_match =
        json_top_level_min_array_length(proof, "process_ids", expected.session_count)
            && json_top_level_min_number_field(
                proof,
                "unique_worker_session_count",
                expected.session_count,
            );

    json_top_level_string_field_equals(proof, "schema", "sks.native-cli-session-proof.v1")
        && json_top_level_string_field_equals(proof, "mission_id", expected.mission_id)
        && !backend.is_empty()
        && !backend.contains("fake")
        && !backend.contains("mock")
        && json_top_level_field_absent(proof, "fake_backend_disclaimer")
        && !proof_mode.contains("fake")
        && !proof_mode.contains("mock")
        && json_top_level_bool_field_equals(proof, "ok", true)
        && json_top_level_bool_field_equals(proof, "real_parallel_claim", true)
        && json_top_level_bool_field_equals(proof, "native_cli_session_proof", true)
        && json_top_level_string_field_equals(
            proof,
            "agent_session_ref",
            expected.agent_session_ref,
        )
        && json_top_level_string_field_equals(
            proof,
            "agent_session_hash",
            expected.agent_session_hash,
        )
        && json_top_level_string_field_equals(
            proof,
            "agent_consensus_ref",
            expected.agent_consensus_ref,
        )
        && json_top_level_string_field_equals(
            proof,
            "agent_consensus_hash",
            expected.agent_consensus_hash,
        )
        && json_top_level_string_field_equals(
            proof,
            "agent_proof_evidence_ref",
            expected.agent_proof_evidence_ref,
        )
        && json_top_level_string_field_equals(
            proof,
            "agent_proof_evidence_hash",
            expected.agent_proof_evidence_hash,
        )
        && json_top_level_string_field_equals(
            proof,
            "parallel_runtime_proof_ref",
            expected.parallel_runtime_proof_ref,
        )
        && json_top_level_string_field_equals(
            proof,
            "parallel_runtime_proof_hash",
            expected.parallel_runtime_proof_hash,
        )
        && (exact_session_counts_match || process_session_counts_match)
        && json_top_level_empty_array_field_equals(proof, "blockers")
        && expected.session_count >= 2
        && expected.completed_session_count >= 2
        && role_lane_count >= 2
}

fn codex_app_agent_session_proof_valid(
    proof: &str,
    expected: NativeProvenanceProofExpectations<'_>,
) -> bool {
    let Some(backend) = extract_json_top_level_string_field(proof, "backend") else {
        return false;
    };
    let Some(proof_mode) = extract_json_top_level_string_field(proof, "proof_mode") else {
        return false;
    };
    let backend = backend.trim().to_ascii_lowercase();
    let proof_mode = proof_mode.trim().to_ascii_lowercase();
    let role_lane_count = [
        expected.worker_lane_count,
        expected.reviewer_lane_count,
        expected.mapper_lane_count,
    ]
    .into_iter()
    .filter(|count| *count > 0)
    .count();

    json_top_level_string_field_equals(proof, "mission_id", expected.mission_id)
        && !backend.is_empty()
        && !backend.contains("fake")
        && !backend.contains("mock")
        && !proof_mode.contains("fake")
        && !proof_mode.contains("mock")
        && json_top_level_field_absent(proof, "fake_backend_disclaimer")
        && json_top_level_bool_field_equals(proof, "ok", true)
        && json_top_level_bool_field_equals(proof, "real_parallel_claim", true)
        && json_top_level_bool_field_equals(proof, "codex_app_agent_session_proof", true)
        && json_top_level_string_field_equals(
            proof,
            "agent_session_ref",
            expected.agent_session_ref,
        )
        && json_top_level_string_field_equals(
            proof,
            "agent_session_hash",
            expected.agent_session_hash,
        )
        && json_top_level_string_field_equals(
            proof,
            "agent_consensus_ref",
            expected.agent_consensus_ref,
        )
        && json_top_level_string_field_equals(
            proof,
            "agent_consensus_hash",
            expected.agent_consensus_hash,
        )
        && json_top_level_string_field_equals(
            proof,
            "agent_proof_evidence_ref",
            expected.agent_proof_evidence_ref,
        )
        && json_top_level_string_field_equals(
            proof,
            "agent_proof_evidence_hash",
            expected.agent_proof_evidence_hash,
        )
        && json_top_level_string_field_equals(
            proof,
            "parallel_runtime_proof_ref",
            expected.parallel_runtime_proof_ref,
        )
        && json_top_level_string_field_equals(
            proof,
            "parallel_runtime_proof_hash",
            expected.parallel_runtime_proof_hash,
        )
        && json_top_level_number_field_equals(
            proof,
            "codex_app_agent_session_count",
            expected.session_count,
        )
        && json_top_level_number_field_equals(
            proof,
            "codex_app_completed_agent_count",
            expected.completed_session_count,
        )
        && json_top_level_number_field_equals(
            proof,
            "worker_lane_count",
            expected.worker_lane_count,
        )
        && json_top_level_number_field_equals(
            proof,
            "reviewer_lane_count",
            expected.reviewer_lane_count,
        )
        && json_top_level_number_field_equals(
            proof,
            "mapper_lane_count",
            expected.mapper_lane_count,
        )
        && json_top_level_min_array_length(proof, "agent_ids", expected.session_count)
        && json_top_level_bool_field_equals(proof, "agent_ids_hash_chain_ok", true)
        && json_top_level_bool_field_equals(proof, "all_sessions_closed", true)
        && json_top_level_empty_array_field_equals(proof, "blockers")
        && expected.session_count >= 2
        && expected.completed_session_count >= 2
        && role_lane_count >= 2
}

pub fn render_native_collaboration_execution(
    generated_at_json: &str,
    evidence: &NativeCollaborationEvidence,
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.native-collaboration-execution.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"scope\": \"native_multi_session_llm_collaboration\",\n",
            "  \"status\": {},\n",
            "  \"native_multi_session_llm_collaboration\": {},\n",
            "  \"native_agent_provenance_verified\": {},\n",
            "  \"native_session_count\": {},\n",
            "  \"completed_session_count\": {},\n",
            "  \"worker_lane_count\": {},\n",
            "  \"reviewer_lane_count\": {},\n",
            "  \"mapper_lane_count\": {},\n",
            "  \"roles\": {},\n",
            "  \"source_mission_id\": {},\n",
            "  \"agent_session_ref\": {},\n",
            "  \"agent_session_hash\": {},\n",
            "  \"agent_consensus_ref\": {},\n",
            "  \"agent_consensus_hash\": {},\n",
            "  \"agent_proof_evidence_ref\": {},\n",
            "  \"agent_proof_evidence_hash\": {},\n",
            "  \"parallel_runtime_proof_ref\": {},\n",
            "  \"parallel_runtime_proof_hash\": {},\n",
            "  \"native_cli_session_proof_ref\": {},\n",
            "  \"native_cli_session_proof_hash\": {},\n",
            "  \"provenance_proof_kind\": {},\n",
            "  \"codex_app_agent_session_proof_ref\": {},\n",
            "  \"codex_app_agent_session_proof_hash\": {},\n",
            "  \"no_hidden_fallback\": true,\n",
            "  \"live_multi_provider_worker_collaboration\": false,\n",
            "  \"live_remote_provider_api_calls\": false,\n",
            "  \"provider_credentials_required\": false,\n",
            "  \"final_apply_executed\": false,\n",
            "  \"secret_value_exposed\": false,\n",
            "  \"reason\": {}\n",
            "}}\n"
        ),
        generated_at_json,
        json_string(&evidence.status),
        evidence.available,
        evidence.native_agent_provenance_verified,
        evidence.session_count,
        evidence.completed_session_count,
        evidence.worker_lane_count,
        evidence.reviewer_lane_count,
        evidence.mapper_lane_count,
        json_vec(&evidence.roles),
        if evidence.mission_id.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.mission_id)
        },
        if evidence.agent_session_ref.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.agent_session_ref)
        },
        if evidence.agent_session_hash.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.agent_session_hash)
        },
        if evidence.agent_consensus_ref.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.agent_consensus_ref)
        },
        if evidence.agent_consensus_hash.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.agent_consensus_hash)
        },
        if evidence.agent_proof_evidence_ref.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.agent_proof_evidence_ref)
        },
        if evidence.agent_proof_evidence_hash.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.agent_proof_evidence_hash)
        },
        if evidence.parallel_runtime_proof_ref.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.parallel_runtime_proof_ref)
        },
        if evidence.parallel_runtime_proof_hash.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.parallel_runtime_proof_hash)
        },
        if evidence.native_cli_session_proof_ref.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.native_cli_session_proof_ref)
        },
        if evidence.native_cli_session_proof_hash.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.native_cli_session_proof_hash)
        },
        if evidence.native_session_proof_kind.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.native_session_proof_kind)
        },
        if evidence.native_session_proof_kind == "codex_app_multi_agent_v1"
            && !evidence.native_cli_session_proof_ref.is_empty()
        {
            json_string(&evidence.native_cli_session_proof_ref)
        } else {
            "null".to_string()
        },
        if evidence.native_session_proof_kind == "codex_app_multi_agent_v1"
            && !evidence.native_cli_session_proof_hash.is_empty()
        {
            json_string(&evidence.native_cli_session_proof_hash)
        } else {
            "null".to_string()
        },
        json_string(&evidence.reason)
    )
}

pub fn render_native_collaboration_events_jsonl(
    generated_at_json: &str,
    evidence: &NativeCollaborationEvidence,
) -> String {
    if !evidence.available {
        return format!(
            "{{\"schema\":\"opensks.native-collaboration-event.v1\",\"generated_at\":{},\"event\":\"native_sessions_missing\",\"executed\":false,\"reason\":{}}}\n",
            generated_at_json,
            json_string(&evidence.reason)
        );
    }
    [
        format!(
            "{{\"schema\":\"opensks.native-collaboration-event.v1\",\"generated_at\":{},\"event\":\"native_sessions_discovered\",\"source_mission_id\":{},\"session_count\":{},\"completed_session_count\":{},\"executed\":true}}",
            generated_at_json,
            json_string(&evidence.mission_id),
            evidence.session_count,
            evidence.completed_session_count
        ),
        format!(
            "{{\"schema\":\"opensks.native-collaboration-event.v1\",\"generated_at\":{},\"event\":\"worker_lane_completed\",\"worker_lane_count\":{},\"executed\":true}}",
            generated_at_json,
            evidence.worker_lane_count
        ),
        format!(
            "{{\"schema\":\"opensks.native-collaboration-event.v1\",\"generated_at\":{},\"event\":\"review_or_mapping_lane_completed\",\"reviewer_lane_count\":{},\"mapper_lane_count\":{},\"executed\":true}}",
            generated_at_json,
            evidence.reviewer_lane_count,
            evidence.mapper_lane_count
        ),
        format!(
            "{{\"schema\":\"opensks.native-collaboration-event.v1\",\"generated_at\":{},\"event\":\"consensus_recorded\",\"agent_consensus_ref\":{},\"agent_consensus_hash\":{},\"executed\":true}}",
            generated_at_json,
            json_string(&evidence.agent_consensus_ref),
            json_string(&evidence.agent_consensus_hash)
        ),
        format!(
            "{{\"schema\":\"opensks.native-collaboration-event.v1\",\"generated_at\":{},\"event\":\"remote_provider_collaboration_not_claimed\",\"live_multi_provider_worker_collaboration\":false,\"live_remote_provider_api_calls\":false,\"executed\":true}}",
            generated_at_json
        ),
    ]
    .join("\n")
        + "\n"
}

pub fn render_native_proof_diagnostics(
    generated_at_json: &str,
    evidence: &NativeCollaborationEvidence,
) -> String {
    let proof_status = if evidence.native_agent_provenance_verified {
        "verified"
    } else if evidence.available {
        "partial_unverified"
    } else {
        "missing"
    };
    let missing_or_unverified = if evidence.native_agent_provenance_verified {
        Vec::new()
    } else if evidence.available {
        vec![
            "agent-proof-evidence.json",
            "parallel-runtime-proof.json",
            "native-cli-session-proof.json",
            "codex-app-agent-session-proof.json",
        ]
    } else {
        vec![
            "agent-sessions.json",
            "agent-consensus.json",
            "agent-proof-evidence.json",
            "parallel-runtime-proof.json",
            "native-cli-session-proof.json",
            "codex-app-agent-session-proof.json",
        ]
    };
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.native-proof-diagnostics.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"status\": {},\n",
            "  \"source_mission_id\": {},\n",
            "  \"native_sessions_available\": {},\n",
            "  \"native_agent_provenance_verified\": {},\n",
            "  \"session_count\": {},\n",
            "  \"completed_session_count\": {},\n",
            "  \"worker_lane_count\": {},\n",
            "  \"reviewer_lane_count\": {},\n",
            "  \"mapper_lane_count\": {},\n",
            "  \"agent_session_ref\": {},\n",
            "  \"agent_session_hash\": {},\n",
            "  \"agent_proof_evidence_ref\": {},\n",
            "  \"agent_proof_evidence_hash\": {},\n",
            "  \"parallel_runtime_proof_ref\": {},\n",
            "  \"parallel_runtime_proof_hash\": {},\n",
            "  \"native_cli_session_proof_ref\": {},\n",
            "  \"native_cli_session_proof_hash\": {},\n",
            "  \"provenance_proof_kind\": {},\n",
            "  \"codex_app_agent_session_proof_ref\": {},\n",
            "  \"codex_app_agent_session_proof_hash\": {},\n",
            "  \"accepted_proof_shapes\": {},\n",
            "  \"rejected_proof_markers\": {},\n",
            "  \"missing_or_unverified\": {},\n",
            "  \"reason\": {}\n",
            "}}\n"
        ),
        generated_at_json,
        json_string(proof_status),
        if evidence.mission_id.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.mission_id)
        },
        evidence.available,
        evidence.native_agent_provenance_verified,
        evidence.session_count,
        evidence.completed_session_count,
        evidence.worker_lane_count,
        evidence.reviewer_lane_count,
        evidence.mapper_lane_count,
        if evidence.agent_session_ref.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.agent_session_ref)
        },
        if evidence.agent_session_hash.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.agent_session_hash)
        },
        if evidence.agent_proof_evidence_ref.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.agent_proof_evidence_ref)
        },
        if evidence.agent_proof_evidence_hash.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.agent_proof_evidence_hash)
        },
        if evidence.parallel_runtime_proof_ref.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.parallel_runtime_proof_ref)
        },
        if evidence.parallel_runtime_proof_hash.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.parallel_runtime_proof_hash)
        },
        if evidence.native_cli_session_proof_ref.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.native_cli_session_proof_ref)
        },
        if evidence.native_cli_session_proof_hash.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.native_cli_session_proof_hash)
        },
        if evidence.native_session_proof_kind.is_empty() {
            "null".to_string()
        } else {
            json_string(&evidence.native_session_proof_kind)
        },
        if evidence.native_session_proof_kind == "codex_app_multi_agent_v1"
            && !evidence.native_cli_session_proof_ref.is_empty()
        {
            json_string(&evidence.native_cli_session_proof_ref)
        } else {
            "null".to_string()
        },
        if evidence.native_session_proof_kind == "codex_app_multi_agent_v1"
            && !evidence.native_cli_session_proof_hash.is_empty()
        {
            json_string(&evidence.native_cli_session_proof_hash)
        } else {
            "null".to_string()
        },
        json_array(&[
            "agent-sessions.sessions-array",
            "agent-sessions.sessions-object",
            "native-cli-session-proof.count-fields",
            "native-cli-session-proof.process_ids-plus-unique_worker_session_count",
            "codex-app-agent-session-proof.count-fields"
        ]),
        json_array(&[
            "backend-or-proof_mode-containing-fake",
            "backend-or-proof_mode-containing-mock",
            "fake_backend_disclaimer",
            "missing-hash-bound-proof-chain",
            "non-empty-blockers"
        ]),
        json_array(&missing_or_unverified),
        json_string(&evidence.reason)
    )
}

pub fn beta006_native_collaboration_gate_passed(cwd: &Path) -> bool {
    let bench_dir = cwd.join(OPEN_SKSDIR).join("bench");
    let Ok(roster) = fs::read_to_string(bench_dir.join("multi-llm-roster.json")) else {
        return false;
    };
    let Ok(role_assignments) = fs::read_to_string(bench_dir.join("role-assignments.json")) else {
        return false;
    };
    let Ok(disagreement) = fs::read_to_string(bench_dir.join("disagreement-report.json")) else {
        return false;
    };
    let Ok(quorum) = fs::read_to_string(bench_dir.join("quorum-report.json")) else {
        return false;
    };
    let Ok(preflight) = fs::read_to_string(bench_dir.join("collaboration-preflight.json")) else {
        return false;
    };
    let Ok(execution) = fs::read_to_string(bench_dir.join("native-collaboration-execution.json"))
    else {
        return false;
    };
    let Ok(events) = fs::read_to_string(bench_dir.join("native-collaboration-events.jsonl")) else {
        return false;
    };

    let Some(source_mission_id) =
        extract_json_top_level_string_field(&execution, "source_mission_id")
    else {
        return false;
    };
    let Some(agent_session_ref) =
        extract_json_top_level_string_field(&execution, "agent_session_ref")
    else {
        return false;
    };
    let Some(agent_session_hash) =
        extract_json_top_level_string_field(&execution, "agent_session_hash")
    else {
        return false;
    };
    let Some(agent_consensus_ref) =
        extract_json_top_level_string_field(&execution, "agent_consensus_ref")
    else {
        return false;
    };
    let Some(agent_consensus_hash) =
        extract_json_top_level_string_field(&execution, "agent_consensus_hash")
    else {
        return false;
    };
    let Some(agent_proof_evidence_ref) =
        extract_json_top_level_string_field(&execution, "agent_proof_evidence_ref")
    else {
        return false;
    };
    let Some(agent_proof_evidence_hash) =
        extract_json_top_level_string_field(&execution, "agent_proof_evidence_hash")
    else {
        return false;
    };
    let Some(parallel_runtime_proof_ref) =
        extract_json_top_level_string_field(&execution, "parallel_runtime_proof_ref")
    else {
        return false;
    };
    let Some(parallel_runtime_proof_hash) =
        extract_json_top_level_string_field(&execution, "parallel_runtime_proof_hash")
    else {
        return false;
    };
    let Some(native_cli_session_proof_ref) =
        extract_json_top_level_string_field(&execution, "native_cli_session_proof_ref")
    else {
        return false;
    };
    let Some(native_cli_session_proof_hash) =
        extract_json_top_level_string_field(&execution, "native_cli_session_proof_hash")
    else {
        return false;
    };
    let Some(native_session_count) =
        extract_json_top_level_number_field(&execution, "native_session_count")
    else {
        return false;
    };
    let Some(completed_session_count) =
        extract_json_top_level_number_field(&execution, "completed_session_count")
    else {
        return false;
    };
    let Some(worker_lane_count) =
        extract_json_top_level_number_field(&execution, "worker_lane_count")
    else {
        return false;
    };
    let Some(reviewer_lane_count) =
        extract_json_top_level_number_field(&execution, "reviewer_lane_count")
    else {
        return false;
    };
    let Some(mapper_lane_count) =
        extract_json_top_level_number_field(&execution, "mapper_lane_count")
    else {
        return false;
    };
    let role_lane_count = [worker_lane_count, reviewer_lane_count, mapper_lane_count]
        .into_iter()
        .filter(|count| *count > 0)
        .count();

    let Some(agent_sessions) =
        read_native_collaboration_source(cwd, &agent_session_ref, &agent_session_hash)
    else {
        return false;
    };
    let Some(agent_consensus) =
        read_native_collaboration_source(cwd, &agent_consensus_ref, &agent_consensus_hash)
    else {
        return false;
    };
    let Some(agent_proof_evidence) = read_native_collaboration_source(
        cwd,
        &agent_proof_evidence_ref,
        &agent_proof_evidence_hash,
    ) else {
        return false;
    };
    let Some(parallel_runtime_proof) = read_native_collaboration_source(
        cwd,
        &parallel_runtime_proof_ref,
        &parallel_runtime_proof_hash,
    ) else {
        return false;
    };
    let Some(native_cli_session_proof) = read_native_collaboration_source(
        cwd,
        &native_cli_session_proof_ref,
        &native_cli_session_proof_hash,
    ) else {
        return false;
    };
    let Some((
        source_session_count,
        source_completed_count,
        source_worker_count,
        source_reviewer_count,
        source_mapper_count,
        _source_roles,
    )) = native_agent_sessions_summary(&agent_sessions, &source_mission_id)
    else {
        return false;
    };
    if !native_agent_consensus_valid(&agent_consensus, &source_mission_id) {
        return false;
    }
    if !native_agent_provenance_valid(
        &agent_proof_evidence,
        &parallel_runtime_proof,
        &native_cli_session_proof,
        NativeProvenanceProofExpectations {
            mission_id: &source_mission_id,
            agent_session_ref: &agent_session_ref,
            agent_session_hash: &agent_session_hash,
            agent_consensus_ref: &agent_consensus_ref,
            agent_consensus_hash: &agent_consensus_hash,
            agent_proof_evidence_ref: &agent_proof_evidence_ref,
            agent_proof_evidence_hash: &agent_proof_evidence_hash,
            parallel_runtime_proof_ref: &parallel_runtime_proof_ref,
            parallel_runtime_proof_hash: &parallel_runtime_proof_hash,
            native_cli_session_proof_ref: &native_cli_session_proof_ref,
            native_session_proof_filename: native_cli_session_proof_ref
                .rsplit('/')
                .next()
                .unwrap_or(""),
            session_count: source_session_count,
            completed_session_count: source_completed_count,
            worker_lane_count: source_worker_count,
            reviewer_lane_count: source_reviewer_count,
            mapper_lane_count: source_mapper_count,
        },
    ) {
        return false;
    }

    json_top_level_string_field_equals(&roster, "schema", "opensks.multi-llm-roster.v1")
        && json_top_level_bool_field_equals(&roster, "no_hidden_fallback", true)
        && json_top_level_string_field_equals(
            &role_assignments,
            "schema",
            "opensks.role-assignments.v1",
        )
        && json_top_level_bool_field_equals(&role_assignments, "no_hidden_fallback", true)
        && json_top_level_string_field_equals(
            &disagreement,
            "schema",
            "opensks.disagreement-report.v1",
        )
        && json_top_level_bool_field_equals(&disagreement, "live_disagreements_observed", false)
        && json_top_level_string_field_equals(&quorum, "schema", "opensks.quorum-report.v1")
        && json_top_level_bool_field_equals(&quorum, "live_quorum_evaluated", false)
        && json_top_level_bool_field_equals(&quorum, "hidden_fallback_allowed", false)
        && json_top_level_string_field_equals(
            &preflight,
            "schema",
            "opensks.collaboration-preflight.v1",
        )
        && json_top_level_bool_field_equals(&preflight, "no_hidden_fallback", true)
        && json_top_level_bool_field_equals(&preflight, "preflight_ready", true)
        && json_top_level_bool_field_equals(&preflight, "live_multi_llm_execution", false)
        && json_top_level_bool_field_equals(
            &preflight,
            "live_multi_provider_worker_collaboration",
            false,
        )
        && json_top_level_bool_field_equals(&preflight, "live_execution_ready", false)
        && !preflight.contains("\"secret_value_exposed\":true")
        && json_top_level_string_field_equals(
            &execution,
            "schema",
            "opensks.native-collaboration-execution.v1",
        )
        && json_top_level_string_field_equals(
            &execution,
            "scope",
            "native_multi_session_llm_collaboration",
        )
        && json_top_level_string_field_equals(
            &execution,
            "status",
            "native_multi_session_collaboration_recorded",
        )
        && json_top_level_bool_field_equals(
            &execution,
            "native_multi_session_llm_collaboration",
            true,
        )
        && json_top_level_bool_field_equals(&execution, "native_agent_provenance_verified", true)
        && json_top_level_bool_field_equals(&execution, "no_hidden_fallback", true)
        && json_top_level_bool_field_equals(
            &execution,
            "live_multi_provider_worker_collaboration",
            false,
        )
        && json_top_level_bool_field_equals(&execution, "live_remote_provider_api_calls", false)
        && json_top_level_bool_field_equals(&execution, "provider_credentials_required", false)
        && json_top_level_bool_field_equals(&execution, "final_apply_executed", false)
        && json_top_level_bool_field_equals(&execution, "secret_value_exposed", false)
        && native_session_count >= 2
        && completed_session_count >= 2
        && completed_session_count <= native_session_count
        && role_lane_count >= 2
        && native_session_count == source_session_count
        && completed_session_count == source_completed_count
        && worker_lane_count == source_worker_count
        && reviewer_lane_count == source_reviewer_count
        && mapper_lane_count == source_mapper_count
        && beta006_native_collaboration_events_valid(
            &events,
            NativeCollaborationEventExpectations {
                source_mission_id: &source_mission_id,
                native_session_count,
                completed_session_count,
                worker_lane_count,
                reviewer_lane_count,
                mapper_lane_count,
                agent_consensus_ref: &agent_consensus_ref,
                agent_consensus_hash: &agent_consensus_hash,
            },
        )
}

fn read_native_collaboration_source(
    cwd: &Path,
    source_ref: &str,
    expected_hash: &str,
) -> Option<String> {
    if source_ref.contains("..")
        || source_ref.starts_with('/')
        || !source_ref.starts_with(".sneakoscope/missions/")
    {
        return None;
    }
    let contents = fs::read_to_string(cwd.join(source_ref)).ok()?;
    if stable_content_hash(&contents) == expected_hash {
        Some(contents)
    } else {
        None
    }
}

fn beta006_native_collaboration_events_valid(
    events: &str,
    expected: NativeCollaborationEventExpectations<'_>,
) -> bool {
    let expected_events = [
        "native_sessions_discovered",
        "worker_lane_completed",
        "review_or_mapping_lane_completed",
        "consensus_recorded",
        "remote_provider_collaboration_not_claimed",
    ];
    let mut seen = HashMap::new();
    for line in events.lines().filter(|line| !line.trim().is_empty()) {
        let line = line.trim();
        if !json_top_level_string_field_equals(
            line,
            "schema",
            "opensks.native-collaboration-event.v1",
        ) {
            return false;
        }
        let Some(event) = extract_json_top_level_string_field(line, "event") else {
            return false;
        };
        if !expected_events.contains(&event.as_str())
            || seen.insert(event, line.to_string()).is_some()
        {
            return false;
        }
    }
    if expected_events
        .iter()
        .any(|event| !seen.contains_key(*event))
    {
        return false;
    }

    let sessions = seen
        .get("native_sessions_discovered")
        .expect("event exists");
    let worker = seen.get("worker_lane_completed").expect("event exists");
    let review = seen
        .get("review_or_mapping_lane_completed")
        .expect("event exists");
    let consensus = seen.get("consensus_recorded").expect("event exists");
    let remote = seen
        .get("remote_provider_collaboration_not_claimed")
        .expect("event exists");

    json_top_level_string_field_equals(sessions, "source_mission_id", expected.source_mission_id)
        && json_top_level_number_field_equals(
            sessions,
            "session_count",
            expected.native_session_count,
        )
        && json_top_level_number_field_equals(
            sessions,
            "completed_session_count",
            expected.completed_session_count,
        )
        && json_top_level_bool_field_equals(sessions, "executed", true)
        && json_top_level_number_field_equals(
            worker,
            "worker_lane_count",
            expected.worker_lane_count,
        )
        && json_top_level_bool_field_equals(worker, "executed", true)
        && json_top_level_number_field_equals(
            review,
            "reviewer_lane_count",
            expected.reviewer_lane_count,
        )
        && json_top_level_number_field_equals(
            review,
            "mapper_lane_count",
            expected.mapper_lane_count,
        )
        && json_top_level_bool_field_equals(review, "executed", true)
        && json_top_level_string_field_equals(
            consensus,
            "agent_consensus_ref",
            expected.agent_consensus_ref,
        )
        && json_top_level_string_field_equals(
            consensus,
            "agent_consensus_hash",
            expected.agent_consensus_hash,
        )
        && json_top_level_bool_field_equals(consensus, "executed", true)
        && json_top_level_bool_field_equals(
            remote,
            "live_multi_provider_worker_collaboration",
            false,
        )
        && json_top_level_bool_field_equals(remote, "live_remote_provider_api_calls", false)
        && json_top_level_bool_field_equals(remote, "executed", true)
}

fn json_top_level_string_field_equals(input: &str, key: &str, expected: &str) -> bool {
    extract_json_top_level_string_field(input, key).as_deref() == Some(expected)
}

fn json_top_level_bool_field_equals(input: &str, key: &str, expected: bool) -> bool {
    extract_json_top_level_raw_field(input, key).as_deref()
        == Some(if expected { "true" } else { "false" })
}

fn json_top_level_number_field_equals(input: &str, key: &str, expected: usize) -> bool {
    extract_json_top_level_number_field(input, key) == Some(expected)
}

fn json_top_level_min_number_field(input: &str, key: &str, minimum: usize) -> bool {
    extract_json_top_level_number_field(input, key).is_some_and(|value| value >= minimum)
}

fn json_top_level_min_array_length(input: &str, key: &str, minimum: usize) -> bool {
    extract_json_top_level_raw_field(input, key)
        .as_deref()
        .is_some_and(|raw| json_array_value_count(raw) >= minimum)
}

fn json_top_level_field_absent(input: &str, key: &str) -> bool {
    extract_json_top_level_raw_fields(input, key).is_empty()
}

fn json_top_level_empty_array_field_equals(input: &str, key: &str) -> bool {
    extract_json_top_level_raw_field(input, key).is_some_and(|raw| raw.trim() == "[]")
}

fn extract_json_top_level_array_objects(input: &str, key: &str) -> Vec<String> {
    let Some(raw) = extract_json_top_level_raw_field(input, key) else {
        return Vec::new();
    };
    let trimmed = raw.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return Vec::new();
    }
    let mut objects = Vec::new();
    let mut index = 1usize;
    while index < trimmed.len().saturating_sub(1) {
        index = skip_json_whitespace(trimmed, index);
        if index >= trimmed.len().saturating_sub(1) {
            break;
        }
        if trimmed[index..].starts_with(',') {
            index += 1;
            continue;
        }
        if !trimmed[index..].starts_with('{') {
            return Vec::new();
        }
        let Some(end) = json_value_end(trimmed, index) else {
            return Vec::new();
        };
        objects.push(trimmed[index..end].to_string());
        index = end;
    }
    objects
}

fn extract_json_top_level_object_values(input: &str, key: &str) -> Vec<String> {
    let Some(raw) = extract_json_top_level_raw_field(input, key) else {
        return Vec::new();
    };
    let trimmed = raw.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return Vec::new();
    }
    let mut values = Vec::new();
    let mut index = 1usize;
    while index < trimmed.len().saturating_sub(1) {
        index = skip_json_whitespace(trimmed, index);
        if index >= trimmed.len().saturating_sub(1) {
            break;
        }
        if trimmed[index..].starts_with(',') {
            index += 1;
            continue;
        }
        if !trimmed[index..].starts_with('"') {
            return Vec::new();
        }
        let Some(key_end) = json_string_token_end(trimmed, index) else {
            return Vec::new();
        };
        let value_start = skip_json_whitespace(trimmed, key_end);
        if !trimmed[value_start..].starts_with(':') {
            return Vec::new();
        }
        let value_start = skip_json_whitespace(trimmed, value_start + 1);
        if !trimmed[value_start..].starts_with('{') {
            return Vec::new();
        }
        let Some(value_end) = json_value_end(trimmed, value_start) else {
            return Vec::new();
        };
        values.push(trimmed[value_start..value_end].to_string());
        index = value_end;
    }
    values
}

fn json_array_value_count(raw: &str) -> usize {
    let trimmed = raw.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return 0;
    }
    let mut count = 0usize;
    let mut index = 1usize;
    while index < trimmed.len().saturating_sub(1) {
        index = skip_json_whitespace(trimmed, index);
        if index >= trimmed.len().saturating_sub(1) {
            break;
        }
        if trimmed[index..].starts_with(',') {
            index += 1;
            continue;
        }
        let Some(value_end) = json_value_end(trimmed, index) else {
            return 0;
        };
        count += 1;
        index = value_end;
    }
    count
}

fn extract_json_top_level_number_field(input: &str, key: &str) -> Option<usize> {
    extract_json_top_level_raw_field(input, key)?.parse().ok()
}

fn extract_json_top_level_string_field(input: &str, key: &str) -> Option<String> {
    let raw = extract_json_top_level_raw_field(input, key)?;
    if raw.len() < 2 || !raw.starts_with('"') || !raw.ends_with('"') {
        return None;
    }
    Some(unescape_simple_json_string(&raw[1..raw.len() - 1]))
}

fn extract_json_top_level_raw_field(input: &str, key: &str) -> Option<String> {
    let values = extract_json_top_level_raw_fields(input, key);
    if values.len() == 1 {
        values.into_iter().next()
    } else {
        None
    }
}

fn extract_json_top_level_raw_fields(input: &str, key: &str) -> Vec<String> {
    let mut values = Vec::new();
    let trimmed_start = input
        .char_indices()
        .find(|(_, ch)| !ch.is_whitespace())
        .map(|(index, _)| index)
        .unwrap_or(0);
    if !input[trimmed_start..].starts_with('{') {
        return values;
    }

    let mut depth = 0usize;
    let mut index = trimmed_start;
    while index < input.len() {
        let Some((_, ch)) = input[index..].char_indices().next() else {
            break;
        };
        match ch {
            '"' => {
                let Some(string_end) = json_string_token_end(input, index) else {
                    return Vec::new();
                };
                if depth == 1 {
                    let token = unescape_simple_json_string(&input[index + 1..string_end - 1]);
                    let after_key = skip_json_whitespace(input, string_end);
                    if token == key && input[after_key..].starts_with(':') {
                        let value_start = skip_json_whitespace(input, after_key + 1);
                        if let Some(value_end) = json_value_end(input, value_start) {
                            values.push(input[value_start..value_end].trim().to_string());
                            index = value_end;
                            continue;
                        }
                        return Vec::new();
                    }
                }
                index = string_end;
                continue;
            }
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        index += ch.len_utf8();
    }
    values
}

fn json_string_token_end(input: &str, start: usize) -> Option<usize> {
    let mut escaped = false;
    for (offset, ch) in input[start + 1..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            return Some(start + 1 + offset + 1);
        }
    }
    None
}

fn json_value_end(input: &str, start: usize) -> Option<usize> {
    let (_, first) = input[start..].char_indices().next()?;
    if first == '"' {
        return json_string_token_end(input, start);
    }
    if first == '{' || first == '[' {
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for (offset, ch) in input[start..].char_indices() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_string = false;
                }
                continue;
            }
            match ch {
                '"' => in_string = true,
                '{' | '[' => depth += 1,
                '}' | ']' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(start + offset + ch.len_utf8());
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    for (offset, ch) in input[start..].char_indices() {
        if ch == ',' || ch == '}' || ch == ']' || ch.is_whitespace() {
            return Some(start + offset);
        }
    }
    Some(input.len())
}

fn skip_json_whitespace(input: &str, mut index: usize) -> usize {
    while index < input.len() {
        let Some((_, ch)) = input[index..].char_indices().next() else {
            break;
        };
        if !ch.is_whitespace() {
            break;
        }
        index += ch.len_utf8();
    }
    index
}

fn unescape_simple_json_string(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('/') => out.push('/'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn stable_content_hash(value: &str) -> String {
    let hash = stable_content_hash_u64(value);
    format!("fnv1a64:{hash:016x}")
}

fn stable_content_hash_u64(value: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn json_array(values: &[&str]) -> String {
    let strings = values
        .iter()
        .map(|value| json_string(value))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{strings}]")
}

fn json_vec(values: &[String]) -> String {
    let strings = values
        .iter()
        .map(|value| json_string(value))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{strings}]")
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{1f}' => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}
