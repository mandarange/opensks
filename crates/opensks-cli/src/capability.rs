use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{CliError, CliOutput};

/// `opensks capability report [--json]` / `opensks capability matrix` — emit the
/// machine-readable runtime capability report (recovery directive §18.4) so CI,
/// the app, and the generated truth matrix all read one honest source. The report
/// starts from the conservative contract baseline, then overlays current
/// workspace/build/runtime evidence (provider setup, daemon protocol, ToolGateway,
/// patch engine, and generated release fixture identity).
pub fn run_capability_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let subcommand = args.first().map(String::as_str).unwrap_or("report");
    let report = runtime_capability_report(cwd, args);
    match subcommand {
        "report" => {
            // JSON is the contract for §18.4; `--json` (if present) is implied.
            let json = serde_json::to_string_pretty(&report).map_err(|error| {
                CliError::Invalid(format!("serialize capability report: {error}"))
            })?;
            Ok(CliOutput {
                stdout: format!("{json}\n"),
            })
        }
        "matrix" => Ok(CliOutput {
            stdout: report.render_truth_matrix_markdown(),
        }),
        other => Err(CliError::Usage(format!(
            "unknown capability subcommand `{other}`\n\nusage: opensks capability report [--json]\n       opensks capability matrix\n"
        ))),
    }
}

