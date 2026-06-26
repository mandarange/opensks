use std::path::Path;

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
        cap.evidence_refs = vec![
            "runtime:capability-registry".to_string(),
            "adapter:openrouter-native-http".to_string(),
        ];
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
            "driver:openrouter-tools".to_string(),
            "driver:provider-failure-terminal".to_string(),
        ];
        cap.evidence_refs.extend(
            provider_posture
                .evidence_refs()
                .into_iter()
                .map(str::to_string),
        );
        cap.actions = provider_posture.actions();
        append_action(cap, "review_patch_policy");
    }

    if let Some(cap) = capability_mut(&mut report, "model.dispatch") {
        cap.maturity = opensks_contracts::CapabilityMaturity::Foundation;
        cap.available = false;
        cap.reason_code = provider_posture.model_dispatch_reason();
        cap.evidence_refs = vec![
            "provider:openrouter-native-reqwest".to_string(),
            "registry:runtime-overlay".to_string(),
        ];
        cap.evidence_refs.extend(
            provider_posture
                .evidence_refs()
                .into_iter()
                .map(str::to_string),
        );
        cap.actions = provider_posture.actions();
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