pub fn runtime_capability_report(
    cwd: &Path,
    args: &[String],
) -> opensks_contracts::RuntimeCapabilityReport {
    let mut report = opensks_contracts::baseline_capability_report();
    let provider_posture = provider_registry_dispatch_posture(cwd);
    let live_coding_proof = live_coding_execution_proof(cwd);
    let workspace_marker = cwd
        .canonicalize()
        .unwrap_or_else(|_| cwd.to_path_buf())
        .display()
        .to_string();
    let fixture = args
        .windows(2)
        .find_map(|pair| (pair[0] == "--runtime-fixture").then(|| pair[1].to_ascii_lowercase()))
        .unwrap_or_else(|| "local".to_string());
    report.generated_for = Some(format!(
        "workspace:{workspace_marker};crate:{};fixture:{fixture}",
        env!("CARGO_PKG_VERSION")
    ));

    if let Some(cap) = capability_mut(&mut report, "chat.answer") {
        cap.maturity = opensks_contracts::CapabilityMaturity::Foundation;
        cap.available = false;
        cap.reason_code = provider_posture.chat_answer_reason();
        cap.evidence_refs = provider_posture.chat_evidence_refs();
        cap.evidence_refs.extend(
            provider_posture
                .evidence_refs()
                .into_iter()
                .map(str::to_string),
        );
        cap.actions = provider_posture.actions();
    }

    if let Some(cap) = capability_mut(&mut report, "agent.code_edit") {
        cap.maturity = opensks_contracts::CapabilityMaturity::Foundation;
        cap.available = false;
        cap.reason_code = provider_posture.code_edit_reason();
        cap.evidence_refs = vec![
            "crate:opensks-adapter".to_string(),
            "crate:opensks-patch-engine".to_string(),
            "toolgateway:policy-enforced".to_string(),
            "adapter:request-patch-lease".to_string(),
            "scheduler:lease-visible-to-worker".to_string(),
            "daemon:turn-scheduler-worker-route".to_string(),
            "patch-engine:typed-preflight-read".to_string(),
            "patch-engine:pre-apply-revalidated".to_string(),
            "patch-engine:path-lease-bound".to_string(),
            "patch-engine:fence-token-bound".to_string(),
            "patch-engine:stale-temp-scavenger".to_string(),
            "patch-engine:rollback-fault-injected".to_string(),
            "patch-engine:attempt-aware-recovery".to_string(),
            "patch-engine:read-back-verified".to_string(),
            "patch-engine:fsynced-transaction-journal".to_string(),
            "patch-engine:transactional-delete-rename".to_string(),
            "driver:provider-failure-terminal".to_string(),
        ];
        cap.evidence_refs
            .extend(provider_posture.code_edit_provider_evidence_refs());
        cap.evidence_refs.extend(
            provider_posture
                .evidence_refs()
                .into_iter()
                .map(str::to_string),
        );
        cap.actions = provider_posture.actions();
        append_action(cap, "review_patch_policy");
        if let Some(proof) = live_coding_proof.as_ref() {
            cap.maturity = opensks_contracts::CapabilityMaturity::Live;
            cap.available = true;
            cap.reason_code = "provider_backed_code_edit_integrated".to_string();
            cap.actions.clear();
            append_live_coding_evidence(cap, proof);
            append_evidence(cap, "integration:main-workspace-apply-completed");
            append_evidence(cap, "integration:verification-receipt");
        }
    }

    if let Some(cap) = capability_mut(&mut report, "model.dispatch") {
        cap.maturity = opensks_contracts::CapabilityMaturity::Foundation;
        cap.available = false;
        cap.reason_code = provider_posture.model_dispatch_reason();
        cap.evidence_refs = provider_posture.model_dispatch_evidence_refs();
        cap.evidence_refs.extend(
            provider_posture
                .evidence_refs()
                .into_iter()
                .map(str::to_string),
        );
        cap.actions = provider_posture.actions();
        if let Some(proof) = live_coding_proof.as_ref() {
            cap.maturity = opensks_contracts::CapabilityMaturity::Live;
            cap.available = true;
            cap.reason_code = "provider_model_dispatch_observed".to_string();
            cap.actions.clear();
            append_live_coding_evidence(cap, proof);
            append_evidence(cap, "daemon:role-worker-model-call");
            append_evidence(cap, "provider:role-routing");
            append_evidence(cap, &format!("provider:{}", proof.provider_id));
            append_evidence(cap, &format!("model:{}", proof.model_id));
        }
    }

    if let Some(cap) = capability_mut(&mut report, "agent.local_test_edit") {
        if cfg!(feature = "simulation") {
            cap.maturity = opensks_contracts::CapabilityMaturity::Live;
            cap.available = true;
            cap.reason_code = "explicit_local_test_adapter_real_file_io".to_string();
            append_evidence(cap, "toolgateway:workspace-policy");
            append_evidence(cap, "adapter:request-patch-lease");
            append_evidence(cap, "patch-engine:transactional-apply");
            append_evidence(cap, "patch-engine:typed-preflight-read");
            append_evidence(cap, "patch-engine:pre-apply-revalidated");
            append_evidence(cap, "patch-engine:path-lease-bound");
            append_evidence(cap, "patch-engine:fence-token-bound");
            append_evidence(cap, "patch-engine:stale-temp-scavenger");
            append_evidence(cap, "patch-engine:rollback-fault-injected");
            append_evidence(cap, "patch-engine:attempt-aware-recovery");
            append_evidence(cap, "patch-engine:read-back-verified");
            append_evidence(cap, "patch-engine:fsynced-transaction-journal");
        } else {
            cap.maturity = opensks_contracts::CapabilityMaturity::Unavailable;
            cap.available = false;
            cap.reason_code = "simulation_feature_disabled_for_release_build".to_string();
            cap.evidence_refs = vec!["build:simulation-feature-disabled".to_string()];
            cap.actions = vec!["enable_simulation_feature_for_developer_smoke".to_string()];
        }
    }

    if let Some(cap) = capability_mut(&mut report, "stream.protocol") {
        cap.maturity = opensks_contracts::CapabilityMaturity::Live;
        cap.available = true;
        cap.reason_code = "daemon_stream_protocol_v2_explicit_terminal_frames".to_string();
        cap.evidence_refs = vec![
            "daemon:request_completed".to_string(),
            "swift:explicit-terminal-router".to_string(),
            "schema:engine-stream-frame".to_string(),
            "test:request_response_ends_with_an_explicit_terminal_marker".to_string(),
            "test:subscribe_events_emits_stream_v2_frames".to_string(),
        ];
    }

    if let Some(cap) = capability_mut(&mut report, "pipeline.graph") {
        cap.maturity = opensks_contracts::CapabilityMaturity::Foundation;
        cap.available = false;
        cap.reason_code =
            "objective_planner_live_model_artifact_apply_seal_runtime_present_live_vendor_pending"
                .to_string();
        cap.evidence_refs = vec![
            "crate:opensks-graph".to_string(),
            "crate:opensks-engine".to_string(),
            "graph:objective-planner".to_string(),
            "graph:dag-validation".to_string(),
            "graph:proof-contract-requirements".to_string(),
            "graph:bounded-repair-plan".to_string(),
            "graph:repair-groups".to_string(),
            "planner:shard-policy".to_string(),
            "engine:scheduler-requirement-propagation".to_string(),
            "scheduler:objective-plan-turn-bootstrap".to_string(),
            "daemon:objective-plan-live-model-planner".to_string(),
            "daemon:objective-plan-artifact".to_string(),
            "daemon:objective-plan-child-runtime".to_string(),
            "daemon:objective-plan-apply-runtime".to_string(),
            "daemon:objective-plan-seal-runtime".to_string(),
            "schema:compiled-plan".to_string(),
            "swift:pipeline-projection-ingest".to_string(),
            "conversation:timeline-read-model".to_string(),
            "swift:conversation-timeline-read-model".to_string(),
        ];
    }

    if let Some(cap) = capability_mut(&mut report, "agent.parallel_build") {
        if let Some(proof) = live_coding_proof
            .as_ref()
            .filter(|proof| proof.proves_parallel_build())
        {
            cap.maturity = opensks_contracts::CapabilityMaturity::Degraded;
            cap.available = true;
            cap.reason_code = "provider_backed_parallel_integration_observed".to_string();
            cap.actions.clear();
            append_live_coding_evidence(cap, proof);
            append_evidence(cap, "integration:role-candidate-aggregate");
            append_evidence(cap, "integration:aggregate-candidate-ready");
            append_evidence(
                cap,
                &format!(
                    "integration:aggregate-candidate-count-{}",
                    proof.aggregate_candidate_count
                ),
            );
            if let Some(max_parallelism) = proof.max_parallelism {
                append_evidence(cap, &format!("settings:max-parallelism-{max_parallelism}"));
            }
        }
    }

    if let Some(cap) = capability_mut(&mut report, "git.push") {
        cap.maturity = opensks_contracts::CapabilityMaturity::Degraded;
        cap.available = true;
        cap.reason_code = "protected_push_outbox_local_remote_proof_only".to_string();
        cap.evidence_refs = vec![
            "crate:opensks-git-service".to_string(),
            "test:push_cli_full_handshake_pushes_to_local_bare_remote_only".to_string(),
        ];
        cap.actions = vec!["approve_push".to_string()];
    }

    report
}

#[derive(Debug, Clone)]
struct LiveCodingExecutionProof {
    generated_at_ms: u64,
    provider_id: String,
    model_id: String,
    candidate_ref: String,
    verification_ref: String,
    integration_ref: String,
    seal_ref: String,
    semantic_judgment_ref: String,
    aggregate_candidate_count: u64,
    max_parallelism: Option<u64>,
}

impl LiveCodingExecutionProof {
    fn proves_parallel_build(&self) -> bool {
        self.aggregate_candidate_count >= 2 && self.max_parallelism.unwrap_or_default() >= 2
    }
}

fn append_live_coding_evidence(
    cap: &mut opensks_contracts::RuntimeCapability,
    proof: &LiveCodingExecutionProof,
) {
    append_evidence(cap, "daemon:role-worker-code-candidate");
    append_evidence(cap, "daemon:role-worker-model-call");
    append_evidence(cap, "provider:role-routing");
    append_evidence(cap, "git:isolation-prepared");
    append_evidence(cap, "git:atomic-apply");
    append_evidence(cap, &proof.candidate_ref);
    append_evidence(cap, &proof.verification_ref);
    append_evidence(cap, &proof.integration_ref);
    append_evidence(cap, &proof.seal_ref);
    append_evidence(cap, &proof.semantic_judgment_ref);
}

fn live_coding_execution_proof(cwd: &Path) -> Option<LiveCodingExecutionProof> {
    let integration_root = cwd.join(".opensks/runtime/integration-candidates");
    let entries = fs::read_dir(&integration_root).ok()?;
    let mut best: Option<LiveCodingExecutionProof> = None;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let turn_dir = entry.path();
        let turn_dir_name = entry.file_name().to_string_lossy().to_string();
        let Some(proof) = live_coding_execution_proof_for_turn(cwd, &turn_dir, &turn_dir_name)
        else {
            continue;
        };
        if best
            .as_ref()
            .is_none_or(|existing| proof.generated_at_ms > existing.generated_at_ms)
        {
            best = Some(proof);
        }
    }
    best
}

fn live_coding_execution_proof_for_turn(
    cwd: &Path,
    turn_dir: &Path,
    turn_dir_name: &str,
) -> Option<LiveCodingExecutionProof> {
    let candidate_path = turn_dir.join("candidate.json");
    let verification_path = turn_dir.join("verification.json");
    let integration_path = turn_dir.join("integration.json");
    let seal_path = turn_dir.join("seal.json");
    let candidate = read_json_value(&candidate_path)?;
    let verification = read_json_value(&verification_path)?;
    let integration = read_json_value(&integration_path)?;
    let seal = read_json_value(&seal_path)?;
    let candidate_id = json_str(&candidate, "id")?;
    let expected_candidate_ref = artifact_ref(cwd, &candidate_path);
    let expected_verification_ref = artifact_ref(cwd, &verification_path);
    let expected_integration_ref = artifact_ref(cwd, &integration_path);
    let expected_seal_ref = artifact_ref(cwd, &seal_path);

    if json_str(&candidate, "schema")? != "opensks.integration-candidate.v1"
        || json_str(&candidate, "run_id")? != turn_dir_name
        || json_str(&candidate, "state")? != "candidate_ready"
        || json_str(&candidate, "source_isolation_mode")? != "git_worktree"
        || json_u64(&candidate, "patch_count").unwrap_or_default() == 0
        || json_u64(&candidate, "apply_result_count").unwrap_or_default() == 0
        || !json_array_contains_str(
            &candidate,
            "evidence_refs",
            "daemon:role-worker-code-candidate",
        )
        || !json_source_candidates_include_role(&candidate, "role_subcontract", "code")
    {
        return None;
    }

    if json_str(&verification, "schema")? != "opensks.integration-verification-receipt.v1"
        || json_str(&verification, "run_id")? != turn_dir_name
        || json_str(&verification, "candidate_id")? != candidate_id
        || json_str(&verification, "candidate_ref")? != expected_candidate_ref
        || json_str(&verification, "verification_ref")? != expected_verification_ref
        || json_str(&verification, "state")? != "passed"
        || json_str(&verification, "reason_code")? != "candidate_verification_passed"
        || !json_array_empty(&verification, "failed_gates")
        || !json_array_contains_str(&verification, "passed_gates", "git_apply_check_passed")
    {
        return None;
    }

    if json_str(&integration, "schema")? != "opensks.integration-apply-receipt.v1"
        || json_str(&integration, "run_id")? != turn_dir_name
        || json_str(&integration, "candidate_id")? != candidate_id
        || json_str(&integration, "candidate_ref")? != expected_candidate_ref
        || json_str(&integration, "verification_ref")? != expected_verification_ref
        || json_str(&integration, "integration_ref")? != expected_integration_ref
        || json_str(&integration, "state")? != "integrated"
        || !json_bool(&integration, "main_workspace_modified").unwrap_or(false)
        || !json_bool(&integration, "verifier_passed").unwrap_or(false)
    {
        return None;
    }

    if json_str(&seal, "schema")? != "opensks.integration-final-seal.v1"
        || json_str(&seal, "run_id")? != turn_dir_name
        || json_str(&seal, "candidate_id")? != candidate_id
        || json_str(&seal, "candidate_ref")? != expected_candidate_ref
        || json_str(&seal, "verification_ref")? != expected_verification_ref
        || json_str(&seal, "integration_ref")? != expected_integration_ref
        || json_str(&seal, "seal_ref")? != expected_seal_ref
        || json_str(&seal, "state")? != "sealed"
        || !json_array_empty(&seal, "failed_gates")
        || !json_array_contains_str(&seal, "passed_gates", "main_workspace_apply_completed")
        || !json_array_contains_str(&seal, "passed_gates", "final_diff_captured")
    {
        return None;
    }

    let semantic = semantic_provider_judgment(cwd, turn_dir_name)?;
    Some(LiveCodingExecutionProof {
        generated_at_ms: json_u64(&seal, "generated_at_ms")
            .or_else(|| json_u64(&integration, "generated_at_ms"))
            .or_else(|| json_u64(&candidate, "generated_at_ms"))
            .unwrap_or_default(),
        provider_id: semantic.provider_id,
        model_id: semantic.model_id,
        candidate_ref: expected_candidate_ref,
        verification_ref: expected_verification_ref,
        integration_ref: expected_integration_ref,
        seal_ref: expected_seal_ref,
        semantic_judgment_ref: semantic.judgment_ref,
        aggregate_candidate_count: json_u64(&candidate, "aggregate_candidate_count").unwrap_or(1),
        max_parallelism: candidate
            .get("turn_settings")
            .and_then(|settings| json_u64(settings, "max_parallelism")),
    })
}

#[derive(Debug, Clone)]
struct SemanticProviderJudgment {
    provider_id: String,
    model_id: String,
    judgment_ref: String,
}

fn semantic_provider_judgment(cwd: &Path, turn_dir_name: &str) -> Option<SemanticProviderJudgment> {
    let root = cwd
        .join(".opensks/runtime/semantic-verifiers")
        .join(turn_dir_name);
    let mut judgments = Vec::new();
    collect_judgment_paths(&root, &mut judgments);
    for path in judgments {
        let judgment = read_json_value(&path)?;
        if json_str(&judgment, "schema")? != "opensks.semantic-verifier-judgment.v1"
            || json_str(&judgment, "run_id")? != turn_dir_name
            || json_str(&judgment, "state")? != "judgment_ready"
            || !json_array_contains_str(&judgment, "evidence_refs", "daemon:role-worker-model-call")
            || !json_array_contains_str(&judgment, "evidence_refs", "provider:role-routing")
        {
            continue;
        }
        let provider_id = json_str(&judgment, "provider_id")?.to_string();
        let model_id = json_str(&judgment, "model_id")?.to_string();
        if provider_id.trim().is_empty() || model_id.trim().is_empty() {
            continue;
        }
        return Some(SemanticProviderJudgment {
            provider_id,
            model_id,
            judgment_ref: artifact_ref(cwd, &path),
        });
    }
    None
}

fn collect_judgment_paths(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_judgment_paths(&path, out);
        } else if path.file_name().and_then(|name| name.to_str()) == Some("judgment.json") {
            out.push(path);
        }
    }
}

fn artifact_ref(cwd: &Path, path: &Path) -> String {
    let relative = path
        .strip_prefix(cwd)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    format!("artifact://{relative}")
}

fn read_json_value(path: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn json_str<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn json_bool(value: &serde_json::Value, key: &str) -> Option<bool> {
    value.get(key)?.as_bool()
}

fn json_u64(value: &serde_json::Value, key: &str) -> Option<u64> {
    value.get(key)?.as_u64()
}

fn json_array_empty(value: &serde_json::Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(|item| item.as_array())
        .is_some_and(Vec::is_empty)
}

fn json_array_contains_str(value: &serde_json::Value, key: &str, expected: &str) -> bool {
    value
        .get(key)
        .and_then(|item| item.as_array())
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.as_str().is_some_and(|value| value == expected))
        })
}

fn json_source_candidates_include_role(
    value: &serde_json::Value,
    expected_source: &str,
    expected_role: &str,
) -> bool {
    value
        .get("source_candidates")
        .and_then(|item| item.as_array())
        .is_some_and(|items| {
            items.iter().any(|item| {
                json_str(item, "source") == Some(expected_source)
                    && json_str(item, "role") == Some(expected_role)
                    && json_str(item, "source_isolation_mode") == Some("git_worktree")
            })
        })
}

fn capability_mut<'a>(
    report: &'a mut opensks_contracts::RuntimeCapabilityReport,
    id: &str,
) -> Option<&'a mut opensks_contracts::RuntimeCapability> {
    report.capabilities.iter_mut().find(|cap| cap.id == id)
}

fn append_evidence(cap: &mut opensks_contracts::RuntimeCapability, evidence: &str) {
    if !cap
        .evidence_refs
        .iter()
        .any(|existing| existing == evidence)
    {
        cap.evidence_refs.push(evidence.to_string());
    }
}

fn append_action(cap: &mut opensks_contracts::RuntimeCapability, action: &str) {
    if !cap.actions.iter().any(|existing| existing == action) {
        cap.actions.push(action.to_string());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderRegistryDispatchPosture {
    NoRegistryDb,
    NoEnabledProvider,
    EnabledProviderNeedsModelCatalog,
    CodeRoutePresent,
}

impl ProviderRegistryDispatchPosture {
    fn chat_answer_reason(self) -> String {
        match self {
            Self::CodeRoutePresent => {
                "provider_registry_code_route_present_live_chat_probe_required".to_string()
            }
            Self::EnabledProviderNeedsModelCatalog => {
                "provider_registry_enabled_provider_needs_model_catalog".to_string()
            }
            Self::NoEnabledProvider => "provider_registry_no_enabled_provider".to_string(),
            Self::NoRegistryDb => {
                if std::env::var_os("OPENROUTER_API_KEY").is_some() {
                    "openrouter_key_present_but_live_chat_answer_unprobed".to_string()
                } else {
                    "model_credentials_missing_for_live_chat_answer".to_string()
                }
            }
        }
    }

    fn code_edit_reason(self) -> String {
        match self {
            Self::CodeRoutePresent => {
                "agentic_loop_provider_registry_code_route_present_live_dispatch_unverified"
                    .to_string()
            }
            Self::EnabledProviderNeedsModelCatalog => {
                "agentic_loop_provider_registry_needs_model_catalog".to_string()
            }
            Self::NoEnabledProvider => {
                "agentic_loop_provider_registry_needs_enabled_provider".to_string()
            }
            Self::NoRegistryDb => {
                "agentic_loop_toolgateway_patch_engine_need_live_provider_credentials".to_string()
            }
        }
    }

    fn model_dispatch_reason(self) -> String {
        match self {
            Self::CodeRoutePresent => {
                "provider_registry_code_route_present_dispatch_probe_required".to_string()
            }
            Self::EnabledProviderNeedsModelCatalog => {
                "provider_registry_enabled_provider_needs_model_catalog".to_string()
            }
            Self::NoEnabledProvider => "provider_registry_no_enabled_provider".to_string(),
            Self::NoRegistryDb => {
                if std::env::var_os("OPENROUTER_API_KEY").is_some() {
                    "openrouter_secret_present_runtime_probe_required".to_string()
                } else {
                    "openrouter_secret_missing".to_string()
                }
            }
        }
    }

    fn evidence_refs(self) -> Vec<&'static str> {
        match self {
            Self::CodeRoutePresent => vec![
                "provider-registry:enabled-provider",
                "provider-registry:enabled-code-model",
                "provider-registry:secret-ref-only",
            ],
            Self::EnabledProviderNeedsModelCatalog => vec![
                "provider-registry:enabled-provider",
                "provider-registry:model-catalog-missing-code-route",
                "provider-registry:secret-ref-only",
            ],
            Self::NoEnabledProvider => vec!["provider-registry:no-enabled-provider"],
            Self::NoRegistryDb => vec!["provider-registry:not-materialized"],
        }
    }

    fn chat_evidence_refs(self) -> Vec<String> {
        let mut refs = vec!["runtime:capability-registry".to_string()];
        if self == Self::NoRegistryDb {
            refs.push("adapter:openrouter-native-http".to_string());
        } else if self.has_enabled_provider() {
            refs.push("adapter:openai-compatible-native-http".to_string());
        }
        refs
    }

    fn code_edit_provider_evidence_refs(self) -> Vec<String> {
        if self == Self::NoRegistryDb {
            vec!["driver:openrouter-tools".to_string()]
        } else if self.has_enabled_provider() {
            vec!["driver:openai-compatible-tools".to_string()]
        } else {
            Vec::new()
        }
    }

    fn model_dispatch_evidence_refs(self) -> Vec<String> {
        let mut refs = vec!["registry:runtime-overlay".to_string()];
        if self == Self::NoRegistryDb {
            refs.insert(0, "provider:openrouter-native-reqwest".to_string());
        } else if self.has_enabled_provider() {
            refs.insert(0, "provider:openai-compatible-native-reqwest".to_string());
        }
        refs
    }

    fn has_enabled_provider(self) -> bool {
        matches!(
            self,
            Self::CodeRoutePresent | Self::EnabledProviderNeedsModelCatalog
        )
    }

    fn actions(self) -> Vec<String> {
        match self {
            Self::CodeRoutePresent => vec![
                "run_provider_adapter_check".to_string(),
                "connect_model".to_string(),
            ],
            Self::EnabledProviderNeedsModelCatalog => vec![
                "sync_models".to_string(),
                "run_provider_adapter_check".to_string(),
            ],
            Self::NoEnabledProvider | Self::NoRegistryDb => vec!["connect_model".to_string()],
        }
    }
}

fn provider_registry_dispatch_posture(cwd: &Path) -> ProviderRegistryDispatchPosture {
    let db_path = cwd.join(opensks_provider::PROVIDER_DB_RELATIVE_PATH);
    if !db_path.exists() {
        return ProviderRegistryDispatchPosture::NoRegistryDb;
    }
    let Ok(repo) = opensks_provider::ProviderRepository::open_path(&db_path) else {
        return ProviderRegistryDispatchPosture::NoRegistryDb;
    };
    let Ok(providers) = repo.list_connections() else {
        return ProviderRegistryDispatchPosture::NoRegistryDb;
    };
    let enabled_providers = providers
        .iter()
        .filter(|provider| provider.enabled)
        .collect::<Vec<_>>();
    if enabled_providers.is_empty() {
        return ProviderRegistryDispatchPosture::NoEnabledProvider;
    }

    let mut enabled_code_model_count = 0usize;
    for provider in &enabled_providers {
        let Ok(models) = repo.list_models(&provider.id) else {
            continue;
        };
        enabled_code_model_count += models
            .iter()
            .filter(|model| model.enabled)
            .filter(|model| model.capabilities.code)
            .filter(|model| {
                !matches!(
                    model.health,
                    opensks_contracts::HealthState::Unavailable
                        | opensks_contracts::HealthState::OpenCircuit
                )
            })
            .count();
    }
    if enabled_code_model_count == 0 {
        ProviderRegistryDispatchPosture::EnabledProviderNeedsModelCatalog
    } else {
        ProviderRegistryDispatchPosture::CodeRoutePresent
    }
}
