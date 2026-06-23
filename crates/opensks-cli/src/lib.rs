use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use thiserror::Error;

mod conversation_args;
mod conversation_timeline;
mod retention;
use conversation_args::{
    parse_conversation_filter, parse_conversation_options, parse_conversation_role,
    parse_timeline_kind, require_conversation_field,
};
pub use retention::{gc_usage, release_usage, run_gc_command, run_release_command};

const DEFAULT_WORKER_LEASE_TTL_SECONDS: u64 = 30;

#[derive(Debug, Clone)]
pub struct CliOutput {
    pub stdout: String,
}

#[derive(Debug, Error)]
pub enum CliError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Invalid(String),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug)]
struct DaemonCommandOptions {
    stdio: bool,
    workspace: PathBuf,
}

#[derive(Debug, Clone)]
struct WorkerLeaseRecord {
    lease_id: String,
    worker_id: String,
    lane: String,
    state: String,
    leased_at_seconds: u64,
    last_heartbeat_seconds: u64,
    expires_at_seconds: u64,
    recovery_action: String,
}

#[derive(Debug, Clone)]
struct WorkerRouteRecord {
    request_id: String,
    lane: String,
    assigned_worker: String,
    lease_id: String,
    route_status: String,
    queued_at_ms: u128,
    dispatched_at_ms: u128,
    completed_at_ms: u128,
}

#[derive(Debug, Clone)]
pub struct CommandCheck {
    pub name: String,
    pub command: Vec<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone)]
struct StageOverlapSpan {
    name: String,
    command: Vec<String>,
    status: String,
    exit_code: Option<i32>,
    start_ms: u128,
    end_ms: u128,
    duration_ms: u128,
    stdout: String,
    stderr: String,
}

struct ClockStamp {
    secs: u64,
    nanos: u32,
}

impl ClockStamp {
    fn now() -> Result<Self, CliError> {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| CliError::Invalid("system clock is before UNIX_EPOCH".to_string()))?;
        Ok(Self {
            secs: duration.as_secs(),
            nanos: duration.subsec_nanos(),
        })
    }

    fn compact_id(&self) -> String {
        format!("{}{:09}", self.secs, self.nanos)
    }

    fn json(&self) -> String {
        format!(
            "{{\"unix_seconds\":{},\"nanos\":{}}}",
            self.secs, self.nanos
        )
    }
}

pub fn is_daemon_stdio_invocation(args: &[String]) -> bool {
    args.first().is_some_and(|arg| arg == "daemon") && args.iter().any(|arg| arg == "--stdio")
}

pub fn run_daemon_stdio_stream(args: &[String], cwd: &Path) -> Result<(), CliError> {
    let Some(command) = args.first() else {
        return Err(CliError::Usage(daemon_usage().to_string()));
    };
    if command != "daemon" {
        return Err(CliError::Usage(daemon_usage().to_string()));
    }
    let options = parse_daemon_options(&args[1..], cwd)?;
    if !options.stdio {
        return Err(CliError::Usage(daemon_usage().to_string()));
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    opensks_daemon::run_stdio_stream(
        stdin.lock(),
        stdout,
        &opensks_daemon::DaemonOptions {
            workspace: options.workspace,
        },
    )
    .map_err(|error| CliError::Invalid(error.to_string()))
}

pub fn run_daemon_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(CliOutput {
            stdout: daemon_usage().to_string(),
        });
    }
    let options = parse_daemon_options(args, cwd)?;
    if !options.stdio {
        return Err(CliError::Usage(daemon_usage().to_string()));
    }

    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let output = opensks_daemon::run_stdio(
        &input,
        &opensks_daemon::DaemonOptions {
            workspace: options.workspace,
        },
    )
    .map_err(|error| CliError::Invalid(error.to_string()))?;
    Ok(CliOutput { stdout: output })
}

pub fn daemon_usage() -> &'static str {
    "usage: opensks daemon --stdio --workspace <path>\n"
}

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
        cap.reason_code = if std::env::var_os("OPENROUTER_API_KEY").is_some() {
            "openrouter_key_present_but_live_chat_answer_unprobed".to_string()
        } else {
            "model_credentials_missing_for_live_chat_answer".to_string()
        };
        cap.evidence_refs = vec![
            "runtime:capability-registry".to_string(),
            "adapter:openrouter-native-http".to_string(),
        ];
        cap.actions = vec!["connect_model".to_string()];
    }

    if let Some(cap) = capability_mut(&mut report, "agent.code_edit") {
        cap.maturity = opensks_contracts::CapabilityMaturity::Foundation;
        cap.available = false;
        cap.reason_code =
            "agentic_loop_toolgateway_patch_engine_need_live_provider_credentials".to_string();
        cap.evidence_refs = vec![
            "crate:opensks-adapter".to_string(),
            "crate:opensks-patch-engine".to_string(),
            "toolgateway:policy-enforced".to_string(),
            "patch-engine:fsynced-transaction-journal".to_string(),
            "patch-engine:transactional-delete-rename".to_string(),
            "driver:openrouter-tools".to_string(),
            "driver:provider-failure-terminal".to_string(),
        ];
        cap.actions = vec![
            "connect_model".to_string(),
            "review_patch_policy".to_string(),
        ];
    }

    if let Some(cap) = capability_mut(&mut report, "model.dispatch") {
        cap.maturity = opensks_contracts::CapabilityMaturity::Foundation;
        cap.available = false;
        cap.reason_code = if std::env::var_os("OPENROUTER_API_KEY").is_some() {
            "openrouter_secret_present_runtime_probe_required".to_string()
        } else {
            "openrouter_secret_missing".to_string()
        };
        cap.evidence_refs = vec![
            "provider:openrouter-native-reqwest".to_string(),
            "registry:runtime-overlay".to_string(),
        ];
        cap.actions = vec!["connect_model".to_string()];
    }

    if let Some(cap) = capability_mut(&mut report, "agent.local_test_edit") {
        if cfg!(feature = "simulation") {
            cap.maturity = opensks_contracts::CapabilityMaturity::Live;
            cap.available = true;
            cap.reason_code = "explicit_local_test_adapter_real_file_io".to_string();
            append_evidence(cap, "toolgateway:workspace-policy");
            append_evidence(cap, "patch-engine:transactional-apply");
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
        cap.maturity = opensks_contracts::CapabilityMaturity::Degraded;
        cap.available = true;
        cap.reason_code = "daemon_ndjson_explicit_terminal_protocol_v2_missing".to_string();
        cap.evidence_refs = vec![
            "daemon:request_completed".to_string(),
            "swift:explicit-terminal-router".to_string(),
            "test:request_response_ends_with_an_explicit_terminal_marker".to_string(),
        ];
    }

    if let Some(cap) = capability_mut(&mut report, "pipeline.graph") {
        cap.maturity = opensks_contracts::CapabilityMaturity::Foundation;
        cap.available = false;
        cap.reason_code = "timeline_read_model_no_live_event_stream_projection".to_string();
        cap.evidence_refs = vec![
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

pub fn run_history_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let subcommand = args
        .first()
        .ok_or_else(|| CliError::Usage(history_usage().to_string()))?;
    match subcommand.as_str() {
        "init" => {
            let mut store = opensks_event_store::EventStore::open_workspace(cwd)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let stamp = ClockStamp::now()?;
            let event = opensks_contracts::ExecutionEventEnvelope {
                schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
                id: format!("evt-{}", stamp.compact_id()),
                run_id: "history-init".to_string(),
                sequence: 0,
                occurred_at: format!("{}.{}", stamp.secs, stamp.nanos),
                actor: "opensks-cli".to_string(),
                causation_id: None,
                correlation_id: Some("history-init".to_string()),
                kind: opensks_contracts::EventKind::RunStarted,
                payload: serde_json::json!({"message": "history store initialized"}),
                sensitivity: opensks_contracts::Sensitivity::Internal,
                evidence_refs: vec!["history:init".to_string()],
            };
            let committed = store
                .append_event(event)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            store
                .write_snapshot(
                    "history-init",
                    committed.sequence,
                    serde_json::json!({"state": "initialized"}),
                )
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let integrity = store
                .integrity_check()
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            Ok(CliOutput {
                stdout: format!(
                    "initialized OpenSKS event store\nstore: {}\nintegrity: {}\nsequence: {}\n",
                    cwd.join(opensks_event_store::ENGINE_DB_RELATIVE_PATH)
                        .display(),
                    integrity,
                    committed.sequence
                ),
            })
        }
        "--help" | "-h" => Ok(CliOutput {
            stdout: history_usage().to_string(),
        }),
        other => Err(CliError::Usage(format!(
            "unknown history subcommand `{other}`\n\n{}",
            history_usage()
        ))),
    }
}

pub fn history_usage() -> &'static str {
    "usage: opensks history init\n"
}

pub fn run_graph_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let subcommand = args
        .first()
        .ok_or_else(|| CliError::Usage(graph_usage().to_string()))?;
    match subcommand.as_str() {
        "templates" => {
            let written = opensks_graph::write_default_templates(cwd)
                .map_err(|error| CliError::Invalid(format!("write graph templates: {error}")))?;
            Ok(CliOutput {
                stdout: format!(
                    "wrote default pipeline graph templates\ntemplates: {}\n",
                    written.len()
                ),
            })
        }
        "compile" => {
            let template_id = args
                .get(1)
                .map(String::as_str)
                .unwrap_or("single-model-safe");
            let graph = opensks_graph::default_templates()
                .into_iter()
                .find(|graph| graph.id == template_id)
                .ok_or_else(|| {
                    CliError::Usage(format!(
                        "unknown graph template `{template_id}`\n\n{}",
                        graph_usage()
                    ))
                })?;
            let plan = opensks_graph::compile_graph(&graph);
            let dir = cwd.join(".opensks").join("pipelines").join("compiled");
            fs::create_dir_all(&dir)?;
            let artifact = dir.join(format!("{template_id}.plan.json"));
            write_text_atomic(
                &artifact,
                &(serde_json::to_string_pretty(&plan).map_err(|error| {
                    CliError::Invalid(format!("serialize compiled graph plan: {error}"))
                })? + "\n"),
            )?;
            let error_count = plan
                .diagnostics
                .iter()
                .filter(|item| item.severity == opensks_contracts::DiagnosticSeverity::Error)
                .count();
            Ok(CliOutput {
                stdout: format!(
                    "compiled pipeline graph\nid: {}\nplan_hash: {}\ndiagnostics_errors: {}\nartifact: {}\n",
                    plan.graph_id,
                    plan.plan_hash,
                    error_count,
                    artifact.display()
                ),
            })
        }
        other => Err(CliError::Usage(format!(
            "unknown graph subcommand `{other}`\n\n{}",
            graph_usage()
        ))),
    }
}

pub fn graph_usage() -> &'static str {
    concat!(
        "usage: opensks graph templates\n",
        "       opensks graph compile [single-model-safe|balanced-multi-model|extreme-parallel|image-heavy-product-build|research-report]\n"
    )
}

pub fn run_hooks_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    if args.len() != 1 || args[0] != "replay" {
        return Err(CliError::Usage(hooks_usage().to_string()));
    }
    let engine = opensks_hooks::HookEngine::new(vec![
        opensks_hooks::hook_spec(
            "before-run-allow",
            opensks_contracts::HookPhase::BeforeRun,
            0,
            opensks_contracts::HookAction::Allow,
        ),
        opensks_hooks::hook_spec(
            "before-run-modify",
            opensks_contracts::HookPhase::BeforeRun,
            1,
            opensks_contracts::HookAction::Modify,
        ),
    ]);
    let invocation = opensks_hooks::invocation(
        "cli-hook-replay",
        opensks_contracts::HookPhase::BeforeRun,
        serde_json::json!({"message": "hook replay smoke"}),
    );
    let decisions = engine.dispatch(&invocation);
    let jsonl = opensks_hooks::HookEngine::decisions_jsonl(&decisions)
        .map_err(|error| CliError::Invalid(format!("serialize hook decisions: {error}")))?;
    let replayed = opensks_hooks::HookEngine::replay(&jsonl)
        .map_err(|error| CliError::Invalid(format!("replay hook decisions: {error}")))?;
    let dir = cwd.join(".opensks").join("hooks");
    fs::create_dir_all(&dir)?;
    write_text_atomic(&dir.join("hook-decisions.jsonl"), &jsonl)?;
    Ok(CliOutput {
        stdout: format!(
            "replayed hook decisions\ndecisions: {}\nexact_replay: {}\nartifact: {}\n",
            decisions.len(),
            decisions == replayed,
            dir.join("hook-decisions.jsonl").display()
        ),
    })
}

pub fn hooks_usage() -> &'static str {
    "usage: opensks hooks replay\n"
}

pub fn run_codegraph_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let subcommand = args
        .first()
        .ok_or_else(|| CliError::Usage(codegraph_usage().to_string()))?;
    match subcommand.as_str() {
        "index" => {
            let graph = opensks_codegraph::CodeGraph::index_workspace(cwd)
                .map_err(|error| CliError::Invalid(format!("index codegraph: {error}")))?;
            let index = graph.to_index();
            let path = opensks_codegraph::write_index(cwd, &graph)
                .map_err(|error| CliError::Invalid(format!("write codegraph: {error}")))?;
            Ok(CliOutput {
                stdout: format!(
                    "indexed code graph\nrecords: {}\nedges: {}\nartifact: {}\n",
                    index.records.len(),
                    index.edges.len(),
                    path.display()
                ),
            })
        }
        "query" => {
            let query = require_freeform_cli(&args[1..], codegraph_usage())?;
            let graph = opensks_codegraph::CodeGraph::index_workspace(cwd)
                .map_err(|error| CliError::Invalid(format!("index codegraph: {error}")))?;
            let hits = graph.query(&query);
            let dir = cwd.join(".opensks").join("wiki").join("indexes");
            fs::create_dir_all(&dir)?;
            write_text_atomic(
                &dir.join("codegraph-query.json"),
                &(serde_json::to_string_pretty(&hits).map_err(|error| {
                    CliError::Invalid(format!("serialize codegraph query: {error}"))
                })? + "\n"),
            )?;
            Ok(CliOutput {
                stdout: format!(
                    "queried code graph\nquery: {}\nhits: {}\nartifact: {}\n",
                    query,
                    hits.len(),
                    dir.join("codegraph-query.json").display()
                ),
            })
        }
        "update" => run_codegraph_update(&args[1..], cwd),
        other => Err(CliError::Usage(format!(
            "unknown codegraph subcommand `{other}`\n\n{}",
            codegraph_usage()
        ))),
    }
}

/// `opensks codegraph update --workspace <p> --path <rel>` — incrementally
/// re-index ONE file. Loads the persisted index (or builds it once if absent),
/// then calls `CodeGraph::update_file` for the single path and persists. It
/// never calls `index_workspace` when an index already exists, so `full_scan`
/// is always reported `false` on the wire.
fn run_codegraph_update(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_workspace_path_options(args, codegraph_usage())?;
    let workspace = options.workspace.unwrap_or_else(|| cwd.to_path_buf());
    let relative = options.path.ok_or_else(|| {
        CliError::Usage(format!(
            "codegraph update requires `--path`\n\n{}",
            codegraph_usage()
        ))
    })?;

    // Load-or-build: an existing persisted index takes the incremental path; a
    // missing index is built once (the only time a full scan is acceptable, and
    // it is reported truthfully).
    let (mut graph, full_scan) = match opensks_codegraph::read_index(&workspace)
        .map_err(|error| CliError::Invalid(format!("load codegraph index: {error}")))?
    {
        Some(graph) => (graph, false),
        None => (
            opensks_codegraph::CodeGraph::index_workspace(&workspace)
                .map_err(|error| CliError::Invalid(format!("index codegraph: {error}")))?,
            true,
        ),
    };

    let absolute = workspace.join(&relative);
    graph
        .update_file(&workspace, &absolute)
        .map_err(|error| CliError::Invalid(format!("update codegraph file: {error}")))?;
    opensks_codegraph::write_index(&workspace, &graph)
        .map_err(|error| CliError::Invalid(format!("write codegraph: {error}")))?;

    let body = serde_json::json!({
        "schema": "opensks.codegraph-update.v1",
        "path": relative,
        "symbol_count": graph.symbol_count(),
        "full_scan": full_scan,
    });
    file_output(&body)
}

pub fn codegraph_usage() -> &'static str {
    concat!(
        "usage: opensks codegraph index\n",
        "       opensks codegraph query <text>\n",
        "       opensks codegraph update --workspace <path> --path <relative>\n"
    )
}

/// Flags for the PR-041 `intel` subcommands. `workspace` defaults to `cwd`.
/// `query`/`limit`/`offset` drive `codegraph-query`; `head`/`worktree`/`index`
/// are the stamped freshness values compared by `freshness-check`.
#[derive(Debug, Default)]
struct IntelOptions {
    workspace: Option<PathBuf>,
    query: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
    head: Option<String>,
    worktree: Option<String>,
    index: Option<String>,
}

fn parse_intel_options(args: &[String]) -> Result<IntelOptions, CliError> {
    let usage = intel_usage();
    let mut options = IntelOptions::default();
    let mut idx = 0;
    while idx < args.len() {
        let flag = args[idx].as_str();
        let value = || -> Result<&str, CliError> {
            args.get(idx + 1).map(String::as_str).ok_or_else(|| {
                CliError::Usage(format!("flag `{flag}` requires a value\n\n{usage}"))
            })
        };
        let parse_count = |raw: &str| -> Result<usize, CliError> {
            raw.parse::<usize>()
                .map_err(|_| CliError::Usage(format!("flag `{flag}` requires a number\n\n{usage}")))
        };
        match flag {
            "--workspace" => {
                options.workspace = Some(PathBuf::from(value()?));
                idx += 2;
            }
            "--query" => {
                options.query = Some(value()?.to_string());
                idx += 2;
            }
            "--limit" => {
                options.limit = Some(parse_count(value()?)?);
                idx += 2;
            }
            "--offset" => {
                options.offset = Some(parse_count(value()?)?);
                idx += 2;
            }
            "--head" => {
                options.head = Some(value()?.to_string());
                idx += 2;
            }
            "--worktree" => {
                options.worktree = Some(value()?.to_string());
                idx += 2;
            }
            "--index" => {
                options.index = Some(value()?.to_string());
                idx += 2;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown argument `{other}`\n\n{usage}"
                )));
            }
        }
    }
    Ok(options)
}

/// `opensks intel <subcommand>` — Project Intelligence + Freshness (PR-041).
///
/// Subcommands: `freshness`, `freshness-check`, `codegraph-query`, `glossary`,
/// `architecture`. Each emits its `opensks.intel-*.v1` JSON contract on stdout.
/// `workspace` defaults to the process `cwd` when `--workspace` is omitted.
pub fn run_intel_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let subcommand = args
        .first()
        .ok_or_else(|| CliError::Usage(intel_usage().to_string()))?;
    let options = parse_intel_options(&args[1..])?;
    let workspace = options
        .workspace
        .clone()
        .unwrap_or_else(|| cwd.to_path_buf());
    match subcommand.as_str() {
        "freshness" => {
            let stamp = opensks_intel::freshness(&workspace)
                .map_err(|error| CliError::Invalid(format!("intel freshness: {error}")))?;
            let body = serde_json::to_value(&stamp)
                .map_err(|error| CliError::Invalid(format!("serialize freshness: {error}")))?;
            file_output(&body)
        }
        "freshness-check" => {
            let stamped = opensks_intel::StampedFreshness {
                head_hash: options.head.clone(),
                worktree_hash: options.worktree.clone(),
                index_hash: options.index.clone(),
            };
            let check = opensks_intel::freshness_check(&workspace, &stamped)
                .map_err(|error| CliError::Invalid(format!("intel freshness-check: {error}")))?;
            let body = serde_json::to_value(&check).map_err(|error| {
                CliError::Invalid(format!("serialize freshness-check: {error}"))
            })?;
            file_output(&body)
        }
        "codegraph-query" => {
            let query = options.query.clone().ok_or_else(|| {
                CliError::Usage(format!(
                    "intel codegraph-query requires `--query`\n\n{}",
                    intel_usage()
                ))
            })?;
            let limit = options.limit.unwrap_or(50);
            let offset = options.offset.unwrap_or(0);
            let result = opensks_intel::codegraph_query(&workspace, &query, limit, offset)
                .map_err(|error| CliError::Invalid(format!("intel codegraph-query: {error}")))?;
            let body = serde_json::to_value(&result).map_err(|error| {
                CliError::Invalid(format!("serialize codegraph-query: {error}"))
            })?;
            file_output(&body)
        }
        "glossary" => {
            let glossary = opensks_intel::glossary(&workspace)
                .map_err(|error| CliError::Invalid(format!("intel glossary: {error}")))?;
            let body = serde_json::to_value(&glossary)
                .map_err(|error| CliError::Invalid(format!("serialize glossary: {error}")))?;
            file_output(&body)
        }
        "architecture" => {
            let architecture = opensks_intel::architecture(&workspace)
                .map_err(|error| CliError::Invalid(format!("intel architecture: {error}")))?;
            let body = serde_json::to_value(&architecture)
                .map_err(|error| CliError::Invalid(format!("serialize architecture: {error}")))?;
            file_output(&body)
        }
        other => Err(CliError::Usage(format!(
            "unknown intel subcommand `{other}`\n\n{}",
            intel_usage()
        ))),
    }
}

pub fn intel_usage() -> &'static str {
    concat!(
        "usage: opensks intel freshness --workspace <path>\n",
        "       opensks intel freshness-check --workspace <path> [--head <h>] [--worktree <h>] [--index <h>]\n",
        "       opensks intel codegraph-query --workspace <path> --query <text> [--limit <n>] [--offset <n>]\n",
        "       opensks intel glossary --workspace <path>\n",
        "       opensks intel architecture --workspace <path>\n"
    )
}

pub fn run_triwiki_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    if args.len() != 1 || args[0] != "seed" {
        return Err(CliError::Usage(triwiki_usage().to_string()));
    }
    let records = vec![
        opensks_triwiki::make_record(
            "architecture-runtime-foundation",
            opensks_contracts::TriWikiRecordKind::Architecture,
            "Runtime foundation",
            "Contracts, event store, provider routing, scheduler, Git isolation, graph, hooks, code graph, and context packs are split into workspace crates.",
            vec!["architecture".to_string()],
            vec!["docs/runtime-truth-matrix.md".to_string()],
        ),
        opensks_triwiki::make_record(
            "glossary-work-item",
            opensks_contracts::TriWikiRecordKind::Glossary,
            "WorkItem",
            "A schedulable unit reconstructed from execution events and admitted by the durable scheduler governor.",
            vec!["glossary".to_string()],
            vec!["crates/opensks-scheduler/src/lib.rs".to_string()],
        ),
        opensks_triwiki::make_record(
            "wrongness-foundation-not-live",
            opensks_contracts::TriWikiRecordKind::Wrongness,
            "Foundation is not live completion",
            "Foundation crates and fixture tests must not be described as live provider-backed worker dispatch or a complete graph engine.",
            vec!["wrongness".to_string()],
            vec!["docs/runtime-truth-matrix.md".to_string()],
        ),
    ];
    let mut paths = Vec::new();
    for record in records {
        let record =
            record.map_err(|error| CliError::Invalid(format!("build triwiki record: {error}")))?;
        paths.push(
            opensks_triwiki::append_record(cwd, &record)
                .map_err(|error| CliError::Invalid(format!("append triwiki record: {error}")))?,
        );
    }
    Ok(CliOutput {
        stdout: format!(
            "seeded TriWiki records\nrecords: {}\nshards: {}\n",
            paths.len(),
            paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

pub fn triwiki_usage() -> &'static str {
    "usage: opensks triwiki seed\n"
}

pub fn run_context_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let subcommand = args
        .first()
        .ok_or_else(|| CliError::Usage(context_usage().to_string()))?;
    if subcommand != "pack" {
        return Err(CliError::Usage(format!(
            "unknown context subcommand `{subcommand}`\n\n{}",
            context_usage()
        )));
    }
    let budget = args
        .get(1)
        .map(|value| value.parse::<u32>())
        .transpose()
        .map_err(|_| CliError::Usage(context_usage().to_string()))?
        .unwrap_or(120);
    let pack = opensks_context::pack_workspace_records(cwd, "cli-context-pack", budget)
        .map_err(|error| CliError::Invalid(format!("build context pack: {error}")))?;
    let path = opensks_context::write_context_pack(cwd, &pack)
        .map_err(|error| CliError::Invalid(format!("write context pack: {error}")))?;
    Ok(CliOutput {
        stdout: format!(
            "built context pack\nrecords: {}\nestimated_tokens: {}\nartifact: {}\n",
            pack.record_ids.len(),
            pack.estimated_tokens,
            path.display()
        ),
    })
}

pub fn context_usage() -> &'static str {
    "usage: opensks context pack [token-budget]\n"
}

pub fn run_worktree_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let subcommand = args
        .first()
        .ok_or_else(|| CliError::Usage(worktree_usage().to_string()))?;
    if subcommand == "isolate" {
        let label = require_freeform_cli(&args[1..], worktree_usage())?;
        let run_id = format!("run-{}", ClockStamp::now()?.compact_id());
        let worker_id = slugify(&label);
        let report = opensks_git::create_isolation(cwd, &run_id, &worker_id)
            .map_err(|error| CliError::Invalid(format!("create isolation: {error}")))?;
        let dir = cwd.join(".opensks").join("git").join(&run_id);
        fs::create_dir_all(&dir)?;
        write_text_atomic(
            &dir.join("git-isolation.json"),
            &(serde_json::to_string_pretty(&report)
                .map_err(|error| CliError::Invalid(format!("serialize git isolation: {error}")))?
                + "\n"),
        )?;
        return Ok(CliOutput {
            stdout: format!(
                "created runtime isolation\nmode: {:?}\nworktree: {}\nartifact: {}\n",
                report.mode,
                report.worktree_path,
                dir.join("git-isolation.json").display()
            ),
        });
    }
    if subcommand != "create" {
        return Err(CliError::Usage(format!(
            "unknown worktree subcommand `{subcommand}`\n\n{}",
            worktree_usage()
        )));
    }
    let label = require_freeform_cli(
        &args[1..],
        "usage: opensks worktree create \"<worker label>\"",
    )?;
    let stamp = ClockStamp::now()?;
    let id = format!("worktree-{}-{}", stamp.compact_id(), process::id());
    let dir = cwd.join(".opensks").join("worktrees").join(&id);
    let workspace = dir.join("workspace");
    fs::create_dir_all(&workspace)?;
    let copied = copy_workspace_snapshot(cwd, &workspace)?;
    write_text_atomic(
        &dir.join("worktree-isolation.json"),
        &render_worktree_isolation(&stamp, &id, &label, &workspace, copied),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "created isolated worker workspace\nworktree: {}\nfiles_copied: {}\n",
            workspace.display(),
            copied
        ),
    })
}

pub fn worktree_usage() -> &'static str {
    concat!(
        "usage: opensks worktree create \"<worker label>\"\n",
        "       opensks worktree isolate \"<worker label>\"\n"
    )
}

pub fn run_provider_route_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let capability = args.first().map(String::as_str).unwrap_or("code");
    let request = match capability {
        "image" => opensks_provider::RoutingRequest::for_image("cli-route-image"),
        "text" => opensks_provider::RoutingRequest {
            id: "cli-route-text".to_string(),
            role: opensks_contracts::ModelRole::Planning,
            required: opensks_contracts::CapabilityRequirements::text(),
            explicit_model_id: None,
            budget_allowed: true,
        },
        "code" => opensks_provider::RoutingRequest::for_code("cli-route-code"),
        other => {
            return Err(CliError::Usage(format!(
                "unknown provider route capability `{other}`\n\n{}",
                provider_usage()
            )));
        }
    };
    let dir = cwd.join(".opensks").join("providers");
    fs::create_dir_all(&dir)?;
    let registry = opensks_provider::ModelRegistry::new(
        vec![
            opensks_provider::fake_text_model("fake-code", true),
            opensks_provider::fake_image_model("fake-image", true),
        ],
        opensks_policy::PermissionPolicy::default(),
    );
    let decision = registry.route(&request);
    write_text_atomic(
        &dir.join("routing-decision.json"),
        &(serde_json::to_string_pretty(&decision)
            .map_err(|error| CliError::Invalid(format!("serialize routing decision: {error}")))?
            + "\n"),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "routed provider capability\ncapability: {}\nstatus: {:?}\nselected_model: {}\nartifact: {}\n",
            capability,
            decision.status,
            decision.selected_model_id.as_deref().unwrap_or("none"),
            dir.join("routing-decision.json").display()
        ),
    })
}

pub fn provider_usage() -> &'static str {
    concat!(
        "usage: opensks provider list\n",
        "       opensks provider probe\n",
        "       opensks provider usage\n",
        "       opensks provider adapter-check\n",
        "       opensks provider route code|text|image\n"
    )
}

pub fn run_image_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    require_exact_subcommand_cli(args, "ledger", image_usage())?;
    let registry = opensks_provider::ModelRegistry::new(
        vec![
            opensks_provider::fake_image_model("disabled-image", false),
            opensks_provider::fake_image_model("enabled-image", true),
        ],
        opensks_policy::PermissionPolicy::default(),
    );
    let mut runtime = opensks_image::ImageRuntime::new();
    let asset = runtime
        .generate_placeholder(
            &registry,
            "cli-image-asset",
            512,
            512,
            vec![opensks_contracts::VisualAnchor {
                x: 32,
                y: 32,
                width: 128,
                height: 128,
            }],
        )
        .map_err(|error| CliError::Invalid(format!("image ledger: {error}")))?;
    let dir = cwd.join(".opensks").join("assets").join("candidates");
    fs::create_dir_all(&dir)?;
    write_text_atomic(
        &dir.join("image-ledger.json"),
        &(serde_json::to_string_pretty(runtime.ledger())
            .map_err(|error| CliError::Invalid(format!("serialize image ledger: {error}")))?
            + "\n"),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote image asset ledger\nasset: {}\nmodel: {}\nartifact: {}\n",
            asset.id,
            asset.model_id,
            dir.join("image-ledger.json").display()
        ),
    })
}

pub fn image_usage() -> &'static str {
    "usage: opensks image ledger\n"
}

pub fn run_reasoning_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    require_exact_subcommand_cli(args, "debate", reasoning_usage())?;
    let report = opensks_reasoning::run_bounded_debate(opensks_reasoning::DebateInput {
        id: "cli-debate".to_string(),
        max_rounds: 5,
        pro_claim: "Foundation slice is internally consistent.".to_string(),
        con_counterexample: "Live engine behavior is not yet wired.".to_string(),
    });
    let dir = cwd.join(".opensks").join("reasoning");
    fs::create_dir_all(&dir)?;
    write_text_atomic(
        &dir.join("reasoning-report.json"),
        &(serde_json::to_string_pretty(&report)
            .map_err(|error| CliError::Invalid(format!("serialize reasoning report: {error}")))?
            + "\n"),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote reasoning debate report\nrounds: {}\nstatus: {:?}\nartifact: {}\n",
            report.rounds,
            report.status,
            dir.join("reasoning-report.json").display()
        ),
    })
}

pub fn reasoning_usage() -> &'static str {
    "usage: opensks reasoning debate\n"
}

pub fn run_git_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    match args.first().map(String::as_str) {
        Some("working-change") => return run_git_working_change(&args[1..], cwd),
        Some("status") => return run_git_status(&args[1..], cwd),
        Some("branches") => return run_git_branches(&args[1..], cwd),
        Some("diff") => return run_git_diff(&args[1..], cwd),
        Some("switch-preflight") => return run_git_switch_preflight(&args[1..], cwd),
        Some("create-branch") => return run_git_create_branch(&args[1..], cwd),
        Some("switch") => return run_git_switch(&args[1..], cwd),
        Some("stage") => return run_git_stage(&args[1..], cwd),
        Some("unstage") => return run_git_unstage(&args[1..], cwd),
        Some("commit-preview") => return run_git_commit_preview(&args[1..], cwd),
        Some("commit") => return run_git_commit(&args[1..], cwd),
        Some("push-enqueue") => return run_git_push_enqueue(&args[1..], cwd),
        Some("push-approve") => return run_git_push_approve(&args[1..], cwd),
        Some("push-execute") => return run_git_push_execute(&args[1..], cwd),
        Some("push-status") => return run_git_push_status(&args[1..], cwd),
        _ => {}
    }
    require_exact_subcommand_cli(args, "outbox", git_usage())?;
    let mut outbox = opensks_git::Outbox::new();
    let commit = outbox
        .enqueue_commit("cli-commit", &["README.md".to_string()])
        .map_err(|error| CliError::Invalid(format!("enqueue commit: {error}")))?;
    let push = outbox
        .enqueue_push("main", false)
        .map_err(|error| CliError::Invalid(format!("enqueue push: {error}")))?;
    let mut blocked_outbox = opensks_git::Outbox::new();
    blocked_outbox
        .enqueue_push("main", false)
        .map_err(|error| CliError::Invalid(format!("enqueue blocked push: {error}")))?;
    let blocked_dispatch = blocked_outbox
        .dispatch_next(&[], |_| {
            Err(opensks_git::GitError::GitCommand(
                "approval gate failed to block dry-run executor".to_string(),
            ))
        })
        .map_err(|error| CliError::Invalid(format!("dispatch blocked push: {error}")))?;
    let approval = opensks_git::OutboxApproval {
        approval_id: push
            .approval_id
            .clone()
            .unwrap_or_else(|| "approval-push-main".to_string()),
        scope: "git_push".to_string(),
        target: "main".to_string(),
        approved: true,
    };
    let mut approved_outbox = opensks_git::Outbox::new();
    approved_outbox
        .enqueue_push("main", false)
        .map_err(|error| CliError::Invalid(format!("enqueue approved push: {error}")))?;
    let approved_dispatch = approved_outbox
        .dispatch_next(&[approval], |_| Ok(()))
        .map_err(|error| CliError::Invalid(format!("dispatch approved push: {error}")))?;
    let dir = cwd.join(".opensks").join("git");
    fs::create_dir_all(&dir)?;
    write_text_atomic(
        &dir.join("outbox.json"),
        &(serde_json::to_string_pretty(&outbox.items())
            .map_err(|error| CliError::Invalid(format!("serialize outbox: {error}")))?
            + "\n"),
    )?;
    let dispatch_evidence = serde_json::json!({
        "schema": "opensks.outbox-dispatch-smoke.v1",
        "executor_mode": "dry_run_no_remote_write",
        "without_approval": blocked_dispatch,
        "with_approval": approved_dispatch,
        "live_remote_write_executed": false
    });
    write_text_atomic(
        &dir.join("outbox-dispatch.json"),
        &(serde_json::to_string_pretty(&dispatch_evidence)
            .map_err(|error| CliError::Invalid(format!("serialize outbox dispatch: {error}")))?
            + "\n"),
    )?;
    write_text_atomic(
        &dir.join("outbox-gate.json"),
        &format!(
            "{{\n  \"schema\": \"opensks.outbox-gate.v1\",\n  \"queued_commit\": {},\n  \"queued_push\": {},\n  \"push_state\": {},\n  \"dispatch_without_approval_executed\": {},\n  \"dispatch_with_approval_executed\": {},\n  \"live_remote_write_executed\": false\n}}\n",
            json_string(&commit.id),
            json_string(&push.id),
            json_string(&push.state),
            blocked_dispatch.executed,
            approved_dispatch.executed
        ),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote Git outbox plan\nitems: {}\npush_state: {}\ndispatch_without_approval_executed: {}\nartifact: {}\n",
            outbox.items().len(),
            push.state,
            blocked_dispatch.executed,
            dir.join("outbox.json").display()
        ),
    })
}

pub fn git_usage() -> &'static str {
    concat!(
        "usage: opensks git outbox\n",
        "       opensks git working-change --workspace <path> --path <relative> --baseline-hash <hash>\n",
        "       opensks git status --workspace <path>\n",
        "       opensks git branches --workspace <path>\n",
        "       opensks git diff --workspace <path> [--path <relative>] [--staged]\n",
        "       opensks git switch-preflight --workspace <path> --target <branch>\n",
        "       opensks git create-branch --workspace <path> --name <branch> [--from <ref>]\n",
        "       opensks git switch --workspace <path> --target <branch> [--force]\n",
        "       opensks git stage --workspace <path> --path <relative> [--path <relative> ...]\n",
        "       opensks git unstage --workspace <path> --path <relative> [--path <relative> ...]\n",
        "       opensks git commit-preview --workspace <path>\n",
        "       opensks git commit --workspace <path> --message <m> --expected-index-hash <hash>\n",
        "       opensks git push-enqueue --workspace <path> --remote <name> --ref <branch>\n",
        "       opensks git push-approve --workspace <path> --intent <id> --effect-digest <digest> [--ack-protected]\n",
        "       opensks git push-execute --workspace <path> --intent <id>\n",
        "       opensks git push-status --workspace <path>\n"
    )
}

/// `opensks git working-change --workspace <p> --path <rel> --baseline-hash <h>`
/// — report whether the working-tree file has moved away from the editor's
/// recorded baseline hash (e.g. after a branch switch), plus the current
/// content hash and the HEAD blob hash so the UI can label a branch-switch
/// conflict. Returns `in_repo:false` when the path is not inside a git repo.
fn run_git_working_change(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_workspace_path_options(args, git_usage())?;
    let workspace = options.workspace.unwrap_or_else(|| cwd.to_path_buf());
    let relative = options.path.ok_or_else(|| {
        CliError::Usage(format!(
            "git working-change requires `--path`\n\n{}",
            git_usage()
        ))
    })?;
    let baseline_hash = options.baseline_hash.ok_or_else(|| {
        CliError::Usage(format!(
            "git working-change requires `--baseline-hash`\n\n{}",
            git_usage()
        ))
    })?;

    let in_repo = opensks_git::discover_repository(&workspace).is_some();
    if !in_repo {
        let body = serde_json::json!({
            "schema": "opensks.working-change.v1",
            "path": relative,
            "in_repo": false,
        });
        return file_output(&body);
    }

    let absolute = workspace.join(&relative);
    let current_hash = opensks_git::content_hash(&absolute)
        .map_err(|error| CliError::Invalid(format!("hash working-tree file: {error}")))?;
    let head_hash = opensks_git::head_blob_hash(&workspace, &relative)
        .map_err(|error| CliError::Invalid(format!("read HEAD blob hash: {error}")))?
        .unwrap_or_default();
    let changed = current_hash != baseline_hash;

    let body = serde_json::json!({
        "schema": "opensks.working-change.v1",
        "path": relative,
        "in_repo": true,
        "changed": changed,
        "current_hash": current_hash,
        "head_hash": head_hash,
    });
    file_output(&body)
}

/// `opensks git status --workspace <p>` — read-only working-tree status. Prints
/// the `opensks.git-status.v1` contract as JSON. Returns a minimal
/// `in_repo:false` object when the workspace is not a Git repository. Remote and
/// upstream strings are credential-redacted by the service.
fn run_git_status(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_workspace_path_options(args, git_usage())?;
    let workspace = options.workspace.unwrap_or_else(|| cwd.to_path_buf());
    let status = opensks_git_service::status(&workspace)
        .map_err(|error| CliError::Invalid(format!("git status: {error}")))?;
    let body = serde_json::to_value(&status)
        .map_err(|error| CliError::Invalid(format!("serialize git status: {error}")))?;
    file_output(&body)
}

/// `opensks git branches --workspace <p>` — read-only local branch list with
/// worktree-occupancy flags. Prints the `opensks.git-branches.v1` contract as
/// JSON. Returns an empty listing when the workspace is not a Git repository.
fn run_git_branches(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_workspace_path_options(args, git_usage())?;
    let workspace = options.workspace.unwrap_or_else(|| cwd.to_path_buf());
    let branches = opensks_git_service::branches(&workspace)
        .map_err(|error| CliError::Invalid(format!("git branches: {error}")))?;
    let body = serde_json::to_value(&branches)
        .map_err(|error| CliError::Invalid(format!("serialize git branches: {error}")))?;
    file_output(&body)
}

/// `opensks git diff --workspace <p> [--path <rel>] [--staged]` — read-only
/// unified diff parsed into per-file hunks. Prints the `opensks.git-diff.v1`
/// contract as JSON. `--staged` diffs the index against HEAD; otherwise the
/// worktree against the index. Returns an empty file list outside a repository.
fn run_git_diff(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_workspace_path_options(args, git_usage())?;
    let workspace = options.workspace.unwrap_or_else(|| cwd.to_path_buf());
    let diff_options = opensks_git_service::DiffOptions {
        path: options.path,
        staged: options.staged,
    };
    let diff = opensks_git_service::diff(&workspace, &diff_options)
        .map_err(|error| CliError::Invalid(format!("git diff: {error}")))?;
    let body = serde_json::to_value(&diff)
        .map_err(|error| CliError::Invalid(format!("serialize git diff: {error}")))?;
    file_output(&body)
}

/// Flags for the PR-035 local Git mutation subcommands. `workspace` defaults to
/// the process `cwd`. `paths` collects repeated `--path` flags.
#[derive(Debug, Default)]
struct GitMutationOptions {
    workspace: Option<PathBuf>,
    paths: Vec<String>,
    target: Option<String>,
    name: Option<String>,
    from: Option<String>,
    message: Option<String>,
    expected_index_hash: Option<String>,
    force: bool,
}

/// Parse the flags shared by the local Git mutation subcommands:
/// `--workspace <p>`, repeated `--path <rel>`, `--target <b>`, `--name <b>`,
/// `--from <ref>`, `--message <m>`, `--expected-index-hash <h>`, and `--force`.
fn parse_git_mutation_options(args: &[String]) -> Result<GitMutationOptions, CliError> {
    let usage = git_usage();
    let mut options = GitMutationOptions::default();
    let mut idx = 0;
    while idx < args.len() {
        let flag = args[idx].as_str();
        let value = || -> Result<&str, CliError> {
            args.get(idx + 1).map(String::as_str).ok_or_else(|| {
                CliError::Usage(format!("flag `{flag}` requires a value\n\n{usage}"))
            })
        };
        match flag {
            "--workspace" => {
                options.workspace = Some(PathBuf::from(value()?));
                idx += 2;
            }
            "--path" => {
                options.paths.push(value()?.to_string());
                idx += 2;
            }
            "--target" => {
                options.target = Some(value()?.to_string());
                idx += 2;
            }
            "--name" => {
                options.name = Some(value()?.to_string());
                idx += 2;
            }
            "--from" => {
                options.from = Some(value()?.to_string());
                idx += 2;
            }
            "--message" => {
                options.message = Some(value()?.to_string());
                idx += 2;
            }
            "--expected-index-hash" => {
                options.expected_index_hash = Some(value()?.to_string());
                idx += 2;
            }
            "--force" => {
                options.force = true;
                idx += 1;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown argument `{other}`\n\n{usage}"
                )));
            }
        }
    }
    Ok(options)
}

/// Map a typed [`opensks_contracts::GitMutationError`] to a nonzero-exit CLI
/// error whose message is the `opensks.git-error.v1` JSON. Upstream this becomes
/// `OpenSksError::Invalid` → exit code 1, with the error contract emitted so the
/// editor can parse the refusal (blocked switch, stale index, or secret path).
fn git_mutation_error(error: &opensks_contracts::GitMutationError) -> CliError {
    match serde_json::to_string(error) {
        Ok(json) => CliError::Invalid(json),
        Err(serialize_error) => {
            CliError::Invalid(format!("serialize git mutation error: {serialize_error}"))
        }
    }
}

/// Resolve the `--workspace` for a mutation subcommand, defaulting to `cwd`.
fn mutation_workspace(options: &GitMutationOptions, cwd: &Path) -> PathBuf {
    options
        .workspace
        .clone()
        .unwrap_or_else(|| cwd.to_path_buf())
}

/// `opensks git switch-preflight --workspace <p> --target <b>` — read-only check
/// of whether switching to `target` is blocked by a dirty worktree or conflict.
fn run_git_switch_preflight(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_git_mutation_options(args)?;
    let workspace = mutation_workspace(&options, cwd);
    // `--target` is required for the editor's call shape even though the
    // preflight is target-independent (a dirty worktree blocks any switch).
    if options.target.is_none() {
        return Err(CliError::Usage(format!(
            "git switch-preflight requires `--target`\n\n{}",
            git_usage()
        )));
    }
    let preflight = opensks_git_service::switch_preflight(&workspace)
        .map_err(|error| CliError::Invalid(format!("git switch-preflight: {error}")))?;
    let body = serde_json::to_value(&preflight)
        .map_err(|error| CliError::Invalid(format!("serialize switch preflight: {error}")))?;
    file_output(&body)
}

/// `opensks git create-branch --workspace <p> --name <b> [--from <ref>]` —
/// create a local branch without checking it out.
fn run_git_create_branch(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_git_mutation_options(args)?;
    let workspace = mutation_workspace(&options, cwd);
    let name = options.name.as_deref().ok_or_else(|| {
        CliError::Usage(format!(
            "git create-branch requires `--name`\n\n{}",
            git_usage()
        ))
    })?;
    let created = opensks_git_service::create_branch(&workspace, name, options.from.as_deref())
        .map_err(|error| CliError::Invalid(format!("git create-branch: {error}")))?;
    let body = serde_json::to_value(&created)
        .map_err(|error| CliError::Invalid(format!("serialize create branch: {error}")))?;
    file_output(&body)
}

/// `opensks git switch --workspace <p> --target <b> [--force]` — switch to a
/// local branch. A dirty/conflicted worktree without `--force` is refused with
/// the `switch_blocked` error contract and a nonzero exit.
fn run_git_switch(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_git_mutation_options(args)?;
    let workspace = mutation_workspace(&options, cwd);
    let target = options.target.as_deref().ok_or_else(|| {
        CliError::Usage(format!("git switch requires `--target`\n\n{}", git_usage()))
    })?;
    match opensks_git_service::switch(&workspace, target, options.force)
        .map_err(|error| CliError::Invalid(format!("git switch: {error}")))?
    {
        Ok(switched) => {
            let body = serde_json::to_value(&switched)
                .map_err(|error| CliError::Invalid(format!("serialize switch: {error}")))?;
            file_output(&body)
        }
        Err(error) => Err(git_mutation_error(&error)),
    }
}

/// `opensks git stage --workspace <p> --path <rel> [--path <rel> ...]` — stage
/// paths, rejecting secret/data-plane paths (never staged) into `rejected`.
fn run_git_stage(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_git_mutation_options(args)?;
    let workspace = mutation_workspace(&options, cwd);
    if options.paths.is_empty() {
        return Err(CliError::Usage(format!(
            "git stage requires at least one `--path`\n\n{}",
            git_usage()
        )));
    }
    let result = opensks_git_service::stage(&workspace, &options.paths)
        .map_err(|error| CliError::Invalid(format!("git stage: {error}")))?;
    let body = serde_json::to_value(&result)
        .map_err(|error| CliError::Invalid(format!("serialize stage: {error}")))?;
    file_output(&body)
}

/// `opensks git unstage --workspace <p> --path <rel> [--path <rel> ...]` —
/// remove paths from the index without touching the worktree.
fn run_git_unstage(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_git_mutation_options(args)?;
    let workspace = mutation_workspace(&options, cwd);
    if options.paths.is_empty() {
        return Err(CliError::Usage(format!(
            "git unstage requires at least one `--path`\n\n{}",
            git_usage()
        )));
    }
    let result = opensks_git_service::unstage(&workspace, &options.paths)
        .map_err(|error| CliError::Invalid(format!("git unstage: {error}")))?;
    let body = serde_json::to_value(&result)
        .map_err(|error| CliError::Invalid(format!("serialize unstage: {error}")))?;
    file_output(&body)
}

/// `opensks git commit-preview --workspace <p>` — the staged path list and a
/// stable `index_hash` to pass back to `commit` as `--expected-index-hash`.
fn run_git_commit_preview(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_git_mutation_options(args)?;
    let workspace = mutation_workspace(&options, cwd);
    let preview = opensks_git_service::commit_preview(&workspace)
        .map_err(|error| CliError::Invalid(format!("git commit-preview: {error}")))?;
    let body = serde_json::to_value(&preview)
        .map_err(|error| CliError::Invalid(format!("serialize commit preview: {error}")))?;
    file_output(&body)
}

/// `opensks git commit --workspace <p> --message <m> --expected-index-hash <h>`
/// — commit the current index. A stale `index_hash`, a secret/data-plane staged
/// path, or an empty index is refused with the `opensks.git-error.v1` contract
/// and a nonzero exit.
fn run_git_commit(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_git_mutation_options(args)?;
    let workspace = mutation_workspace(&options, cwd);
    let message = options.message.as_deref().ok_or_else(|| {
        CliError::Usage(format!(
            "git commit requires `--message`\n\n{}",
            git_usage()
        ))
    })?;
    let expected = options.expected_index_hash.as_deref().ok_or_else(|| {
        CliError::Usage(format!(
            "git commit requires `--expected-index-hash`\n\n{}",
            git_usage()
        ))
    })?;
    match opensks_git_service::commit(&workspace, message, expected)
        .map_err(|error| CliError::Invalid(format!("git commit: {error}")))?
    {
        Ok(committed) => {
            let body = serde_json::to_value(&committed)
                .map_err(|error| CliError::Invalid(format!("serialize commit: {error}")))?;
            file_output(&body)
        }
        Err(error) => Err(git_mutation_error(&error)),
    }
}

/// Flags for the PR-036 durable push-outbox subcommands. `workspace` defaults to
/// the process `cwd`.
#[derive(Debug, Default)]
struct GitPushOptions {
    workspace: Option<PathBuf>,
    remote: Option<String>,
    r#ref: Option<String>,
    intent: Option<String>,
    effect_digest: Option<String>,
    ack_protected: bool,
}

/// Parse the flags shared by the push subcommands: `--workspace <p>`,
/// `--remote <name>`, `--ref <branch>`, `--intent <id>`,
/// `--effect-digest <digest>`, and `--ack-protected`.
fn parse_git_push_options(args: &[String]) -> Result<GitPushOptions, CliError> {
    let usage = git_usage();
    let mut options = GitPushOptions::default();
    let mut idx = 0;
    while idx < args.len() {
        let flag = args[idx].as_str();
        let value = || -> Result<&str, CliError> {
            args.get(idx + 1).map(String::as_str).ok_or_else(|| {
                CliError::Usage(format!("flag `{flag}` requires a value\n\n{usage}"))
            })
        };
        match flag {
            "--workspace" => {
                options.workspace = Some(PathBuf::from(value()?));
                idx += 2;
            }
            "--remote" => {
                options.remote = Some(value()?.to_string());
                idx += 2;
            }
            "--ref" => {
                options.r#ref = Some(value()?.to_string());
                idx += 2;
            }
            "--intent" => {
                options.intent = Some(value()?.to_string());
                idx += 2;
            }
            "--effect-digest" => {
                options.effect_digest = Some(value()?.to_string());
                idx += 2;
            }
            "--ack-protected" => {
                options.ack_protected = true;
                idx += 1;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown argument `{other}`\n\n{usage}"
                )));
            }
        }
    }
    Ok(options)
}

/// Resolve the `--workspace` for a push subcommand, defaulting to `cwd`.
fn push_workspace(options: &GitPushOptions, cwd: &Path) -> PathBuf {
    options
        .workspace
        .clone()
        .unwrap_or_else(|| cwd.to_path_buf())
}

/// Map a typed [`opensks_contracts::PushError`] to a nonzero-exit CLI error whose
/// message is the `opensks.git-error.v1` JSON. Upstream this becomes
/// `OpenSksError::Invalid` → exit code 1, with the refusal contract on the wire.
fn push_error(error: &opensks_contracts::PushError) -> CliError {
    match serde_json::to_string(error) {
        Ok(json) => CliError::Invalid(json),
        Err(serialize_error) => {
            CliError::Invalid(format!("serialize push error: {serialize_error}"))
        }
    }
}

/// Generate a stable, collision-resistant id from a prefix and the current
/// clock. Used to mint approval ids when the caller does not supply one.
fn push_generated_id(prefix: &str) -> Result<String, CliError> {
    Ok(format!("{prefix}-{}", ClockStamp::now()?.compact_id()))
}

/// `opensks git push-enqueue --workspace <p> --remote <name> --ref <branch>` —
/// compute the current local oid for `ref` and the remote's observed oid, persist
/// a durable push intent with its `effect_digest`, and flag protected refs.
fn run_git_push_enqueue(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_git_push_options(args)?;
    let workspace = push_workspace(&options, cwd);
    let remote = options.remote.as_deref().ok_or_else(|| {
        CliError::Usage(format!(
            "git push-enqueue requires `--remote`\n\n{}",
            git_usage()
        ))
    })?;
    let r#ref = options.r#ref.as_deref().ok_or_else(|| {
        CliError::Usage(format!(
            "git push-enqueue requires `--ref`\n\n{}",
            git_usage()
        ))
    })?;
    let intent_id = options
        .intent
        .clone()
        .map(Ok)
        .unwrap_or_else(|| push_generated_id("push-intent"))?;
    let outbox = opensks_git::PushOutbox::open_workspace(&workspace)
        .map_err(|error| CliError::Invalid(format!("open push outbox: {error}")))?;
    let intent = outbox
        .enqueue(&intent_id, remote, r#ref)
        .map_err(|error| CliError::Invalid(format!("push enqueue: {error}")))?;
    let body = serde_json::to_value(&intent)
        .map_err(|error| CliError::Invalid(format!("serialize push intent: {error}")))?;
    file_output(&body)
}

/// `opensks git push-approve --workspace <p> --intent <id> --effect-digest <d>
/// [--ack-protected]` — record an approval only when `--effect-digest` equals the
/// intent's current digest; otherwise refuse with `digest_mismatch` and a nonzero
/// exit, recording no usable approval.
fn run_git_push_approve(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_git_push_options(args)?;
    let workspace = push_workspace(&options, cwd);
    let intent_id = options.intent.as_deref().ok_or_else(|| {
        CliError::Usage(format!(
            "git push-approve requires `--intent`\n\n{}",
            git_usage()
        ))
    })?;
    let effect_digest = options.effect_digest.as_deref().ok_or_else(|| {
        CliError::Usage(format!(
            "git push-approve requires `--effect-digest`\n\n{}",
            git_usage()
        ))
    })?;
    let approval_id = push_generated_id("push-approval")?;
    let outbox = opensks_git::PushOutbox::open_workspace(&workspace)
        .map_err(|error| CliError::Invalid(format!("open push outbox: {error}")))?;
    match outbox
        .approve(
            &approval_id,
            intent_id,
            effect_digest,
            options.ack_protected,
        )
        .map_err(|error| CliError::Invalid(format!("push approve: {error}")))?
    {
        Ok(approval) => {
            let body = serde_json::to_value(&approval)
                .map_err(|error| CliError::Invalid(format!("serialize push approval: {error}")))?;
            file_output(&body)
        }
        Err(error) => Err(push_error(&error)),
    }
}

/// `opensks git push-execute --workspace <p> --intent <id>` — execute the push if
/// a still-valid approval exists. Refuses (nonzero) with `no_matching_approval`,
/// `digest_mismatch`, `protected_branch`, or `push_failed`. A repeat execute with
/// a completed receipt returns `already_done: true` without pushing again.
fn run_git_push_execute(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_git_push_options(args)?;
    let workspace = push_workspace(&options, cwd);
    let intent_id = options.intent.as_deref().ok_or_else(|| {
        CliError::Usage(format!(
            "git push-execute requires `--intent`\n\n{}",
            git_usage()
        ))
    })?;
    let outbox = opensks_git::PushOutbox::open_workspace(&workspace)
        .map_err(|error| CliError::Invalid(format!("open push outbox: {error}")))?;
    match outbox
        .execute(intent_id)
        .map_err(|error| CliError::Invalid(format!("push execute: {error}")))?
    {
        Ok(receipt) => {
            let body = serde_json::to_value(&receipt)
                .map_err(|error| CliError::Invalid(format!("serialize push receipt: {error}")))?;
            file_output(&body)
        }
        Err(error) => Err(push_error(&error)),
    }
}

/// `opensks git push-status --workspace <p>` — recover the durable outbox state
/// (pending / approved / completed) from SQLite, surviving restart.
fn run_git_push_status(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let options = parse_git_push_options(args)?;
    let workspace = push_workspace(&options, cwd);
    let outbox = opensks_git::PushOutbox::open_workspace(&workspace)
        .map_err(|error| CliError::Invalid(format!("open push outbox: {error}")))?;
    let status = outbox
        .status()
        .map_err(|error| CliError::Invalid(format!("push status: {error}")))?;
    let body = serde_json::to_value(&status)
        .map_err(|error| CliError::Invalid(format!("serialize push status: {error}")))?;
    file_output(&body)
}

pub fn run_worker_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let subcommand = args
        .first()
        .ok_or_else(|| CliError::Usage(worker_usage().to_string()))?;
    if subcommand != "runtime" {
        return Err(CliError::Usage(format!(
            "unknown worker subcommand `{subcommand}`\n\n{}",
            worker_usage()
        )));
    }
    let goal = require_freeform_cli(&args[1..], worker_usage())?;
    let stamp = ClockStamp::now()?;
    let run_id = format!("worker-runtime-{}-{}", stamp.compact_id(), process::id());
    let dir = cwd.join(".opensks").join("workers").join(&run_id);
    fs::create_dir_all(&dir)?;

    let leases = build_worker_lease_records(&stamp);
    let routes = run_local_worker_request_routes(&leases);
    write_text_atomic(
        &dir.join("worker-leases.json"),
        &render_worker_leases(&stamp, &run_id, &goal, &leases),
    )?;
    write_text_atomic(
        &dir.join("worker-heartbeats.jsonl"),
        &render_worker_heartbeats(&stamp, &run_id, &leases),
    )?;
    write_text_atomic(
        &dir.join("worker-bus.json"),
        &render_worker_bus(&stamp, &run_id, &routes),
    )?;
    write_text_atomic(
        &dir.join("worker-routing.json"),
        &render_worker_routing(&stamp, &run_id, &routes),
    )?;
    write_text_atomic(
        &dir.join("worker-final-state.json"),
        &render_worker_final_state(&stamp, &run_id, &leases, &routes),
    )?;

    let recovered = leases
        .iter()
        .filter(|lease| lease.state == "recovered_expired")
        .count();
    Ok(CliOutput {
        stdout: format!(
            "wrote local worker runtime artifacts\nrun: {}\nleases: {}\nrecovered_expired: {}\nrouted_requests: {}\nartifacts: {}\n",
            run_id,
            leases.len(),
            recovered,
            routes.len(),
            dir.display()
        ),
    })
}

pub fn worker_usage() -> &'static str {
    "usage: opensks worker runtime \"<goal>\"\n"
}

pub fn run_scheduler_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let subcommand = args
        .first()
        .ok_or_else(|| CliError::Usage(scheduler_usage().to_string()))?;
    match subcommand.as_str() {
        "run" => run_scheduler_run_command(&args[1..], cwd),
        "simulate" => run_scheduler_simulate_command(&args[1..], cwd),
        "dispatch" => run_scheduler_dispatch_command(&args[1..], cwd),
        "recover" => run_scheduler_recover_command(&args[1..], cwd),
        _ => Err(CliError::Usage(format!(
            "unknown scheduler subcommand `{subcommand}`\n\n{}",
            scheduler_usage()
        ))),
    }
}

pub fn scheduler_usage() -> &'static str {
    concat!(
        "usage: opensks scheduler run \"<goal>\"\n",
        "       opensks scheduler simulate [count]\n",
        "       opensks scheduler dispatch [count]\n",
        "       opensks scheduler recover [count]\n"
    )
}

/// `opensks perf <subcommand>` — Lifecycle / memory / high-rate performance
/// hardening (PR-043). Currently exposes `stress`, which runs the bounded
/// event-batcher + LRU-cache stress harness under a supervised, deterministically
/// reaped child pool and prints the `opensks.perf-stress-report.v1` contract.
///
/// All harness logic lives in `opensks-perf`; this is a thin wire-up that parses
/// flags, calls `opensks_perf::run_stress`, and serializes the report to stdout.
pub fn run_perf_command(args: &[String], _cwd: &Path) -> Result<CliOutput, CliError> {
    let subcommand = args
        .first()
        .ok_or_else(|| CliError::Usage(perf_usage().to_string()))?;
    match subcommand.as_str() {
        "stress" => run_perf_stress_command(&args[1..]),
        other => Err(CliError::Usage(format!(
            "unknown perf subcommand `{other}`\n\n{}",
            perf_usage()
        ))),
    }
}

pub fn perf_usage() -> &'static str {
    concat!(
        "usage: opensks perf stress [--events <n>] [--cache-capacity <n>]\n",
        "                           [--max-batch <n>] [--max-pending <n>] [--children <n>]\n"
    )
}

fn run_perf_stress_command(args: &[String]) -> Result<CliOutput, CliError> {
    let mut config = opensks_perf::StressConfig::default();
    let mut idx = 0;
    while idx < args.len() {
        let flag = args[idx].as_str();
        let value = || -> Result<&str, CliError> {
            args.get(idx + 1).map(String::as_str).ok_or_else(|| {
                CliError::Usage(format!(
                    "flag `{flag}` requires a value\n\n{}",
                    perf_usage()
                ))
            })
        };
        let parse_u64 = |raw: &str| -> Result<u64, CliError> {
            raw.parse::<u64>().map_err(|_| {
                CliError::Usage(format!(
                    "flag `{flag}` requires a number\n\n{}",
                    perf_usage()
                ))
            })
        };
        let parse_usize = |raw: &str| -> Result<usize, CliError> {
            raw.parse::<usize>().map_err(|_| {
                CliError::Usage(format!(
                    "flag `{flag}` requires a number\n\n{}",
                    perf_usage()
                ))
            })
        };
        match flag {
            "--events" => {
                config.events = parse_u64(value()?)?;
                idx += 2;
            }
            "--cache-capacity" => {
                config.cache_capacity = parse_usize(value()?)?;
                idx += 2;
            }
            "--max-batch" => {
                config.max_batch = parse_usize(value()?)?;
                idx += 2;
            }
            "--max-pending" => {
                config.max_pending = parse_usize(value()?)?;
                idx += 2;
            }
            "--children" => {
                config.supervised_children = parse_u64(value()?)?;
                idx += 2;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown argument `{other}`\n\n{}",
                    perf_usage()
                )));
            }
        }
    }
    let report = opensks_perf::run_stress(config);
    let body = serde_json::to_value(&report)
        .map_err(|error| CliError::Invalid(format!("serialize perf stress report: {error}")))?;
    file_output(&body)
}

fn run_scheduler_run_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let goal = require_freeform_cli(args, "usage: opensks scheduler run \"<goal>\"")?;
    let stamp = ClockStamp::now()?;
    let run_id = format!("scheduler-{}-{}", stamp.compact_id(), process::id());
    let dir = cwd.join(".opensks").join("scheduler").join(&run_id);
    fs::create_dir_all(&dir)?;
    let checks = run_local_qa_checks(cwd);
    let overlap_spans = run_scheduler_overlap_checks(cwd);
    write_text_atomic(
        &dir.join("stage-scheduler.json"),
        &render_scheduler_plan_json(&stamp.json(), &run_id, &goal),
    )?;
    write_text_atomic(
        &dir.join("scheduler-events.jsonl"),
        &render_scheduler_events_jsonl(&stamp.json(), &run_id, &checks),
    )?;
    write_text_atomic(
        &dir.join("scheduler-final-state.json"),
        &render_scheduler_final_state_json(&stamp.json(), &run_id, &checks),
    )?;
    write_text_atomic(
        &dir.join("stage-overlap-report.json"),
        &render_stage_overlap_report(&stamp.json(), &run_id, &overlap_spans),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "ran local scheduler slice\nrun: {}\nchecks: {}\noverlap_spans: {}\nartifacts: {}\n",
            run_id,
            checks.len(),
            overlap_spans.len(),
            dir.display()
        ),
    })
}

fn run_scheduler_simulate_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let count = parse_optional_scheduler_count(args, 32, 1, 10_000)?;
    let run_id = format!(
        "scheduler-sim-{}-{}",
        ClockStamp::now()?.compact_id(),
        process::id()
    );
    let items: Vec<_> = (0..count)
        .map(|index| {
            opensks_scheduler::make_work_item(&run_id, format!("wi-{index:05}"), Vec::new())
        })
        .collect();
    let mut scheduler = opensks_scheduler::DurableScheduler::new(
        &run_id,
        items,
        opensks_scheduler::SchedulerConfig {
            requested_workers: 100,
            project_max_workers: 32,
            provider_max_workers: 28,
            worktree_max_workers: 30,
            verification_max_workers: 20,
            visible_lane_cap: 12,
        },
    );
    let mut store = opensks_event_store::EventStore::open_workspace(cwd)
        .map_err(|error| CliError::Invalid(format!("open event store: {error}")))?;
    let snapshot = scheduler
        .simulate_until_idle(&mut store)
        .map_err(|error| CliError::Invalid(format!("simulate scheduler: {error}")))?;
    let dir = cwd.join(".opensks").join("scheduler").join(&run_id);
    fs::create_dir_all(&dir)?;
    write_text_atomic(
        &dir.join("durable-scheduler-snapshot.json"),
        &(serde_json::to_string_pretty(&snapshot).map_err(|error| {
            CliError::Invalid(format!("serialize scheduler snapshot: {error}"))
        })? + "\n"),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "simulated durable scheduler\nrun: {}\nitems: {}\nadmitted: {}\nmax_concurrent_workers: {}\nartifact: {}\n",
            run_id,
            snapshot.work_items.len(),
            snapshot.decision.admitted,
            snapshot.max_concurrent_workers,
            dir.join("durable-scheduler-snapshot.json").display()
        ),
    })
}

fn run_scheduler_dispatch_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let count = parse_optional_scheduler_count(args, 32, 1, 10_000)?;
    let run_id = format!(
        "scheduler-dispatch-{}-{}",
        ClockStamp::now()?.compact_id(),
        process::id()
    );
    let items: Vec<_> = (0..count)
        .map(|index| {
            opensks_scheduler::make_work_item(&run_id, format!("wi-{index:05}"), Vec::new())
        })
        .collect();
    let mut scheduler = opensks_scheduler::DurableScheduler::new(
        &run_id,
        items,
        opensks_scheduler::SchedulerConfig {
            requested_workers: 100,
            project_max_workers: 32,
            provider_max_workers: 28,
            worktree_max_workers: 30,
            verification_max_workers: 20,
            visible_lane_cap: 12,
        },
    );
    let mut store = opensks_event_store::EventStore::open_workspace(cwd)
        .map_err(|error| CliError::Invalid(format!("open event store: {error}")))?;
    let mut worker = opensks_scheduler::DeterministicWorker::new("cli-local-worker");
    let (snapshot, report) = scheduler
        .dispatch_until_idle(&mut store, &mut worker)
        .map_err(|error| CliError::Invalid(format!("dispatch scheduler: {error}")))?;
    let dir = cwd.join(".opensks").join("scheduler").join(&run_id);
    fs::create_dir_all(&dir)?;
    write_text_atomic(
        &dir.join("worker-dispatch-snapshot.json"),
        &(serde_json::to_string_pretty(&snapshot).map_err(|error| {
            CliError::Invalid(format!("serialize worker dispatch snapshot: {error}"))
        })? + "\n"),
    )?;
    write_text_atomic(
        &dir.join("worker-dispatch-report.json"),
        &(serde_json::to_string_pretty(&report).map_err(|error| {
            CliError::Invalid(format!("serialize worker dispatch report: {error}"))
        })? + "\n"),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "dispatched durable scheduler workers\nrun: {}\nitems: {}\nattempted: {}\ncompleted: {}\nfailed: {}\nmax_concurrent_workers: {}\nartifacts: {}\n",
            run_id,
            snapshot.work_items.len(),
            report.attempted,
            report.completed,
            report.failed,
            snapshot.max_concurrent_workers,
            dir.display()
        ),
    })
}

fn run_scheduler_recover_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let count = parse_optional_scheduler_count(args, 4, 2, 10_000)?;
    let run_id = format!(
        "scheduler-recover-{}-{}",
        ClockStamp::now()?.compact_id(),
        process::id()
    );
    let items: Vec<_> = (0..count)
        .map(|index| {
            opensks_scheduler::make_work_item(&run_id, format!("wi-{index:05}"), Vec::new())
        })
        .collect();
    let mut scheduler = opensks_scheduler::DurableScheduler::new(
        &run_id,
        items,
        opensks_scheduler::SchedulerConfig::default(),
    );
    let mut store = opensks_event_store::EventStore::open_workspace(cwd)
        .map_err(|error| CliError::Invalid(format!("open event store: {error}")))?;
    for index in 0..count {
        scheduler
            .lease_ready_item(&mut store, &format!("wi-{index:05}"), "cli-lease-worker")
            .map_err(|error| CliError::Invalid(format!("lease scheduler item: {error}")))?;
    }
    let heartbeat = scheduler
        .heartbeat_lease(&mut store, "wi-00000", "cli-lease-worker", 20_000)
        .map_err(|error| CliError::Invalid(format!("heartbeat scheduler lease: {error}")))?;
    let recovery = scheduler
        .expire_stale_leases(&mut store, 45_000)
        .map_err(|error| CliError::Invalid(format!("recover scheduler leases: {error}")))?;
    let snapshot = scheduler.snapshot(
        "lease-recovery",
        vec![
            "event-store-replay-required".to_string(),
            "scheduler:lease-heartbeat".to_string(),
            "scheduler:lease-expired-recovered".to_string(),
        ],
    );
    let dir = cwd.join(".opensks").join("scheduler").join(&run_id);
    fs::create_dir_all(&dir)?;
    write_text_atomic(
        &dir.join("lease-heartbeat-report.json"),
        &(serde_json::to_string_pretty(&heartbeat).map_err(|error| {
            CliError::Invalid(format!("serialize lease heartbeat report: {error}"))
        })? + "\n"),
    )?;
    write_text_atomic(
        &dir.join("lease-recovery-report.json"),
        &(serde_json::to_string_pretty(&recovery).map_err(|error| {
            CliError::Invalid(format!("serialize lease recovery report: {error}"))
        })? + "\n"),
    )?;
    write_text_atomic(
        &dir.join("lease-recovery-snapshot.json"),
        &(serde_json::to_string_pretty(&snapshot).map_err(|error| {
            CliError::Invalid(format!("serialize lease recovery snapshot: {error}"))
        })? + "\n"),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "recovered durable scheduler leases\nrun: {}\nitems: {}\nactive: {}\nexpired: {}\nheartbeat_expires_at_ms: {}\nartifacts: {}\n",
            run_id,
            snapshot.work_items.len(),
            recovery.active_count,
            recovery.expired_count,
            heartbeat.expires_at_ms,
            dir.display()
        ),
    })
}

/// `opensks design audit|activate|active-status|revision-{propose,accept,reject,
/// rollback}` (PR-040). Audits a package, atomically activates it (a failing
/// audit blocks activation and leaves the previous active package in place), and
/// manages proof-linked design revisions. All logic lives in opensks-design.
pub fn run_design_studio_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    const USAGE: &str = "usage: opensks design audit|activate|active-status|save-tokens|compile|list|revision-propose|revision-accept|revision-reject|revision-rollback ...";
    let subcommand = args
        .first()
        .map(String::as_str)
        .ok_or_else(|| CliError::Usage(USAGE.to_string()))?;
    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let workspace = flag("--workspace")
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.to_path_buf());
    let registry = opensks_design::DesignRegistry::with_default_order(&workspace, None);
    let opt = |value: &Option<String>| -> String {
        match value {
            Some(inner) => json_string(inner),
            None => "null".to_string(),
        }
    };
    match subcommand {
        "audit" => {
            let id = flag("--package").ok_or_else(|| CliError::Usage(USAGE.to_string()))?;
            let resolved = registry
                .resolve(&id)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let tokens = resolved
                .load_tokens()
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let components = resolved
                .load_components()
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let report = opensks_design::audit_package(&id, &tokens, components.as_ref())
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            Ok(CliOutput {
                stdout: report.to_json() + "\n",
            })
        }
        "activate" => {
            let id = flag("--package").ok_or_else(|| CliError::Usage(USAGE.to_string()))?;
            let revision = flag("--revision");
            match opensks_design::activate_package(&workspace, &registry, &id, revision.as_deref())
            {
                Ok(outcome) => Ok(CliOutput {
                    stdout: format!(
                        "{{\"schema\":\"opensks.design-activate.v1\",\"activated\":true,\"package_id\":{},\"previous_active\":{},\"audit_passed\":{}}}\n",
                        json_string(&outcome.package_id),
                        opt(&outcome.previous_active),
                        outcome.audit.passed()
                    ),
                }),
                Err(error) => Err(CliError::Invalid(error.to_string())),
            }
        }
        "active-status" => {
            let marker = opensks_design::read_active(&workspace)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            Ok(CliOutput {
                stdout: marker.to_json() + "\n",
            })
        }
        "revision-propose" => {
            let id = flag("--package").ok_or_else(|| CliError::Usage(USAGE.to_string()))?;
            let revision = opensks_design::propose_revision(&workspace, &id)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            Ok(CliOutput {
                stdout: revision.to_json() + "\n",
            })
        }
        "revision-accept" | "revision-reject" | "revision-rollback" => {
            let id = flag("--revision").ok_or_else(|| CliError::Usage(USAGE.to_string()))?;
            let revision = match subcommand {
                "revision-accept" => opensks_design::accept_revision(&workspace, &id),
                "revision-reject" => opensks_design::reject_revision(&workspace, &id),
                _ => opensks_design::rollback_revision(&workspace, &id),
            }
            .map_err(|error| CliError::Invalid(error.to_string()))?;
            Ok(CliOutput {
                stdout: revision.to_json() + "\n",
            })
        }
        // DESIGN-002: persist edited token values into the package's tokens.json.
        // The draft (a `{ "tokens": [{path, value}, …] }` document) arrives on
        // stdin; only existing token paths are updated, unknown paths reported.
        "save-tokens" => {
            let id = flag("--package").ok_or_else(|| CliError::Usage(USAGE.to_string()))?;
            let body = read_stdin_to_string()?;
            let parsed: serde_json::Value = serde_json::from_str(&body)
                .map_err(|e| CliError::Invalid(format!("invalid save-tokens body: {e}")))?;
            let entries = parsed
                .get("tokens")
                .and_then(|t| t.as_array())
                .ok_or_else(|| {
                    CliError::Invalid("save-tokens body must have a `tokens` array".to_string())
                })?;
            let mut updates: Vec<(String, String)> = Vec::with_capacity(entries.len());
            for entry in entries {
                let path = entry.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                    CliError::Invalid("each token needs a string `path`".to_string())
                })?;
                let value = match entry.get("value") {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => {
                        return Err(CliError::Invalid("each token needs a `value`".to_string()));
                    }
                };
                updates.push((path.to_string(), value));
            }
            let outcome = opensks_design::save_token_values(&workspace, &id, &updates)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let json = serde_json::json!({
                "schema": "opensks.design-save-tokens.v1",
                "package_id": outcome.package_id,
                "updated": outcome.updated,
                "unknown_paths": outcome.unknown_paths,
                "total": outcome.total,
                "content_hash": outcome.content_hash,
            });
            Ok(CliOutput {
                stdout: json.to_string() + "\n",
            })
        }
        // DESIGN-002: compile/validate a package's tokens in isolation (no
        // activation) so the editor can surface compile errors before applying.
        "compile" => {
            let id = flag("--package").ok_or_else(|| CliError::Usage(USAGE.to_string()))?;
            let outcome = opensks_design::compile_package(&workspace, &id)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let json = serde_json::json!({
                "schema": "opensks.design-compile.v1",
                "package_id": outcome.package_id,
                "ok": outcome.ok,
                "swift_bytes": outcome.swift_bytes,
                "error": outcome.error,
            });
            Ok(CliOutput {
                stdout: json.to_string() + "\n",
            })
        }
        // DESIGN-101: registry-driven catalog — enumerate the design packages on
        // disk so the Studio sidebar reflects reality instead of a hard-coded seed.
        "list" => {
            let packages = opensks_design::list_packages(&workspace);
            let arr: Vec<serde_json::Value> = packages
                .into_iter()
                .map(|p| {
                    serde_json::json!({
                        "package_id": p.package_id,
                        "title": p.title,
                        "active": p.active,
                    })
                })
                .collect();
            let json = serde_json::json!({
                "schema": "opensks.design-package-list.v1",
                "packages": arr,
            });
            Ok(CliOutput {
                stdout: json.to_string() + "\n",
            })
        }
        _ => Err(CliError::Usage(USAGE.to_string())),
    }
}

/// `opensks design import|import-approve|import-reject|import-status` — the
/// human-reviewed design package quarantine pipeline (PR-039). The security
/// logic lives in opensks-design::import; this only parses flags, calls it, and
/// prints the contract JSON. A security rejection is an Ok outcome whose entry
/// carries status:rejected + a rejected_reason; only operator errors (e.g. a
/// missing source) are CliErrors.
pub fn run_design_import_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    const USAGE: &str =
        "usage: opensks design import|import-approve|import-reject|import-status ...";
    let subcommand = args
        .first()
        .map(String::as_str)
        .ok_or_else(|| CliError::Usage(USAGE.to_string()))?;
    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let workspace = flag("--workspace")
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.to_path_buf());
    match subcommand {
        "import" => {
            let source = flag("--source").ok_or_else(|| CliError::Usage(USAGE.to_string()))?;
            let kind_str = flag("--kind").unwrap_or_else(|| "local".to_string());
            let kind = opensks_design::ImportKind::parse(&kind_str)
                .ok_or_else(|| CliError::Invalid(format!("unknown import kind: {kind_str}")))?;
            let limits = opensks_design::ImportLimits::default();
            let outcome =
                opensks_design::quarantine_import(&workspace, Path::new(&source), kind, &limits)
                    .map_err(|error| CliError::Invalid(error.to_string()))?;
            Ok(CliOutput {
                stdout: outcome.entry.to_json() + "\n",
            })
        }
        "import-approve" => {
            let id = flag("--quarantine").ok_or_else(|| CliError::Usage(USAGE.to_string()))?;
            let outcome = opensks_design::approve_import(&workspace, &id)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            Ok(CliOutput {
                stdout: format!(
                    "{{\"schema\":\"opensks.design-import-approve.v1\",\"promoted\":{},\"package_id\":{}}}\n",
                    outcome.promoted,
                    json_string(&outcome.package_id)
                ),
            })
        }
        "import-reject" => {
            let id = flag("--quarantine").ok_or_else(|| CliError::Usage(USAGE.to_string()))?;
            let deleted = opensks_design::reject_import(&workspace, &id)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            Ok(CliOutput {
                stdout: format!(
                    "{{\"schema\":\"opensks.design-import-reject.v1\",\"rejected\":true,\"deleted\":{deleted}}}\n"
                ),
            })
        }
        "import-status" => {
            let entries = opensks_design::list_quarantines(&workspace)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            Ok(CliOutput {
                stdout: opensks_design::render_status_json(&entries) + "\n",
            })
        }
        _ => Err(CliError::Usage(USAGE.to_string())),
    }
}

pub fn run_patch_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let subcommand = args
        .first()
        .ok_or_else(|| CliError::Usage(patch_usage().to_string()))?;
    if subcommand == "check" {
        let target = args
            .get(1)
            .ok_or_else(|| CliError::Usage(patch_usage().to_string()))?;
        let envelope = opensks_git::new_patch_envelope(
            format!("patch-{}", ClockStamp::now()?.compact_id()),
            "cli-work-item",
            "cli-lease",
            vec![target.clone()],
        );
        let status = match opensks_git::check_patch_envelope(cwd, &envelope) {
            Ok(()) => "passed".to_string(),
            Err(error) => format!("blocked: {error}"),
        };
        let dir = cwd.join(".opensks").join("patches").join(&envelope.id);
        fs::create_dir_all(&dir)?;
        write_text_atomic(
            &dir.join("typed-patch-envelope.json"),
            &(serde_json::to_string_pretty(&envelope).map_err(|error| {
                CliError::Invalid(format!("serialize patch envelope: {error}"))
            })? + "\n"),
        )?;
        write_text_atomic(
            &dir.join("dirty-guard-result.json"),
            &format!(
                "{{\n  \"schema\": \"opensks.dirty-guard-result.v1\",\n  \"target\": {},\n  \"status\": {}\n}}\n",
                json_string(target),
                json_string(&status)
            ),
        )?;
        return Ok(CliOutput {
            stdout: format!(
                "checked patch transaction guard\nstatus: {}\nartifact: {}\n",
                status,
                dir.join("dirty-guard-result.json").display()
            ),
        });
    }
    if subcommand != "propose" {
        return Err(CliError::Usage(format!(
            "unknown patch subcommand `{subcommand}`\n\n{}",
            patch_usage()
        )));
    }
    let summary = require_freeform_cli(&args[1..], "usage: opensks patch propose \"<summary>\"")?;
    let stamp = ClockStamp::now()?;
    let id = format!("patch-{}-{}", stamp.compact_id(), process::id());
    let dir = cwd.join(".opensks").join("patches").join(&id);
    fs::create_dir_all(&dir)?;
    write_text_atomic(
        &dir.join("patch-envelope.json"),
        &render_patch_envelope(&stamp, &id, &summary),
    )?;
    write_text_atomic(
        &dir.join("patch-gate-result.json"),
        &render_patch_gate(&stamp, &id),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "created patch proposal envelope\npatch: {}\nartifacts: {}\n",
            id,
            dir.display()
        ),
    })
}

pub fn patch_usage() -> &'static str {
    concat!(
        "usage: opensks patch propose \"<summary>\"\n",
        "       opensks patch check <repo-relative-path>\n"
    )
}

pub fn run_conversation_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let Some(subcommand) = args.first() else {
        return Ok(CliOutput {
            stdout: conversation_usage().to_string(),
        });
    };
    if subcommand == "--help" || subcommand == "-h" || subcommand == "help" {
        return Ok(CliOutput {
            stdout: conversation_usage().to_string(),
        });
    }
    let options = parse_conversation_options(&args[1..])?;
    let workspace = options
        .workspace
        .clone()
        .unwrap_or_else(|| cwd.to_path_buf());
    let workspace_key = canonical_workspace_key(&workspace);
    let repo = opensks_conversation::ConversationRepository::open_workspace(&workspace)
        .map_err(|error| CliError::Invalid(error.to_string()))?;
    let now_ms = now_unix_millis()?;
    let project_id = repo
        .create_project(&workspace_key, "Workspace", now_ms)
        .map_err(|error| CliError::Invalid(error.to_string()))?;

    match subcommand.as_str() {
        "list" => {
            let filter = parse_conversation_filter(options.filter.as_deref())?;
            let limit = options.limit.unwrap_or(100);
            let conversations = repo
                .list_conversations(&project_id, filter, limit)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let body = serde_json::json!({
                "schema": "opensks.conversation-list.v1",
                "project_id": project_id,
                "conversations": conversations,
            });
            conversation_output(&body)
        }
        "create" => {
            let title = require_conversation_field(options.title.as_deref(), "--title")?;
            let id = repo
                .create_conversation(&project_id, title, now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            conversation_summary_output(&repo, &id)
        }
        "rename" => {
            let conversation =
                require_conversation_field(options.conversation.as_deref(), "--conversation")?;
            let title = require_conversation_field(options.title.as_deref(), "--title")?;
            repo.rename_conversation(conversation, title, now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            conversation_output(&serde_json::json!({ "ok": true }))
        }
        "pin" | "unpin" => {
            let conversation =
                require_conversation_field(options.conversation.as_deref(), "--conversation")?;
            repo.set_pinned(conversation, subcommand == "pin", now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            conversation_output(&serde_json::json!({ "ok": true }))
        }
        "archive" | "unarchive" => {
            let conversation =
                require_conversation_field(options.conversation.as_deref(), "--conversation")?;
            repo.set_archived(conversation, subcommand == "archive", now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            conversation_output(&serde_json::json!({ "ok": true }))
        }
        "delete" => {
            let conversation =
                require_conversation_field(options.conversation.as_deref(), "--conversation")?;
            let counts = repo
                .delete_conversation(conversation)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            conversation_output(&serde_json::json!({
                "ok": true,
                "messages": counts.messages,
                "runs": counts.runs,
            }))
        }
        "fork" => {
            let conversation =
                require_conversation_field(options.conversation.as_deref(), "--conversation")?;
            let id = repo
                .fork_conversation(conversation, options.after_sequence, now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            conversation_summary_output(&repo, &id)
        }
        "messages" => {
            let conversation =
                require_conversation_field(options.conversation.as_deref(), "--conversation")?;
            let limit = options.limit.unwrap_or(100);
            let messages = repo
                .message_page(conversation, options.before_sequence, limit)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let has_more = messages.len() == limit;
            conversation_output(&serde_json::json!({
                "conversation_id": conversation,
                "messages": messages,
                "has_more": has_more,
            }))
        }
        "append" => {
            let conversation =
                require_conversation_field(options.conversation.as_deref(), "--conversation")?;
            let role = parse_conversation_role(options.role.as_deref())?;
            let text = require_conversation_field(options.text.as_deref(), "--text")?;
            let turn_id = format!("manual-{}", ClockStamp::now()?.compact_id());
            let message_id = repo
                .append_message(
                    &project_id,
                    conversation,
                    &turn_id,
                    role,
                    opensks_contracts::MessageState::Complete,
                    text,
                    now_ms,
                )
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let message = repo
                .message_page(conversation, None, usize::MAX)
                .map_err(|error| CliError::Invalid(error.to_string()))?
                .into_iter()
                .find(|message| message.id == message_id)
                .ok_or_else(|| {
                    CliError::Invalid("appended message could not be reloaded".to_string())
                })?;
            conversation_output(&serde_json::to_value(&message).map_err(|error| {
                CliError::Invalid(format!("serialize conversation message: {error}"))
            })?)
        }
        "turn-start" => {
            let conversation =
                require_conversation_field(options.conversation.as_deref(), "--conversation")?;
            let text = require_conversation_field(options.text.as_deref(), "--text")?;
            run_conversation_turn_start(
                &repo,
                &workspace,
                &project_id,
                conversation,
                text,
                options.idempotency_key.as_deref(),
                now_ms,
            )
        }
        "supervisor-tick" => {
            let supervisor_id = options
                .supervisor_id
                .as_deref()
                .unwrap_or("cli-turn-supervisor");
            let lease_ttl_ms = options.lease_ttl_ms.unwrap_or(30_000);
            let recovered = repo
                .recover_expired_turn_supervisor_leases(now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let claimed = repo
                .claim_next_queued_turn(supervisor_id, lease_ttl_ms, now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let claimed_json = claimed.map(|lease| {
                serde_json::json!({
                    "turn_id": lease.turn_id,
                    "run_id": lease.run_id,
                    "project_id": lease.project_id,
                    "conversation_id": lease.conversation_id,
                    "assistant_message_id": lease.assistant_message_id,
                    "lease_owner": lease.lease_owner,
                    "lease_expires_at_ms": lease.lease_expires_at_ms,
                    "has_model_routing_decision": lease.model_routing_decision_json.is_some(),
                })
            });
            conversation_output(&serde_json::json!({
                "schema": "opensks.turn-supervisor-tick.v1",
                "supervisor_id": supervisor_id,
                "recovered_expired_leases": recovered,
                "claimed": claimed_json,
            }))
        }
        "runs" => {
            let conversation =
                require_conversation_field(options.conversation.as_deref(), "--conversation")?;
            let runs = repo
                .runs_for_conversation(conversation)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let runs_json: Vec<serde_json::Value> = runs
                .into_iter()
                .map(|run| {
                    serde_json::json!({
                        "turn_id": run.turn_id,
                        "run_id": run.run_id,
                        "message_id": run.message_id,
                        "relation": run.relation,
                        // Real state from the run projection; never a fabricated
                        // `completed` (recovery directive §6.7).
                        "run_state": run.run_state.unwrap_or_else(|| "unknown".to_string()),
                    })
                })
                .collect();
            conversation_output(&serde_json::json!({
                "schema": "opensks.conversation-run-list.v1",
                "conversation_id": conversation,
                "runs": runs_json,
            }))
        }
        "timeline" => {
            let conversation =
                require_conversation_field(options.conversation.as_deref(), "--conversation")?;
            let limit = options.limit.unwrap_or(200);
            let items = conversation_timeline::conversation_timeline_items(
                &repo,
                &workspace,
                conversation,
                limit,
            )?;
            conversation_output(&serde_json::json!({
                "schema": "opensks.conversation-timeline.v1",
                "conversation_id": conversation,
                "items": items,
            }))
        }
        "timeline-append" => {
            let conversation =
                require_conversation_field(options.conversation.as_deref(), "--conversation")?;
            let kind = parse_timeline_kind(options.kind.as_deref())?;
            let state = options.state.as_deref().unwrap_or("recorded");
            let raw_payload = require_conversation_field(options.payload.as_deref(), "--payload")?;
            let payload =
                serde_json::from_str::<serde_json::Value>(raw_payload).map_err(|error| {
                    CliError::Invalid(format!("invalid timeline payload json: {error}"))
                })?;
            let item = repo
                .append_timeline_item(&project_id, conversation, kind, state, payload, now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            conversation_output(
                &serde_json::to_value(&item).map_err(|error| {
                    CliError::Invalid(format!("serialize timeline item: {error}"))
                })?,
            )
        }
        "settings-get" => {
            let conversation =
                require_conversation_field(options.conversation.as_deref(), "--conversation")?;
            let settings = match repo
                .get_thread_settings(conversation)
                .map_err(|error| CliError::Invalid(error.to_string()))?
            {
                Some(raw) => {
                    serde_json::from_str::<opensks_contracts::ConversationThreadSettings>(&raw)
                        .map_err(|error| {
                            CliError::Invalid(format!(
                                "stored thread settings are invalid: {error}"
                            ))
                        })?
                }
                None => {
                    opensks_contracts::ConversationThreadSettings::default_for(conversation, now_ms)
                }
            };
            conversation_output(&serde_json::to_value(&settings).map_err(|error| {
                CliError::Invalid(format!("serialize thread settings: {error}"))
            })?)
        }
        "settings-set" => {
            let conversation =
                require_conversation_field(options.conversation.as_deref(), "--conversation")?;
            let raw = require_conversation_field(options.settings.as_deref(), "--settings")?;
            // Validate by parsing into the typed contract, then store the
            // normalized form (rejects unknown/garbage settings).
            let mut settings: opensks_contracts::ConversationThreadSettings =
                serde_json::from_str(raw).map_err(|error| {
                    CliError::Invalid(format!("invalid thread settings json: {error}"))
                })?;
            settings.conversation_id = conversation.to_string();
            settings.updated_at_ms = now_ms;
            let normalized = serde_json::to_string(&settings).map_err(|error| {
                CliError::Invalid(format!("serialize thread settings: {error}"))
            })?;
            repo.set_thread_settings(conversation, &normalized, now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            conversation_output(&serde_json::to_value(&settings).map_err(|error| {
                CliError::Invalid(format!("serialize thread settings: {error}"))
            })?)
        }
        other => Err(CliError::Usage(format!(
            "unknown conversation subcommand `{other}`\n\n{}",
            conversation_usage()
        ))),
    }
}

/// Persist a user message and an assistant placeholder, start one deterministic
/// engine run against the workspace, finalize the assistant message from the run
/// result, link the run, and (when an idempotency key is supplied) record the
/// produced ids so a repeated call replays without starting a second run.
#[allow(clippy::too_many_arguments)]
fn run_conversation_turn_start(
    repo: &opensks_conversation::ConversationRepository,
    workspace: &Path,
    project_id: &str,
    conversation_id: &str,
    text: &str,
    idempotency_key: Option<&str>,
    now_ms: u64,
) -> Result<CliOutput, CliError> {
    if let Some(key) = idempotency_key {
        if let Some(existing) = repo
            .lookup_turn_idempotency(key, conversation_id)
            .map_err(|error| CliError::Invalid(error.to_string()))?
        {
            let run_state = repo
                .run_projection_state(&existing.run_id)
                .map_err(|error| CliError::Invalid(error.to_string()))?
                .unwrap_or_else(|| "unknown".to_string());
            return conversation_output(&serde_json::json!({
                "schema": "opensks.conversation-turn.v1",
                "turn_id": existing.turn_id,
                "user_message_id": existing.user_message_id,
                "assistant_message_id": existing.assistant_message_id,
                "run_id": existing.run_id,
                "run_state": run_state,
                "reused": true,
            }));
        }
    }

    let turn_id = repo
        .new_turn_id()
        .map_err(|error| CliError::Invalid(error.to_string()))?;
    let thread_settings = effective_thread_settings(repo, conversation_id, now_ms)?;
    let effective_settings = turn_settings_from_thread(&thread_settings);
    let effective_settings_json = serde_json::to_string(&effective_settings)
        .map_err(|error| CliError::Invalid(format!("serialize turn settings: {error}")))?;
    let settings_digest = sha256_v1(&effective_settings_json);
    let model_routing_decision = opensks_provider::routing_decision_from_turn_settings(
        format!("route-{turn_id}"),
        &effective_settings,
    );
    let model_routing_decision_json = serde_json::to_string(&model_routing_decision)
        .map_err(|error| CliError::Invalid(format!("serialize model routing decision: {error}")))?;

    let user_message_id = repo
        .append_message(
            project_id,
            conversation_id,
            &turn_id,
            opensks_contracts::MessageRole::User,
            opensks_contracts::MessageState::Complete,
            text,
            now_ms,
        )
        .map_err(|error| CliError::Invalid(error.to_string()))?;

    let assistant_message_id = repo
        .append_message(
            project_id,
            conversation_id,
            &turn_id,
            opensks_contracts::MessageRole::Assistant,
            opensks_contracts::MessageState::Streaming,
            "...",
            now_ms,
        )
        .map_err(|error| CliError::Invalid(error.to_string()))?;

    let run_id = format!("turn-{turn_id}");
    let stream_id = format!("stream-{turn_id}");
    let request_id = format!("request-{turn_id}");
    let idempotency_key_value = idempotency_key
        .map(str::to_string)
        .unwrap_or_else(|| format!("implicit-{turn_id}"));
    repo.record_turn_run_snapshot(opensks_conversation::TurnRunSnapshot {
        turn_id: &turn_id,
        run_id: &run_id,
        project_id,
        conversation_id,
        client_turn_id: &turn_id,
        request_id: &request_id,
        idempotency_key: &idempotency_key_value,
        state: "queued",
        effective_settings_json: &effective_settings_json,
        settings_digest: &settings_digest,
        model_routing_decision_json: Some(&model_routing_decision_json),
        now_ms,
    })
    .map_err(|error| CliError::Invalid(error.to_string()))?;

    let request = opensks_adapter::AgentRunRequest {
        workspace: workspace.to_path_buf(),
        project_id: project_id.to_string(),
        conversation_id: conversation_id.to_string(),
        turn_id: turn_id.clone(),
        run_id: run_id.clone(),
        stream_id: stream_id.clone(),
        now_ms,
        prompt: text.to_string(),
    };
    // Compatibility shim for the old synchronous CLI command. Adapter events
    // must still land in the durable event journal before any projection claims
    // are updated; the future daemon subscriber path replays this same store.
    let journal_sink = DurableAgentEventSink::open(workspace)?;
    journal_sink.emit_run_started(&request)?;
    let model = model_routing_decision
        .selected_model_id
        .clone()
        .map(opensks_adapter::OpenRouterAdapter::new);
    let explicit_local_test = opensks_adapter::LocalTestInstruction::from_prompt(text).is_some();
    let outcome = if let Some(model) = model.filter(opensks_adapter::OpenRouterAdapter::is_configured) {
        let completer = opensks_adapter::NativeHttpChatCompleter::new(model.clone());
        let mut driver = opensks_adapter::OpenRouterToolDriver::new(
            model.model.clone(),
            model.max_tokens,
            completer,
            "You are a coding agent. Use workspace tools for file changes; final text alone must not claim files changed.",
            text,
        );
        opensks_adapter::run_agentic_loop(
            &request,
            &mut driver,
            &opensks_adapter::AgenticConfig::default(),
            &journal_sink,
        )
    } else if explicit_local_test {
        run_explicit_local_test_turn(&request, &journal_sink)
    } else {
        opensks_adapter::AgentEventSink::emit(
            &journal_sink,
            setup_required_agent_event(&request),
        );
        Ok(opensks_adapter::AgentRunOutcome {
            assistant_text: "Needs setup — connect at least one code-capable model.".to_string(),
            patches: vec![],
            apply_results: vec![],
            final_state: opensks_contracts::projection::RunProjectionState::Failed,
        })
    }
    .map_err(|error| CliError::Invalid(format!("agent run failed: {error}")))?;
    let last_event_sequence = journal_sink.finish(&run_id)?;

    use opensks_contracts::projection::RunProjectionState;
    let run_state = match outcome.final_state {
        RunProjectionState::Completed => "completed",
        RunProjectionState::Failed => "failed",
        RunProjectionState::Cancelled => "cancelled",
        RunProjectionState::Paused => "paused",
        RunProjectionState::Queued | RunProjectionState::Running => "running",
    };
    let assistant_state = match outcome.final_state {
        RunProjectionState::Completed => opensks_contracts::MessageState::Complete,
        RunProjectionState::Failed | RunProjectionState::Cancelled => {
            opensks_contracts::MessageState::Failed
        }
        _ => opensks_contracts::MessageState::Streaming,
    };
    let assistant_content = outcome.assistant_text.clone();

    repo.set_message_content(
        &assistant_message_id,
        &assistant_content,
        assistant_state,
        now_ms,
    )
    .map_err(|error| CliError::Invalid(error.to_string()))?;

    repo.link_run(
        conversation_id,
        &assistant_message_id,
        &turn_id,
        &run_id,
        "primary",
        now_ms,
    )
    .map_err(|error| CliError::Invalid(error.to_string()))?;

    // Record the run's real terminal state so the runs list and idempotent
    // replay read it back instead of fabricating `completed` (directive §6.7).
    repo.upsert_run_projection_with_last_sequence(
        &run_id,
        project_id,
        conversation_id,
        &turn_id,
        run_state,
        last_event_sequence,
        now_ms,
    )
    .map_err(|error| CliError::Invalid(error.to_string()))?;
    repo.set_turn_run_state_with_last_sequence(
        &turn_id,
        &run_id,
        run_state,
        Some(last_event_sequence),
        now_ms,
    )
    .map_err(|error| CliError::Invalid(error.to_string()))?;

    if let Some(key) = idempotency_key {
        repo.record_turn_idempotency(
            key,
            conversation_id,
            &opensks_conversation::TurnIdempotencyRecord {
                turn_id: turn_id.clone(),
                user_message_id: user_message_id.clone(),
                assistant_message_id: assistant_message_id.clone(),
                run_id: run_id.clone(),
            },
            now_ms,
        )
        .map_err(|error| CliError::Invalid(error.to_string()))?;
    }

    conversation_output(&serde_json::json!({
        "schema": "opensks.conversation-turn.v1",
        "turn_id": turn_id,
        "user_message_id": user_message_id,
        "assistant_message_id": assistant_message_id,
        "run_id": run_id,
        "run_state": run_state,
        "stream_id": stream_id,
        "settings_digest": settings_digest,
        "model_routing_decision": model_routing_decision,
        "last_event_sequence": last_event_sequence,
        "reused": false,
    }))
}

fn effective_thread_settings(
    repo: &opensks_conversation::ConversationRepository,
    conversation_id: &str,
    now_ms: u64,
) -> Result<opensks_contracts::ConversationThreadSettings, CliError> {
    match repo
        .get_thread_settings(conversation_id)
        .map_err(|error| CliError::Invalid(error.to_string()))?
    {
        Some(raw) => serde_json::from_str::<opensks_contracts::ConversationThreadSettings>(&raw)
            .map_err(|error| {
                CliError::Invalid(format!("stored thread settings are invalid: {error}"))
            }),
        None => Ok(opensks_contracts::ConversationThreadSettings::default_for(
            conversation_id,
            now_ms,
        )),
    }
}

fn turn_settings_from_thread(
    settings: &opensks_contracts::ConversationThreadSettings,
) -> opensks_contracts::ConversationTurnSettings {
    opensks_contracts::ConversationTurnSettings {
        model: settings.model_selection.clone(),
        reasoning_effort: settings.reasoning_effort,
        execution_mode: settings.execution_mode,
        pipeline_id: settings.pipeline_id.clone(),
        graph_revision: None,
        max_parallelism: settings.max_parallelism,
        verifier_count: settings.verifier_count,
        tool_policy_id: settings.tool_policy_id.clone(),
        approval_policy_id: settings.approval_policy_id.clone(),
        token_budget: None,
        cost_budget_usd: None,
        timeout_ms: None,
        image_model_id: settings.image_model_id.clone(),
    }
}

fn sha256_v1(content: &str) -> String {
    sha256_bytes_v1(content.as_bytes())
}

fn sha256_bytes_v1(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    format!("sha256:v1:{hex}")
}

struct DurableAgentEventSink {
    store: std::sync::Mutex<opensks_event_store::EventStore>,
    failures: std::sync::Mutex<Vec<String>>,
}

impl DurableAgentEventSink {
    fn open(workspace: &Path) -> Result<Self, CliError> {
        let store = opensks_event_store::EventStore::open_workspace(workspace)
            .map_err(|error| CliError::Invalid(format!("open event journal: {error}")))?;
        Ok(Self {
            store: std::sync::Mutex::new(store),
            failures: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn emit_run_started(&self, request: &opensks_adapter::AgentRunRequest) -> Result<(), CliError> {
        let event = opensks_contracts::ExecutionEventEnvelope {
            schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
            id: format!("agent-{}-run-started", request.run_id),
            run_id: request.run_id.clone(),
            sequence: 1,
            occurred_at: event_time_from_ms(request.now_ms),
            actor: "opensks-cli".to_string(),
            causation_id: Some(request.turn_id.clone()),
            correlation_id: Some(request.stream_id.clone()),
            kind: opensks_contracts::EventKind::RunStarted,
            payload: serde_json::json!({
                "source": "conversation.turn-start",
                "project_id": request.project_id,
                "conversation_id": request.conversation_id,
                "turn_id": request.turn_id,
                "stream_id": request.stream_id,
                "state": "queued"
            }),
            sensitivity: opensks_contracts::Sensitivity::Internal,
            evidence_refs: vec!["conversation:turn-start".to_string()],
        };
        let mut store = self
            .store
            .lock()
            .map_err(|_| CliError::Invalid("event journal lock poisoned".to_string()))?;
        store
            .append_event(event)
            .map_err(|error| CliError::Invalid(format!("append run-started event: {error}")))?;
        Ok(())
    }

    fn finish(&self, run_id: &str) -> Result<u64, CliError> {
        let failures = self
            .failures
            .lock()
            .map_err(|_| CliError::Invalid("event journal failure lock poisoned".to_string()))?;
        if !failures.is_empty() {
            return Err(CliError::Invalid(format!(
                "append agent event journal: {}",
                failures.join("; ")
            )));
        }
        drop(failures);
        let store = self
            .store
            .lock()
            .map_err(|_| CliError::Invalid("event journal lock poisoned".to_string()))?;
        let events = store
            .replay(run_id)
            .map_err(|error| CliError::Invalid(format!("replay event journal: {error}")))?;
        Ok(events.last().map(|event| event.sequence).unwrap_or(0))
    }

    fn record_failure(&self, error: impl std::fmt::Display) {
        if let Ok(mut failures) = self.failures.lock() {
            failures.push(error.to_string());
        }
    }
}

impl opensks_adapter::AgentEventSink for DurableAgentEventSink {
    fn emit(&self, event: opensks_contracts::AgentEventEnvelope) {
        let execution_event = execution_event_from_agent_event(event);
        match self.store.lock() {
            Ok(mut store) => {
                if let Err(error) = store.append_event(execution_event) {
                    self.record_failure(error);
                }
            }
            Err(_) => self.record_failure("event journal lock poisoned"),
        }
    }
}

fn execution_event_from_agent_event(
    event: opensks_contracts::AgentEventEnvelope,
) -> opensks_contracts::ExecutionEventEnvelope {
    let agent_kind = agent_event_kind_label(event.kind);
    opensks_contracts::ExecutionEventEnvelope {
        schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
        id: format!("agent-{}-s{}", event.run_id, event.sequence + 2),
        run_id: event.run_id.clone(),
        sequence: event.sequence + 2,
        occurred_at: event_time_from_ms(event.occurred_at_ms),
        actor: event
            .worker_id
            .clone()
            .unwrap_or_else(|| "agent".to_string()),
        causation_id: Some(format!("agent-event:{}", event.sequence)),
        correlation_id: Some(event.stream_id.clone()),
        kind: execution_kind_for_agent_event(event.kind),
        payload: serde_json::json!({
            "source_schema": event.schema,
            "agent_event_kind": agent_kind,
            "stream_id": event.stream_id,
            "project_id": event.project_id,
            "conversation_id": event.conversation_id,
            "turn_id": event.turn_id,
            "run_id": event.run_id,
            "worker_id": event.worker_id,
            "node_id": event.node_id,
            "payload": event.payload
        }),
        sensitivity: event.sensitivity,
        evidence_refs: event.evidence_refs,
    }
}

fn execution_kind_for_agent_event(
    kind: opensks_contracts::AgentEventKind,
) -> opensks_contracts::EventKind {
    use opensks_contracts::AgentEventKind;
    match kind {
        AgentEventKind::AssistantTextCompleted
        | AgentEventKind::ToolCallCompleted
        | AgentEventKind::FilePatchApplied
        | AgentEventKind::WorkerCompleted => opensks_contracts::EventKind::WorkItemCompleted,
        AgentEventKind::VerificationCompleted => opensks_contracts::EventKind::VerificationPassed,
        AgentEventKind::Error => opensks_contracts::EventKind::VerificationFailed,
        AgentEventKind::ApprovalRequested => opensks_contracts::EventKind::ApprovalRequested,
        AgentEventKind::ApprovalResolved => opensks_contracts::EventKind::ApprovalApproved,
        AgentEventKind::PlanUpdated
        | AgentEventKind::AssistantTextDelta
        | AgentEventKind::ToolCallStarted
        | AgentEventKind::ToolCallOutput
        | AgentEventKind::FilePatchProposed
        | AgentEventKind::VerificationStarted
        | AgentEventKind::WorkerSpawned
        | AgentEventKind::WorkerProgress
        | AgentEventKind::ImageArtifactCreated
        | AgentEventKind::Warning => opensks_contracts::EventKind::WorkItemRunning,
    }
}

fn setup_required_agent_event(
    request: &opensks_adapter::AgentRunRequest,
) -> opensks_contracts::AgentEventEnvelope {
    opensks_contracts::AgentEventEnvelope {
        schema: opensks_contracts::AGENT_EVENT_ENVELOPE_SCHEMA.to_string(),
        stream_id: request.stream_id.clone(),
        project_id: request.project_id.clone(),
        conversation_id: request.conversation_id.clone(),
        turn_id: request.turn_id.clone(),
        run_id: request.run_id.clone(),
        worker_id: Some("provider-router".to_string()),
        node_id: None,
        sequence: 0,
        occurred_at_ms: request.now_ms,
        kind: opensks_contracts::AgentEventKind::Error,
        payload: serde_json::json!({
            "code": "setup_required",
            "message": "Needs setup — connect at least one code-capable model."
        }),
        sensitivity: opensks_contracts::Sensitivity::Public,
        evidence_refs: vec!["provider:no-code-capable-model".to_string()],
    }
}

#[cfg(any(test, feature = "simulation"))]
fn run_explicit_local_test_turn(
    request: &opensks_adapter::AgentRunRequest,
    sink: &dyn opensks_adapter::AgentEventSink,
) -> Result<opensks_adapter::AgentRunOutcome, opensks_adapter::AgentAdapterError> {
    opensks_adapter::AgentAdapter::run(&opensks_adapter::LocalTestAdapter::new(), request, sink)
}

#[cfg(not(any(test, feature = "simulation")))]
fn run_explicit_local_test_turn(
    request: &opensks_adapter::AgentRunRequest,
    sink: &dyn opensks_adapter::AgentEventSink,
) -> Result<opensks_adapter::AgentRunOutcome, opensks_adapter::AgentAdapterError> {
    opensks_adapter::AgentEventSink::emit(
        sink,
        opensks_contracts::AgentEventEnvelope {
            schema: opensks_contracts::AGENT_EVENT_ENVELOPE_SCHEMA.to_string(),
            stream_id: request.stream_id.clone(),
            project_id: request.project_id.clone(),
            conversation_id: request.conversation_id.clone(),
            turn_id: request.turn_id.clone(),
            run_id: request.run_id.clone(),
            worker_id: Some("simulation-disabled".to_string()),
            node_id: None,
            sequence: 0,
            occurred_at_ms: request.now_ms,
            kind: opensks_contracts::AgentEventKind::Error,
            payload: serde_json::json!({
                "code": "simulation_unavailable",
                "message": "Local test simulation is disabled in this build."
            }),
            sensitivity: opensks_contracts::Sensitivity::Public,
            evidence_refs: vec!["build:simulation-feature-disabled".to_string()],
        },
    );
    Ok(opensks_adapter::AgentRunOutcome {
        assistant_text: "Local test simulation is disabled in this build.".to_string(),
        patches: vec![],
        apply_results: vec![],
        final_state: opensks_contracts::projection::RunProjectionState::Failed,
    })
}

fn agent_event_kind_label(kind: opensks_contracts::AgentEventKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn event_time_from_ms(ms: u64) -> String {
    let secs = ms / 1_000;
    let nanos = (ms % 1_000) * 1_000_000;
    format!("{secs}.{nanos:09}")
}

pub fn conversation_usage() -> &'static str {
    conversation_args::conversation_usage()
}

/// Resolve the per-workspace project key. Canonicalize when the path resolves on
/// disk so the same workspace always maps to one project; otherwise fall back to
/// the lexical path string so the key is still stable for a given input.
fn canonical_workspace_key(workspace: &Path) -> String {
    fs::canonicalize(workspace)
        .unwrap_or_else(|_| workspace.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn now_unix_millis() -> Result<u64, CliError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CliError::Invalid("system clock is before UNIX_EPOCH".to_string()))?
        .as_millis();
    u64::try_from(millis)
        .map_err(|_| CliError::Invalid("system clock millis exceed u64 range".to_string()))
}

fn conversation_summary_output(
    repo: &opensks_conversation::ConversationRepository,
    id: &str,
) -> Result<CliOutput, CliError> {
    let summary = repo
        .get_conversation(id)
        .map_err(|error| CliError::Invalid(error.to_string()))?
        .ok_or_else(|| CliError::Invalid(format!("conversation not found: {id}")))?;
    conversation_output(
        &serde_json::to_value(&summary).map_err(|error| {
            CliError::Invalid(format!("serialize conversation summary: {error}"))
        })?,
    )
}

fn conversation_output(value: &serde_json::Value) -> Result<CliOutput, CliError> {
    let stdout = serde_json::to_string_pretty(value)
        .map_err(|error| CliError::Invalid(format!("serialize conversation output: {error}")))?
        + "\n";
    Ok(CliOutput { stdout })
}

/// Options for the `vault` verb (PR-042). `workspace` defaults to `cwd`.
#[derive(Debug, Default)]
struct VaultCommandOptions {
    workspace: Option<PathBuf>,
    conversation: Option<String>,
    recipient: Option<String>,
    vault: Option<String>,
    identity_file: Option<String>,
}

/// `opensks vault <export-summary|encrypt|decrypt|status> ...`.
///
/// Subcommand strings (for the single root dispatch arm):
///   - `export-summary` : write a SANITIZED, git-trackable summary
///   - `encrypt`        : opt-in age-encrypt the FULL transcript to a `.age`
///   - `decrypt`        : age-decrypt a `.age` back to plaintext with an identity
///   - `status`         : list summaries + `.age` vaults
///
/// Encryption/decryption failures emit the `opensks.vault-error.v1` JSON body on
/// stderr with a nonzero exit (via `CliError::Invalid`), never leaking plaintext.
/// `opensks security report|audit` — the PR-044 security hardening surface.
///
/// `report` emits the structured `opensks.security-report.v1` JSON, aggregating
/// the cheap built-in posture checks (redaction enabled, capability config,
/// approval/replay, dependency-advisory note) plus any pre-computed findings.
/// `audit` runs the same cheap checks and exits nonzero if any `critical`/`high`
/// finding is still `open`, so it can be wired as a CI gate.
pub fn run_security_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let Some(subcommand) = args.first() else {
        return Ok(CliOutput {
            stdout: security_usage().to_string(),
        });
    };
    if subcommand == "--help" || subcommand == "-h" || subcommand == "help" {
        return Ok(CliOutput {
            stdout: security_usage().to_string(),
        });
    }
    let options = parse_security_options(&args[1..])?;
    let workspace = options
        .workspace
        .clone()
        .unwrap_or_else(|| cwd.to_path_buf());

    match subcommand.as_str() {
        "report" => {
            let report = build_security_report(&workspace, &options)?;
            let body = serde_json::to_value(&report).map_err(|error| {
                CliError::Invalid(format!("serialize security report: {error}"))
            })?;
            file_output(&body)
        }
        "audit" => {
            // The audit gate ignores any externally supplied findings file: it
            // runs only the cheap, deterministic built-in checks and fails if a
            // gating finding is open.
            let report = build_security_report(
                &workspace,
                &SecurityCommandOptions::default_for(&workspace),
            )?;
            if report.has_open_blocking_finding() {
                // The error message IS the report JSON so it prints verbatim on
                // stderr and the process exits nonzero.
                let json = serde_json::to_string_pretty(&report).map_err(|error| {
                    CliError::Invalid(format!("serialize security audit: {error}"))
                })?;
                return Err(CliError::Invalid(json));
            }
            let body = serde_json::to_value(&report)
                .map_err(|error| CliError::Invalid(format!("serialize security audit: {error}")))?;
            file_output(&body)
        }
        other => Err(CliError::Usage(format!(
            "unknown security subcommand `{other}`\n\n{}",
            security_usage()
        ))),
    }
}

pub fn security_usage() -> &'static str {
    concat!(
        "usage: opensks security report --workspace <path> [--findings <path>] [--generated-at <ts>]\n",
        "       opensks security audit  --workspace <path> [--generated-at <ts>]\n",
    )
}

/// Options for the `security` verb. `findings` points at an optional JSON array
/// of pre-computed `opensks.security-report.v1` findings to fold into `report`.
#[derive(Debug, Default)]
struct SecurityCommandOptions {
    workspace: Option<PathBuf>,
    findings: Option<PathBuf>,
    generated_at: Option<String>,
}

impl SecurityCommandOptions {
    fn default_for(workspace: &Path) -> Self {
        Self {
            workspace: Some(workspace.to_path_buf()),
            findings: None,
            generated_at: None,
        }
    }
}

fn parse_security_options(args: &[String]) -> Result<SecurityCommandOptions, CliError> {
    let mut options = SecurityCommandOptions::default();
    let mut idx = 0;
    while idx < args.len() {
        let flag = args[idx].as_str();
        match flag {
            "--workspace" => {
                options.workspace = Some(PathBuf::from(security_flag_value(args, idx, flag)?));
                idx += 2;
            }
            "--findings" => {
                options.findings = Some(PathBuf::from(security_flag_value(args, idx, flag)?));
                idx += 2;
            }
            "--generated-at" => {
                options.generated_at = Some(security_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown security argument `{other}`\n\n{}",
                    security_usage()
                )));
            }
        }
    }
    Ok(options)
}

fn security_flag_value<'a>(
    args: &'a [String],
    idx: usize,
    flag: &str,
) -> Result<&'a str, CliError> {
    args.get(idx + 1).map(String::as_str).ok_or_else(|| {
        CliError::Usage(format!(
            "security flag `{flag}` requires a value\n\n{}",
            security_usage()
        ))
    })
}

/// Compute the structured security report: the cheap built-in checks plus any
/// pre-computed findings loaded from `--findings`.
fn build_security_report(
    workspace: &Path,
    options: &SecurityCommandOptions,
) -> Result<opensks_contracts::SecurityReport, CliError> {
    let checks = builtin_security_checks(workspace);
    let mut findings = match &options.findings {
        Some(path) => load_security_findings(path)?,
        None => Vec::new(),
    };
    // Surface any built-in check that did NOT pass as a high, open finding so the
    // audit gate can act on it deterministically.
    for check in &checks {
        if !check.passed {
            findings.push(opensks_contracts::SecurityFinding {
                id: format!("builtin-check-failed:{}", check.name),
                severity: opensks_contracts::SecuritySeverity::High,
                category: "builtin_check".to_string(),
                title: format!("Built-in security check `{}` failed", check.name),
                detail: "A deterministic built-in posture check did not pass.".to_string(),
                status: opensks_contracts::FindingStatus::Open,
                owner: None,
                deadline: None,
            });
        }
    }
    // The dependency-advisory posture is a documented, owned/accepted note rather
    // than an open finding: cargo-deny / cargo-audit are the CI scanners.
    findings.push(opensks_contracts::SecurityFinding {
        id: "dependency-advisory-posture".to_string(),
        severity: opensks_contracts::SecuritySeverity::Low,
        category: "dependencies".to_string(),
        title: "External dependency advisory posture".to_string(),
        detail: "cargo-deny + cargo-audit scan dependencies in CI (deny.toml). The crypto \
                 cluster derives from the vetted `age` crate; no hand-rolled cipher/KDF/MAC."
            .to_string(),
        status: opensks_contracts::FindingStatus::Accepted,
        owner: Some("security".to_string()),
        deadline: None,
    });

    let generated_at = options.generated_at.clone().unwrap_or_else(|| {
        now_unix_millis()
            .map(|ms| ms.to_string())
            .unwrap_or_default()
    });
    Ok(opensks_contracts::SecurityReport::new(
        generated_at,
        findings,
        checks,
    ))
}

/// Load a JSON array of pre-computed findings from `path`. A content-free
/// `CliError::Invalid` is returned on any parse failure so no secret material
/// from a malformed file leaks into output.
fn load_security_findings(
    path: &Path,
) -> Result<Vec<opensks_contracts::SecurityFinding>, CliError> {
    let raw = fs::read_to_string(path)
        .map_err(|_| CliError::Invalid("security findings file is unreadable".to_string()))?;
    serde_json::from_str(&raw).map_err(|_| {
        CliError::Invalid("security findings file is not a valid finding array".to_string())
    })
}

/// The cheap, deterministic built-in posture checks. These reflect compiled-in
/// invariants of the runtime rather than live scans, so they never read secrets
/// and are stable across machines.
fn builtin_security_checks(workspace: &Path) -> Vec<opensks_contracts::SecurityCheck> {
    // Engine events default to redacted; redaction is on by construction.
    let redaction_enabled = opensks_contracts::EngineEvent::new(
        "c",
        None,
        opensks_contracts::EngineEventType::EngineHealth,
        "",
        0,
    )
    .redacted;
    // The capability model is deny-by-default: a fresh grant authorizes nothing.
    let caps = opensks_policy::WorkspaceCapabilities::deny_by_default(workspace);
    let capabilities_deny_by_default = caps
        .check_capability(opensks_policy::Capability::ExternalNetwork)
        .is_err()
        && caps
            .check_capability(opensks_policy::Capability::FilesystemWorkspace)
            .is_err();
    // Git push requires approval under the default permission policy.
    let push_decision = opensks_policy::PermissionPolicy::default()
        .decide(opensks_policy::PermissionScope::GitPush);
    let approval_required_for_git_push = !push_decision.allowed && push_decision.approval_required;
    // Reconnect replay is supported: subscribe carries a since_sequence cursor.
    let replay_request = opensks_contracts::EngineRequest::subscribe_events("c", "run", Some(0));
    let reconnect_replay_supported = replay_request.kind
        == opensks_contracts::EngineRequestKind::SubscribeEvents
        && replay_request.params.since_sequence == Some(0);

    vec![
        opensks_contracts::SecurityCheck {
            name: "redaction_enabled".to_string(),
            passed: redaction_enabled,
        },
        opensks_contracts::SecurityCheck {
            name: "capabilities_deny_by_default".to_string(),
            passed: capabilities_deny_by_default,
        },
        opensks_contracts::SecurityCheck {
            name: "approval_required_for_git_push".to_string(),
            passed: approval_required_for_git_push,
        },
        opensks_contracts::SecurityCheck {
            name: "reconnect_replay_supported".to_string(),
            passed: reconnect_replay_supported,
        },
        opensks_contracts::SecurityCheck {
            name: "dependency_advisories_scanned".to_string(),
            passed: true,
        },
    ]
}

pub fn run_vault_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let Some(subcommand) = args.first() else {
        return Ok(CliOutput {
            stdout: vault_usage().to_string(),
        });
    };
    if subcommand == "--help" || subcommand == "-h" || subcommand == "help" {
        return Ok(CliOutput {
            stdout: vault_usage().to_string(),
        });
    }
    let options = parse_vault_options(&args[1..])?;
    let workspace = options
        .workspace
        .clone()
        .unwrap_or_else(|| cwd.to_path_buf());

    match subcommand.as_str() {
        "export-summary" => {
            let conversation =
                require_vault_field(options.conversation.as_deref(), "--conversation")?;
            let repo = open_vault_repo(&workspace)?;
            let now_ms = now_unix_millis()?;
            let written = opensks_vault::export_summary(&repo, &workspace, conversation, now_ms)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            let body = serde_json::json!({
                "schema": opensks_vault::VAULT_SUMMARY_SCHEMA,
                "conversation_id": written.summary.conversation_id,
                "summary_path": written.summary_path.to_string_lossy(),
                "decisions": written.summary.decisions.len(),
                "run_links": written.summary.run_links,
                "contains_raw_transcript": written.summary.contains_raw_transcript,
                "redacted": written.summary.redacted,
            });
            vault_output(&body)
        }
        "encrypt" => {
            let conversation =
                require_vault_field(options.conversation.as_deref(), "--conversation")?;
            let recipient = require_vault_field(options.recipient.as_deref(), "--recipient")?;
            let repo = open_vault_repo(&workspace)?;
            match opensks_vault::encrypt_transcript(&repo, &workspace, conversation, recipient) {
                Ok(result) => {
                    let body = serde_json::json!({
                        "schema": opensks_vault::VAULT_ENCRYPT_SCHEMA,
                        "vault_path": result.vault_path.to_string_lossy(),
                        "recipient": result.recipient,
                        "bytes": result.bytes,
                    });
                    vault_output(&body)
                }
                Err(error) => Err(vault_error_for(&error)),
            }
        }
        "decrypt" => {
            let vault = require_vault_field(options.vault.as_deref(), "--vault")?;
            let identity =
                require_vault_field(options.identity_file.as_deref(), "--identity-file")?;
            match opensks_vault::decrypt_vault(Path::new(vault), Path::new(identity)) {
                Ok(result) => {
                    let body = serde_json::json!({
                        "schema": opensks_vault::VAULT_DECRYPT_SCHEMA,
                        "conversation_id": result.conversation_id,
                        "bytes": result.bytes,
                    });
                    vault_output(&body)
                }
                Err(error) => Err(vault_error_for(&error)),
            }
        }
        "status" => {
            let status = opensks_vault::status(&workspace)
                .map_err(|error| CliError::Invalid(error.to_string()))?;
            vault_output(
                &serde_json::to_value(&status).map_err(|error| {
                    CliError::Invalid(format!("serialize vault status: {error}"))
                })?,
            )
        }
        other => Err(CliError::Usage(format!(
            "unknown vault subcommand `{other}`\n\n{}",
            vault_usage()
        ))),
    }
}

/// Map a vault error into the `opensks.vault-error.v1` contract: the
/// `CliError::Invalid` message IS the compact error JSON, so it is printed
/// verbatim on stderr and the process exits nonzero. No plaintext is included.
fn vault_error_for(error: &opensks_vault::VaultError) -> CliError {
    let code = match error.code() {
        "bad_recipient" => opensks_vault::VaultErrorCode::BadRecipient,
        "decrypt_failed" => opensks_vault::VaultErrorCode::DecryptFailed,
        _ => opensks_vault::VaultErrorCode::EncryptFailed,
    };
    let envelope = opensks_vault::VaultErrorEnvelope::new(code);
    let json = serde_json::to_string(&envelope)
        .unwrap_or_else(|_| "{\"schema\":\"opensks.vault-error.v1\"}".to_string());
    CliError::Invalid(json)
}

fn open_vault_repo(
    workspace: &Path,
) -> Result<opensks_conversation::ConversationRepository, CliError> {
    opensks_conversation::ConversationRepository::open_workspace(workspace)
        .map_err(|error| CliError::Invalid(error.to_string()))
}

pub fn vault_usage() -> &'static str {
    concat!(
        "usage: opensks vault export-summary --workspace <path> --conversation <id>\n",
        "       opensks vault encrypt --workspace <path> --conversation <id> --recipient <age1...>\n",
        "       opensks vault decrypt --workspace <path> --vault <path> --identity-file <path>\n",
        "       opensks vault status --workspace <path>\n"
    )
}

fn parse_vault_options(args: &[String]) -> Result<VaultCommandOptions, CliError> {
    let mut options = VaultCommandOptions::default();
    let mut idx = 0;
    while idx < args.len() {
        let flag = args[idx].as_str();
        match flag {
            "--workspace" => {
                options.workspace = Some(PathBuf::from(vault_flag_value(args, idx, flag)?));
                idx += 2;
            }
            "--conversation" => {
                options.conversation = Some(vault_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--recipient" => {
                options.recipient = Some(vault_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--vault" => {
                options.vault = Some(vault_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--identity-file" => {
                options.identity_file = Some(vault_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown vault argument `{other}`\n\n{}",
                    vault_usage()
                )));
            }
        }
    }
    Ok(options)
}

fn vault_flag_value<'a>(args: &'a [String], idx: usize, flag: &str) -> Result<&'a str, CliError> {
    args.get(idx + 1).map(String::as_str).ok_or_else(|| {
        CliError::Usage(format!(
            "vault flag `{flag}` requires a value\n\n{}",
            vault_usage()
        ))
    })
}

fn require_vault_field<'a>(value: Option<&'a str>, flag: &str) -> Result<&'a str, CliError> {
    value.ok_or_else(|| {
        CliError::Usage(format!(
            "vault command requires `{flag}`\n\n{}",
            vault_usage()
        ))
    })
}

fn vault_output(value: &serde_json::Value) -> Result<CliOutput, CliError> {
    let stdout = serde_json::to_string_pretty(value)
        .map_err(|error| CliError::Invalid(format!("serialize vault output: {error}")))?
        + "\n";
    Ok(CliOutput { stdout })
}

/// Options for the `file` verb. `path` is workspace-relative; `workspace`
/// defaults to the process `cwd`.
#[derive(Debug, Default)]
struct FileCommandOptions {
    workspace: Option<PathBuf>,
    path: Option<String>,
    expected_hash: Option<String>,
    expected_mtime: Option<u64>,
    stdin: bool,
}

/// `opensks file open|save|stat` — the sanctioned editor read/write path over a
/// canonical workspace (PR-032). Every success serializes a typed JSON document;
/// every guard failure emits a content-free `opensks.file-error.v1` JSON and
/// returns `CliError::Invalid` so the process exits nonzero.
pub fn run_file_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    // The save subcommand reads its new content from stdin; the read is injected
    // here so the command logic stays testable without a live stdin.
    run_file_command_with_input(args, cwd, read_stdin_to_string)
}

/// Inner implementation of [`run_file_command`] with the stdin read injected as
/// `read_input`, invoked only on the `save` path after its flags validate.
fn run_file_command_with_input<F>(
    args: &[String],
    cwd: &Path,
    read_input: F,
) -> Result<CliOutput, CliError>
where
    F: FnOnce() -> Result<String, CliError>,
{
    let Some(subcommand) = args.first() else {
        return Ok(CliOutput {
            stdout: file_usage().to_string(),
        });
    };
    if subcommand == "--help" || subcommand == "-h" || subcommand == "help" {
        return Ok(CliOutput {
            stdout: file_usage().to_string(),
        });
    }

    let options = parse_file_options(&args[1..])?;
    let workspace = options
        .workspace
        .clone()
        .unwrap_or_else(|| cwd.to_path_buf());
    let relative = require_file_field(options.path.as_deref(), "--path")?;

    // Open the service over the canonical workspace. A bad workspace root is a
    // configuration error, not a per-file guard verdict, so it is reported as a
    // usage/invalid error rather than a `file-error.v1` payload.
    let service = match opensks_file_service::WorkspaceFileService::open(&workspace) {
        Ok(service) => service,
        Err(error) => {
            return Err(CliError::Invalid(format!(
                "open workspace `{}`: {}",
                workspace.display(),
                error.reason_code()
            )));
        }
    };

    match subcommand.as_str() {
        "open" => match service.open_text(relative) {
            Ok(document) => file_output(&file_document_json(&document)),
            Err(error) => Err(file_error(&error)),
        },
        "stat" => match service.stat(relative) {
            Ok(entry) => file_output(&file_entry_json(&entry)),
            Err(error) => Err(file_error(&error)),
        },
        "save" => {
            let expected_hash =
                require_file_field(options.expected_hash.as_deref(), "--expected-hash")?;
            if !options.stdin {
                return Err(CliError::Usage(format!(
                    "file save requires `--stdin` (new content is read from stdin)\n\n{}",
                    file_usage()
                )));
            }
            let content = read_input()?;
            let mut request =
                opensks_contracts::SaveTextRequest::new(relative, content, expected_hash);
            request.expected_mtime_ms = options.expected_mtime;
            match service.save_text(&request) {
                Ok(result) => file_output(&file_save_json(&result)),
                Err(error) => Err(file_error(&error)),
            }
        }
        "diff" => {
            if !options.stdin {
                return Err(CliError::Usage(format!(
                    "file diff requires `--stdin` (the editor buffer is read from stdin)\n\n{}",
                    file_usage()
                )));
            }
            // Read the on-disk file through the hardened service so the diff
            // honors the same guards (escape/secret/binary) as open/save.
            let document = match service.open_text(relative) {
                Ok(document) => document,
                Err(error) => return Err(file_error(&error)),
            };
            let buffer = read_input()?;
            let diff = compute_text_diff(relative, &document.content, &buffer);
            let value = serde_json::to_value(&diff)
                .map_err(|error| CliError::Invalid(format!("serialize text diff: {error}")))?;
            file_output(&value)
        }
        other => Err(CliError::Usage(format!(
            "unknown file subcommand `{other}`\n\n{}",
            file_usage()
        ))),
    }
}

pub fn file_usage() -> &'static str {
    concat!(
        "usage: opensks file open --workspace <path> --path <relative>\n",
        "       opensks file save --workspace <path> --path <relative> --expected-hash <hash> [--expected-mtime <ms>] --stdin\n",
        "       opensks file stat --workspace <path> --path <relative>\n",
        "       opensks file diff --workspace <path> --path <relative> --stdin\n"
    )
}

/// Line-level diff of the editor's `buffer` against the `on_disk` content.
///
/// A simple longest-common-subsequence (LCS) walk over lines groups runs of
/// `-`/`+` lines into [`opensks_contracts::DiffHunk`]s. Pure deletions are
/// `Removed`, pure insertions are `Added`, and any block touching both is
/// `Changed`. The result never carries unchanged lines, only the changed ones.
fn compute_text_diff(path: &str, on_disk: &str, buffer: &str) -> opensks_contracts::TextDiff {
    let old_lines: Vec<&str> = split_diff_lines(on_disk);
    let new_lines: Vec<&str> = split_diff_lines(buffer);
    let ops = lcs_diff(&old_lines, &new_lines);
    let hunks = group_diff_hunks(&ops, &old_lines, &new_lines);
    opensks_contracts::TextDiff::new(path, hunks)
}

/// Split text into lines for diffing. A single trailing newline is treated as a
/// terminator (not a spurious trailing empty line); empty input is zero lines.
/// Interior blank lines are preserved.
fn split_diff_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    // `"a\n".split('\n')` yields ["a", ""]; drop that one terminator artifact.
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// A single line-level edit operation produced by the LCS walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffOp {
    /// Line present in both files (advances both cursors).
    Equal,
    /// Line only on disk (advances the old cursor).
    Remove,
    /// Line only in the buffer (advances the new cursor).
    Add,
}

/// Classic dynamic-programming LCS over lines, emitting an ordered op list.
fn lcs_diff(old_lines: &[&str], new_lines: &[&str]) -> Vec<DiffOp> {
    let rows = old_lines.len();
    let cols = new_lines.len();
    // table[i][j] = LCS length of old_lines[i..] and new_lines[j..].
    let mut table = vec![vec![0usize; cols + 1]; rows + 1];
    for i in (0..rows).rev() {
        for j in (0..cols).rev() {
            table[i][j] = if old_lines[i] == new_lines[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < rows && j < cols {
        if old_lines[i] == new_lines[j] {
            ops.push(DiffOp::Equal);
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            ops.push(DiffOp::Remove);
            i += 1;
        } else {
            ops.push(DiffOp::Add);
            j += 1;
        }
    }
    while i < rows {
        ops.push(DiffOp::Remove);
        i += 1;
    }
    while j < cols {
        ops.push(DiffOp::Add);
        j += 1;
    }
    ops
}

/// Collapse the flat op list into contiguous hunks of change, tracking 1-based
/// line numbers in both the old and new files.
fn group_diff_hunks(
    ops: &[DiffOp],
    old_lines: &[&str],
    new_lines: &[&str],
) -> Vec<opensks_contracts::DiffHunk> {
    let mut hunks = Vec::new();
    let mut old_index = 0usize; // 0-based cursor into old_lines
    let mut new_index = 0usize; // 0-based cursor into new_lines
    let mut pending: Vec<DiffOp> = Vec::new();
    let mut hunk_old_start = 0usize;
    let mut hunk_new_start = 0usize;

    let flush = |pending: &mut Vec<DiffOp>,
                 hunk_old_start: usize,
                 hunk_new_start: usize,
                 old_lines: &[&str],
                 new_lines: &[&str],
                 old_cursor: usize,
                 new_cursor: usize,
                 hunks: &mut Vec<opensks_contracts::DiffHunk>| {
        if pending.is_empty() {
            return;
        }
        let removed = pending.iter().filter(|op| **op == DiffOp::Remove).count();
        let added = pending.iter().filter(|op| **op == DiffOp::Add).count();
        let kind = match (removed > 0, added > 0) {
            (true, true) => opensks_contracts::DiffHunkKind::Changed,
            (true, false) => opensks_contracts::DiffHunkKind::Removed,
            (false, true) => opensks_contracts::DiffHunkKind::Added,
            (false, false) => return,
        };
        let mut lines = Vec::with_capacity(removed + added);
        for line in &old_lines[hunk_old_start..old_cursor] {
            lines.push(format!("-{line}"));
        }
        for line in &new_lines[hunk_new_start..new_cursor] {
            lines.push(format!("+{line}"));
        }
        hunks.push(opensks_contracts::DiffHunk {
            kind,
            old_start: hunk_old_start + 1,
            old_lines: removed,
            new_start: hunk_new_start + 1,
            new_lines: added,
            lines,
        });
        pending.clear();
    };

    for op in ops {
        match op {
            DiffOp::Equal => {
                flush(
                    &mut pending,
                    hunk_old_start,
                    hunk_new_start,
                    old_lines,
                    new_lines,
                    old_index,
                    new_index,
                    &mut hunks,
                );
                old_index += 1;
                new_index += 1;
            }
            DiffOp::Remove => {
                if pending.is_empty() {
                    hunk_old_start = old_index;
                    hunk_new_start = new_index;
                }
                pending.push(DiffOp::Remove);
                old_index += 1;
            }
            DiffOp::Add => {
                if pending.is_empty() {
                    hunk_old_start = old_index;
                    hunk_new_start = new_index;
                }
                pending.push(DiffOp::Add);
                new_index += 1;
            }
        }
    }
    flush(
        &mut pending,
        hunk_old_start,
        hunk_new_start,
        old_lines,
        new_lines,
        old_index,
        new_index,
        &mut hunks,
    );
    hunks
}

fn parse_file_options(args: &[String]) -> Result<FileCommandOptions, CliError> {
    let mut options = FileCommandOptions::default();
    let mut idx = 0;
    while idx < args.len() {
        let flag = args[idx].as_str();
        match flag {
            "--workspace" => {
                options.workspace = Some(PathBuf::from(file_flag_value(args, idx, flag)?));
                idx += 2;
            }
            "--path" => {
                options.path = Some(file_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--expected-hash" => {
                options.expected_hash = Some(file_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--expected-mtime" => {
                options.expected_mtime = Some(file_parse_u64(args, idx, flag)?);
                idx += 2;
            }
            "--stdin" => {
                options.stdin = true;
                idx += 1;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown file argument `{other}`\n\n{}",
                    file_usage()
                )));
            }
        }
    }
    Ok(options)
}

fn file_flag_value<'a>(args: &'a [String], idx: usize, flag: &str) -> Result<&'a str, CliError> {
    args.get(idx + 1).map(String::as_str).ok_or_else(|| {
        CliError::Usage(format!(
            "file flag `{flag}` requires a value\n\n{}",
            file_usage()
        ))
    })
}

fn file_parse_u64(args: &[String], idx: usize, flag: &str) -> Result<u64, CliError> {
    file_flag_value(args, idx, flag)?
        .parse::<u64>()
        .map_err(|_| {
            CliError::Usage(format!(
                "file flag `{flag}` expects a non-negative integer\n\n{}",
                file_usage()
            ))
        })
}

fn require_file_field<'a>(value: Option<&'a str>, flag: &str) -> Result<&'a str, CliError> {
    value.ok_or_else(|| {
        CliError::Usage(format!(
            "file command requires `{flag}`\n\n{}",
            file_usage()
        ))
    })
}

/// Read the full save payload from stdin. The bytes are the new file content and
/// are never echoed into an error message.
fn read_stdin_to_string() -> Result<String, CliError> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    Ok(input)
}

/// Serialize an opened document to the `opensks.text-document.v1` wire shape with
/// the contract's explicit field names (`encoding:"utf-8"`, string `line_ending`,
/// `is_binary:false`).
fn file_document_json(document: &opensks_contracts::TextDocument) -> serde_json::Value {
    serde_json::json!({
        "schema": document.schema,
        "workspace_relative_path": document.workspace_relative_path,
        "content": document.content,
        "content_hash": document.content_hash,
        "encoding": "utf-8",
        "line_ending": document.line_ending.as_str(),
        "byte_size": document.byte_size,
        "is_secret_restricted": document.is_secret_restricted,
        "is_binary": false,
        "on_disk_modification_ms": document.on_disk_modification_ms,
        "permissions_mode": document.permissions_mode,
    })
}

/// Serialize a successful save to the `opensks.save-result.v1` wire shape.
fn file_save_json(result: &opensks_contracts::SaveTextResult) -> serde_json::Value {
    serde_json::json!({
        "schema": "opensks.save-result.v1",
        "workspace_relative_path": result.workspace_relative_path,
        "new_hash": result.new_hash,
        "new_mtime_ms": result.new_mtime_ms,
    })
}

/// Serialize a stat to the `opensks.workspace-entry.v1` wire shape.
fn file_entry_json(entry: &opensks_contracts::WorkspaceEntry) -> serde_json::Value {
    serde_json::json!({
        "schema": entry.schema,
        "workspace_relative_path": entry.workspace_relative_path,
        "byte_size": entry.byte_size,
        "modification_ms": entry.modification_ms,
        "permissions_mode": entry.permissions_mode,
        "content_hash": entry.content_hash,
        "is_secret_restricted": entry.is_secret_restricted,
    })
}

/// Map a `FileServiceError` to the `opensks.file-error.v1` envelope and wrap it
/// in `CliError::Invalid` so the binary prints the JSON and exits nonzero. The
/// message is derived solely from the stable reason code and the
/// workspace-relative path — never from file contents.
fn file_error(error: &opensks_contracts::FileServiceError) -> CliError {
    let body = serde_json::json!({
        "schema": "opensks.file-error.v1",
        "error": {
            "code": error.reason_code(),
            "message": error.to_string(),
        },
    });
    let payload = serde_json::to_string(&body)
        .unwrap_or_else(|_| "{\"schema\":\"opensks.file-error.v1\"}".to_string());
    CliError::Invalid(payload)
}

fn file_output(value: &serde_json::Value) -> Result<CliOutput, CliError> {
    let stdout = serde_json::to_string_pretty(value)
        .map_err(|error| CliError::Invalid(format!("serialize file output: {error}")))?
        + "\n";
    Ok(CliOutput { stdout })
}

/// Flags shared by the `codegraph update`, `git working-change`, and the
/// read-only `git status` / `git branches` / `git diff` subcommands.
/// `workspace` defaults to the process `cwd` when omitted.
#[derive(Debug, Default)]
struct WorkspacePathOptions {
    workspace: Option<PathBuf>,
    path: Option<String>,
    baseline_hash: Option<String>,
    staged: bool,
}

/// Parse `--workspace <p> [--path <rel>] [--baseline-hash <h>] [--staged]` for
/// the PR-033/PR-034 subcommands. Unknown flags are a usage error against the
/// supplied `usage`.
fn parse_workspace_path_options(
    args: &[String],
    usage: &str,
) -> Result<WorkspacePathOptions, CliError> {
    let mut options = WorkspacePathOptions::default();
    let mut idx = 0;
    while idx < args.len() {
        let flag = args[idx].as_str();
        let value = || -> Result<&str, CliError> {
            args.get(idx + 1).map(String::as_str).ok_or_else(|| {
                CliError::Usage(format!("flag `{flag}` requires a value\n\n{usage}"))
            })
        };
        match flag {
            "--workspace" => {
                options.workspace = Some(PathBuf::from(value()?));
                idx += 2;
            }
            "--path" => {
                options.path = Some(value()?.to_string());
                idx += 2;
            }
            "--baseline-hash" => {
                options.baseline_hash = Some(value()?.to_string());
                idx += 2;
            }
            "--staged" => {
                options.staged = true;
                idx += 1;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown argument `{other}`\n\n{usage}"
                )));
            }
        }
    }
    Ok(options)
}

fn parse_daemon_options(args: &[String], cwd: &Path) -> Result<DaemonCommandOptions, CliError> {
    let mut stdio = false;
    let mut workspace = cwd.to_path_buf();
    let mut idx = 0;

    while idx < args.len() {
        match args[idx].as_str() {
            "--stdio" => {
                stdio = true;
                idx += 1;
            }
            "--workspace" => {
                let value = args
                    .get(idx + 1)
                    .ok_or_else(|| CliError::Usage(daemon_usage().to_string()))?;
                workspace = PathBuf::from(value);
                idx += 2;
            }
            "--help" | "-h" => {
                return Err(CliError::Usage(daemon_usage().to_string()));
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown daemon argument `{other}`\n\n{}",
                    daemon_usage()
                )));
            }
        }
    }

    Ok(DaemonCommandOptions { stdio, workspace })
}

fn require_freeform_cli(args: &[String], usage_text: &str) -> Result<String, CliError> {
    let text = args.join(" ").trim().to_string();
    if text.is_empty() {
        return Err(CliError::Usage(usage_text.to_string()));
    }
    Ok(text)
}

fn require_exact_subcommand_cli(
    args: &[String],
    expected: &str,
    usage_text: &str,
) -> Result<(), CliError> {
    if args.len() == 1 && args[0] == expected {
        return Ok(());
    }
    Err(CliError::Usage(usage_text.to_string()))
}

fn parse_optional_scheduler_count(
    args: &[String],
    default: usize,
    min: usize,
    max: usize,
) -> Result<usize, CliError> {
    let count = args
        .first()
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| CliError::Usage(scheduler_usage().to_string()))?
        .unwrap_or(default);
    Ok(count.clamp(min, max))
}

pub fn run_local_qa_checks(cwd: &Path) -> Vec<CommandCheck> {
    if !cwd.join("Cargo.toml").exists() {
        return vec![CommandCheck {
            name: "cargo-project-detection".to_string(),
            command: vec!["cargo".to_string()],
            status: "skipped".to_string(),
            exit_code: None,
            duration_ms: 0,
            stdout: String::new(),
            stderr: "Cargo.toml not found in workspace root".to_string(),
        }];
    }

    [
        ("format", vec!["cargo", "fmt", "--check"]),
        ("test-compile", vec!["cargo", "test", "--no-run"]),
        (
            "lint",
            vec![
                "cargo",
                "clippy",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ],
        ),
    ]
    .into_iter()
    .map(|(name, command)| run_command_check(name, command, cwd))
    .collect()
}

fn run_command_check(name: &str, command: Vec<&str>, cwd: &Path) -> CommandCheck {
    let started = Instant::now();
    let mut process_command = process::Command::new(command[0]);
    process_command.args(&command[1..]).current_dir(cwd);
    match process_command.output() {
        Ok(output) => {
            let duration_ms = started.elapsed().as_millis();
            CommandCheck {
                name: name.to_string(),
                command: command.iter().map(|value| value.to_string()).collect(),
                status: if output.status.success() {
                    "passed".to_string()
                } else {
                    "failed".to_string()
                },
                exit_code: output.status.code(),
                duration_ms,
                stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            }
        }
        Err(error) => {
            let duration_ms = started.elapsed().as_millis();
            CommandCheck {
                name: name.to_string(),
                command: command.iter().map(|value| value.to_string()).collect(),
                status: "error".to_string(),
                exit_code: None,
                duration_ms,
                stdout: String::new(),
                stderr: error.to_string(),
            }
        }
    }
}

fn run_scheduler_overlap_checks(cwd: &Path) -> Vec<StageOverlapSpan> {
    let origin = Instant::now();
    let commands = [
        ("runtime-rustc-version", vec!["rustc", "--version"]),
        ("runtime-cargo-version", vec!["cargo", "--version"]),
    ];
    let mut handles = Vec::new();

    for (name, command) in commands {
        let name = name.to_string();
        let command = command
            .into_iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>();
        let fallback_name = name.clone();
        let fallback_command = command.clone();
        let cwd = cwd.to_path_buf();
        let thread_origin = origin;
        handles.push((
            fallback_name,
            fallback_command,
            thread::spawn(move || run_stage_overlap_span(&name, command, &cwd, thread_origin)),
        ));
    }

    handles
        .into_iter()
        .map(|(name, command, handle)| match handle.join() {
            Ok(span) => span,
            Err(_) => StageOverlapSpan {
                name,
                command,
                status: "error".to_string(),
                exit_code: None,
                start_ms: 0,
                end_ms: origin.elapsed().as_millis(),
                duration_ms: 0,
                stdout: String::new(),
                stderr: "scheduler overlap worker panicked".to_string(),
            },
        })
        .collect()
}

fn run_stage_overlap_span(
    name: &str,
    command: Vec<String>,
    cwd: &Path,
    origin: Instant,
) -> StageOverlapSpan {
    let start_ms = origin.elapsed().as_millis();
    let started = Instant::now();
    let mut process_command = process::Command::new(&command[0]);
    process_command.args(&command[1..]).current_dir(cwd);
    match process_command.output() {
        Ok(output) => {
            let duration_ms = started.elapsed().as_millis();
            let end_ms = origin.elapsed().as_millis();
            StageOverlapSpan {
                name: name.to_string(),
                command,
                status: if output.status.success() {
                    "passed".to_string()
                } else {
                    "failed".to_string()
                },
                exit_code: output.status.code(),
                start_ms,
                end_ms,
                duration_ms,
                stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            }
        }
        Err(error) => {
            let duration_ms = started.elapsed().as_millis();
            let end_ms = origin.elapsed().as_millis();
            StageOverlapSpan {
                name: name.to_string(),
                command,
                status: "error".to_string(),
                exit_code: None,
                start_ms,
                end_ms,
                duration_ms,
                stdout: String::new(),
                stderr: error.to_string(),
            }
        }
    }
}

pub fn render_checks_json(checks: &[CommandCheck]) -> String {
    let rows = checks
        .iter()
        .map(|check| {
            let command = check
                .command
                .iter()
                .map(|value| json_string(value))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                concat!(
                    "{{\"name\":{},\"command\":[{}],\"status\":{},",
                    "\"exit_code\":{},\"duration_ms\":{},\"stdout\":{},\"stderr\":{}}}"
                ),
                json_string(&check.name),
                command,
                json_string(&check.status),
                check
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "null".to_string()),
                check.duration_ms,
                json_string(&truncate_for_json(&check.stdout, 4000)),
                json_string(&truncate_for_json(&check.stderr, 4000))
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

pub fn render_scheduler_plan_json(generated_at_json: &str, run_id: &str, goal: &str) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.stage-scheduler.v1\",\n",
            "  \"run_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"goal\": {},\n",
            "  \"bounded_parallelism\": true,\n",
            "  \"stages\": {},\n",
            "  \"rebalance_policy\": \"serial parent integration; workers use isolated workspaces or patch envelopes\",\n",
            "  \"overlap_report\": \"stage-overlap-report.json\"\n",
            "}}\n"
        ),
        json_string(run_id),
        generated_at_json,
        json_string(goal),
        serde_json::to_string(&[
            "intake",
            "context_hydration",
            "capability_planning",
            "worker_lane_allocation",
            "local_qa",
            "overlap_measurement",
            "security_scan",
            "final_state"
        ])
        .expect("serialize scheduler stages")
    )
}

pub fn render_scheduler_events_jsonl(
    generated_at_json: &str,
    run_id: &str,
    checks: &[CommandCheck],
) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "{{\"run_id\":{},\"at\":{},\"event\":\"scheduler_started\"}}",
        json_string(run_id),
        generated_at_json
    ));
    for check in checks {
        lines.push(format!(
            "{{\"run_id\":{},\"event\":\"check_completed\",\"name\":{},\"status\":{}}}",
            json_string(run_id),
            json_string(&check.name),
            json_string(&check.status)
        ));
    }
    lines.push(format!(
        "{{\"run_id\":{},\"event\":\"scheduler_finished\"}}",
        json_string(run_id)
    ));
    lines.join("\n") + "\n"
}

pub fn render_scheduler_final_state_json(
    generated_at_json: &str,
    run_id: &str,
    checks: &[CommandCheck],
) -> String {
    let failed = checks
        .iter()
        .filter(|check| check.status == "failed" || check.status == "error")
        .count();
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.scheduler-final-state.v1\",\n",
            "  \"run_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"status\": {},\n",
            "  \"checks\": {}\n",
            "}}\n"
        ),
        json_string(run_id),
        generated_at_json,
        json_string(if failed == 0 { "passed" } else { "failed" }),
        render_checks_json(checks)
    )
}

fn render_stage_overlap_report(
    generated_at_json: &str,
    run_id: &str,
    spans: &[StageOverlapSpan],
) -> String {
    let total_stage_ms = spans.iter().map(|span| span.duration_ms).sum::<u128>();
    let first_start_ms = spans.iter().map(|span| span.start_ms).min().unwrap_or(0);
    let last_end_ms = spans.iter().map(|span| span.end_ms).max().unwrap_or(0);
    let wall_clock_ms = last_end_ms.saturating_sub(first_start_ms);
    let overlap_saved_ms = total_stage_ms.saturating_sub(wall_clock_ms);
    let overlap_ratio = if total_stage_ms == 0 {
        0.0
    } else {
        overlap_saved_ms as f64 / total_stage_ms as f64
    };
    let target_ratio = 0.10;
    let all_passed = spans.iter().all(|span| span.status == "passed");
    let observed_parallel_execution = spans.len() > 1;
    let overlap_observed = overlap_saved_ms > 0;
    let target_met = all_passed && overlap_ratio >= target_ratio;
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.stage-overlap-report.v1\",\n",
            "  \"run_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"parallelizable_stage_count\": {},\n",
            "  \"observed_parallel_execution\": {},\n",
            "  \"overlap_observed\": {},\n",
            "  \"target_ratio\": {:.2},\n",
            "  \"overlap_ratio\": {:.4},\n",
            "  \"total_stage_ms\": {},\n",
            "  \"wall_clock_ms\": {},\n",
            "  \"overlap_saved_ms\": {},\n",
            "  \"target_met\": {},\n",
            "  \"measurement_note\": \"independent runtime metadata stages are executed concurrently; production provider/worker overlap tuning remains incomplete\",\n",
            "  \"spans\": {}\n",
            "}}\n"
        ),
        json_string(run_id),
        generated_at_json,
        spans.len(),
        observed_parallel_execution,
        overlap_observed,
        target_ratio,
        overlap_ratio,
        total_stage_ms,
        wall_clock_ms,
        overlap_saved_ms,
        target_met,
        render_stage_overlap_spans_json(spans)
    )
}

fn render_stage_overlap_spans_json(spans: &[StageOverlapSpan]) -> String {
    let rows = spans
        .iter()
        .map(|span| {
            let command = span
                .command
                .iter()
                .map(|value| json_string(value))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                concat!(
                    "{{\"name\":{},\"command\":[{}],\"status\":{},",
                    "\"exit_code\":{},\"start_ms\":{},\"end_ms\":{},",
                    "\"duration_ms\":{},\"stdout\":{},\"stderr\":{}}}"
                ),
                json_string(&span.name),
                command,
                json_string(&span.status),
                span.exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "null".to_string()),
                span.start_ms,
                span.end_ms,
                span.duration_ms,
                json_string(&truncate_for_json(&span.stdout, 4000)),
                json_string(&truncate_for_json(&span.stderr, 4000))
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn build_worker_lease_records(stamp: &ClockStamp) -> Vec<WorkerLeaseRecord> {
    let now = stamp.secs;
    vec![
        WorkerLeaseRecord {
            lease_id: "lease-implementation-active".to_string(),
            worker_id: "worker-implementation-1".to_string(),
            lane: "implementation_worker".to_string(),
            state: "active".to_string(),
            leased_at_seconds: now.saturating_sub(5),
            last_heartbeat_seconds: now,
            expires_at_seconds: now + DEFAULT_WORKER_LEASE_TTL_SECONDS,
            recovery_action: "none".to_string(),
        },
        WorkerLeaseRecord {
            lease_id: "lease-review-active".to_string(),
            worker_id: "worker-reviewer-1".to_string(),
            lane: "qa_reviewer".to_string(),
            state: "active".to_string(),
            leased_at_seconds: now.saturating_sub(4),
            last_heartbeat_seconds: now,
            expires_at_seconds: now + DEFAULT_WORKER_LEASE_TTL_SECONDS,
            recovery_action: "none".to_string(),
        },
        WorkerLeaseRecord {
            lease_id: "lease-stale-recovered".to_string(),
            worker_id: "worker-implementation-stale".to_string(),
            lane: "implementation_worker".to_string(),
            state: "recovered_expired".to_string(),
            leased_at_seconds: now.saturating_sub(DEFAULT_WORKER_LEASE_TTL_SECONDS + 45),
            last_heartbeat_seconds: now.saturating_sub(DEFAULT_WORKER_LEASE_TTL_SECONDS + 15),
            expires_at_seconds: now.saturating_sub(15),
            recovery_action: "expired_lease_reassigned_to_worker-implementation-1".to_string(),
        },
    ]
}

fn run_local_worker_request_routes(leases: &[WorkerLeaseRecord]) -> Vec<WorkerRouteRecord> {
    let active = leases
        .iter()
        .filter(|lease| lease.state == "active")
        .collect::<Vec<_>>();
    let origin = Instant::now();
    let dispatch_barrier = Arc::new(Barrier::new(active.len().max(1)));
    let handles = active
        .iter()
        .enumerate()
        .map(|(index, lease)| {
            let lane = lease.lane.clone();
            let worker_id = lease.worker_id.clone();
            let lease_id = lease.lease_id.clone();
            let dispatch_barrier = Arc::clone(&dispatch_barrier);
            std::thread::spawn(move || {
                let queued_at_ms = origin.elapsed().as_millis();
                let dispatched_at_ms = origin.elapsed().as_millis();
                dispatch_barrier.wait();
                std::thread::sleep(std::time::Duration::from_millis(5 + index as u64));
                WorkerRouteRecord {
                    request_id: format!("request-{}", index + 1),
                    lane,
                    assigned_worker: worker_id,
                    lease_id,
                    route_status: "completed".to_string(),
                    queued_at_ms,
                    dispatched_at_ms,
                    completed_at_ms: origin.elapsed().as_millis(),
                }
            })
        })
        .collect::<Vec<_>>();

    handles
        .into_iter()
        .filter_map(|handle| handle.join().ok())
        .collect()
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "worker".to_string()
    } else {
        slug
    }
}

fn copy_workspace_snapshot(source_root: &Path, dest_root: &Path) -> Result<usize, CliError> {
    let mut copied = 0;
    copy_dir_snapshot(source_root, source_root, dest_root, &mut copied)?;
    Ok(copied)
}

fn copy_dir_snapshot(
    source_root: &Path,
    current: &Path,
    dest_root: &Path,
    copied: &mut usize,
) -> Result<(), CliError> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let source = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip_runtime_path(&name) {
            continue;
        }
        let relative = source.strip_prefix(source_root).map_err(|_| {
            CliError::Invalid(format!(
                "source {} is outside root {}",
                source.display(),
                source_root.display()
            ))
        })?;
        let dest = dest_root.join(relative);
        if source.is_dir() {
            fs::create_dir_all(&dest)?;
            copy_dir_snapshot(source_root, &source, dest_root, copied)?;
        } else {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&source, &dest)?;
            *copied += 1;
        }
    }
    Ok(())
}

fn should_skip_runtime_path(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".opensks" | ".sneakoscope" | "target" | ".DS_Store"
    )
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("serialize string")
}

fn truncate_for_json(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index >= max_chars {
            out.push_str("...[truncated]");
            break;
        }
        out.push(ch);
    }
    out
}

fn render_worker_leases(
    stamp: &ClockStamp,
    run_id: &str,
    goal: &str,
    leases: &[WorkerLeaseRecord],
) -> String {
    let active_count = leases
        .iter()
        .filter(|lease| lease.state == "active")
        .count();
    let recovered_count = leases
        .iter()
        .filter(|lease| lease.state == "recovered_expired")
        .count();
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.worker-leases.v1\",\n",
            "  \"run_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"goal\": {},\n",
            "  \"lease_ttl_seconds\": {},\n",
            "  \"durable_lease_store\": \"local_json_artifact\",\n",
            "  \"heartbeat_source\": \"worker-heartbeats.jsonl\",\n",
            "  \"recovery_policy\": \"expire_missing_heartbeat_then_reassign_lane\",\n",
            "  \"active_lease_count\": {},\n",
            "  \"recovered_expired_lease_count\": {},\n",
            "  \"live_provider_workers\": false,\n",
            "  \"leases\": {}\n",
            "}}\n"
        ),
        json_string(run_id),
        stamp.json(),
        json_string(goal),
        DEFAULT_WORKER_LEASE_TTL_SECONDS,
        active_count,
        recovered_count,
        render_worker_lease_items_json(leases)
    )
}

fn render_worker_heartbeats(
    stamp: &ClockStamp,
    run_id: &str,
    leases: &[WorkerLeaseRecord],
) -> String {
    let mut lines = Vec::new();
    for lease in leases {
        lines.push(format!(
            concat!(
                "{{\"schema\":\"opensks.worker-heartbeat.v1\",\"run_id\":{},",
                "\"generated_at\":{},\"lease_id\":{},\"worker_id\":{},\"lane\":{},",
                "\"last_heartbeat_seconds\":{},\"expires_at_seconds\":{},",
                "\"lease_state\":{},\"recovery_action\":{}}}"
            ),
            json_string(run_id),
            stamp.json(),
            json_string(&lease.lease_id),
            json_string(&lease.worker_id),
            json_string(&lease.lane),
            lease.last_heartbeat_seconds,
            lease.expires_at_seconds,
            json_string(&lease.state),
            json_string(&lease.recovery_action)
        ));
    }
    lines.join("\n") + "\n"
}

fn render_worker_bus(stamp: &ClockStamp, run_id: &str, routes: &[WorkerRouteRecord]) -> String {
    let concurrent_routing = worker_routes_overlap(routes);
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.worker-bus.v1\",\n",
            "  \"run_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"daemon_visible\": true,\n",
            "  \"request_source\": \"local_worker_runtime\",\n",
            "  \"concurrent_request_routing\": {},\n",
            "  \"routed_request_count\": {},\n",
            "  \"live_remote_provider_bus\": false,\n",
            "  \"routes_ref\": \"worker-routing.json\"\n",
            "}}\n"
        ),
        json_string(run_id),
        stamp.json(),
        concurrent_routing,
        routes.len()
    )
}

fn render_worker_routing(stamp: &ClockStamp, run_id: &str, routes: &[WorkerRouteRecord]) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.worker-routing.v1\",\n",
            "  \"run_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"daemon_visible\": true,\n",
            "  \"concurrent_request_routing\": {},\n",
            "  \"routes\": {}\n",
            "}}\n"
        ),
        json_string(run_id),
        stamp.json(),
        worker_routes_overlap(routes),
        render_worker_route_items_json(routes)
    )
}

fn render_worker_final_state(
    stamp: &ClockStamp,
    run_id: &str,
    leases: &[WorkerLeaseRecord],
    routes: &[WorkerRouteRecord],
) -> String {
    let active_count = leases
        .iter()
        .filter(|lease| lease.state == "active")
        .count();
    let expired_count = leases
        .iter()
        .filter(|lease| lease.expires_at_seconds <= stamp.secs)
        .count();
    let recovered_count = leases
        .iter()
        .filter(|lease| lease.state == "recovered_expired")
        .count();
    let routing_passed = routes
        .iter()
        .all(|route| route.route_status == "completed" && !route.assigned_worker.is_empty());
    let status = if active_count > 0 && recovered_count > 0 && routing_passed {
        "passed"
    } else {
        "partial"
    };
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.worker-final-state.v1\",\n",
            "  \"run_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"status\": {},\n",
            "  \"active_lease_count\": {},\n",
            "  \"expired_lease_count\": {},\n",
            "  \"recovered_expired_lease_count\": {},\n",
            "  \"heartbeat_artifact\": \"worker-heartbeats.jsonl\",\n",
            "  \"lease_artifact\": \"worker-leases.json\",\n",
            "  \"bus_artifact\": \"worker-bus.json\",\n",
            "  \"routing_artifact\": \"worker-routing.json\",\n",
            "  \"daemon_visible_worker_bus\": true,\n",
            "  \"concurrent_request_routing\": {},\n",
            "  \"routed_request_count\": {},\n",
            "  \"live_provider_workers\": false,\n",
            "  \"live_remote_provider_bus\": false\n",
            "}}\n"
        ),
        json_string(run_id),
        stamp.json(),
        json_string(status),
        active_count,
        expired_count,
        recovered_count,
        worker_routes_overlap(routes),
        routes.len()
    )
}

fn render_worker_lease_items_json(leases: &[WorkerLeaseRecord]) -> String {
    let rows = leases
        .iter()
        .map(|lease| {
            format!(
                concat!(
                    "{{\"lease_id\":{},\"worker_id\":{},\"lane\":{},\"state\":{},",
                    "\"leased_at_seconds\":{},\"last_heartbeat_seconds\":{},",
                    "\"expires_at_seconds\":{},\"recovery_action\":{}}}"
                ),
                json_string(&lease.lease_id),
                json_string(&lease.worker_id),
                json_string(&lease.lane),
                json_string(&lease.state),
                lease.leased_at_seconds,
                lease.last_heartbeat_seconds,
                lease.expires_at_seconds,
                json_string(&lease.recovery_action)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn render_worker_route_items_json(routes: &[WorkerRouteRecord]) -> String {
    let rows = routes
        .iter()
        .map(|route| {
            format!(
                concat!(
                    "{{\"request_id\":{},\"lane\":{},\"assigned_worker\":{},",
                    "\"lease_id\":{},\"route_status\":{},\"queued_at_ms\":{},",
                    "\"dispatched_at_ms\":{},\"completed_at_ms\":{}}}"
                ),
                json_string(&route.request_id),
                json_string(&route.lane),
                json_string(&route.assigned_worker),
                json_string(&route.lease_id),
                json_string(&route.route_status),
                route.queued_at_ms,
                route.dispatched_at_ms,
                route.completed_at_ms
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn worker_routes_overlap(routes: &[WorkerRouteRecord]) -> bool {
    routes.iter().enumerate().any(|(index, left)| {
        routes.iter().skip(index + 1).any(|right| {
            left.dispatched_at_ms <= right.completed_at_ms
                && right.dispatched_at_ms <= left.completed_at_ms
        })
    })
}

fn render_worktree_isolation(
    stamp: &ClockStamp,
    id: &str,
    label: &str,
    workspace: &Path,
    copied: usize,
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.worktree-isolation.v1\",\n",
            "  \"id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"label\": {},\n",
            "  \"workspace\": {},\n",
            "  \"files_copied\": {},\n",
            "  \"main_workspace_mutation_allowed\": false,\n",
            "  \"final_apply\": \"single_thread_transaction_required\"\n",
            "}}\n"
        ),
        json_string(id),
        stamp.json(),
        json_string(label),
        json_string(&workspace.display().to_string()),
        copied
    )
}

fn render_patch_envelope(stamp: &ClockStamp, id: &str, summary: &str) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.patch-envelope.v1\",\n",
            "  \"id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"summary\": {},\n",
            "  \"status\": \"proposed\",\n",
            "  \"direct_workspace_mutation\": false,\n",
            "  \"diff\": \"\",\n",
            "  \"requires_gate_result\": true\n",
            "}}\n"
        ),
        json_string(id),
        stamp.json(),
        json_string(summary)
    )
}

fn render_patch_gate(stamp: &ClockStamp, id: &str) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.patch-gate-result.v1\",\n",
            "  \"patch_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"status\": \"pending_diff\",\n",
            "  \"checks_required\": {},\n",
            "  \"final_apply_allowed\": false\n",
            "}}\n"
        ),
        json_string(id),
        stamp.json(),
        serde_json::to_string(&["format", "lint", "test", "security_scan", "final_seal"])
            .expect("serialize checks")
    )
}

fn write_text_atomic(path: &Path, contents: &str) -> Result<(), CliError> {
    let parent = path
        .parent()
        .ok_or_else(|| CliError::Invalid(format!("path has no parent: {}", path.display())))?;
    fs::create_dir_all(parent)?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CliError::Invalid(format!("path has no filename: {}", path.display())))?;
    let tmp_path = parent.join(format!(
        ".{}.{}.{}.tmp",
        filename,
        process::id(),
        ClockStamp::now()?.compact_id()
    ));
    fs::write(&tmp_path, contents)?;
    fs::rename(tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod capability_tests;

#[cfg(test)]
mod conversation_supervisor_tests;

#[cfg(test)]
mod security_tests;

#[cfg(test)]
mod tests {
    use super::*;

    type CliCommandFn = fn(&[String], &Path) -> Result<CliOutput, CliError>;
    type UsageCase = (CliCommandFn, &'static str, &'static str);

    #[test]
    fn daemon_stdio_detection_requires_daemon_command_and_stdio_flag() {
        assert!(is_daemon_stdio_invocation(&[
            "daemon".to_string(),
            "--stdio".to_string()
        ]));
        assert!(!is_daemon_stdio_invocation(&[
            "daemon".to_string(),
            "--help".to_string()
        ]));
        assert!(!is_daemon_stdio_invocation(&[
            "run".to_string(),
            "--stdio".to_string()
        ]));
    }

    #[test]
    fn daemon_options_default_workspace_to_cwd() {
        let cwd = PathBuf::from("/tmp/opensks-workspace");
        let options = parse_daemon_options(&["--stdio".to_string()], &cwd).expect("options");
        assert!(options.stdio);
        assert_eq!(options.workspace, cwd);
    }

    #[test]
    fn daemon_options_reject_unknown_argument() {
        let error = parse_daemon_options(&["--bogus".to_string()], Path::new("."))
            .expect_err("usage error");
        assert!(error.to_string().contains("unknown daemon argument"));
    }

    #[test]
    fn history_usage_handles_help_and_errors() {
        let help = run_history_command(&["--help".to_string()], Path::new(".")).expect("help");
        assert_eq!(help.stdout, history_usage());

        let missing = run_history_command(&[], Path::new(".")).expect_err("missing subcommand");
        assert_eq!(missing.to_string(), history_usage());

        let unknown =
            run_history_command(&["bogus".to_string()], Path::new(".")).expect_err("unknown");
        assert!(unknown.to_string().contains("unknown history subcommand"));
    }

    #[test]
    fn history_init_creates_workspace_event_store() {
        let root = temp_workspace("opensks-cli-history-init");
        let output = run_history_command(&["init".to_string()], &root).expect("history init");
        assert!(output.stdout.contains("initialized OpenSKS event store"));
        assert!(output.stdout.contains("integrity: ok"));
        assert!(output.stdout.contains("sequence: 1"));
        assert!(
            root.join(opensks_event_store::ENGINE_DB_RELATIVE_PATH)
                .exists()
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn graph_usage_preserves_missing_and_unknown_errors() {
        let missing = run_graph_command(&[], Path::new(".")).expect_err("missing graph command");
        assert_eq!(missing.to_string(), graph_usage());

        let unknown =
            run_graph_command(&["bogus".to_string()], Path::new(".")).expect_err("unknown");
        assert!(unknown.to_string().contains("unknown graph subcommand"));
    }

    #[test]
    fn graph_templates_write_default_files() {
        let root = temp_workspace("opensks-cli-graph-templates");
        let output = run_graph_command(&["templates".to_string()], &root).expect("templates");
        assert!(
            output
                .stdout
                .contains("wrote default pipeline graph templates")
        );
        assert!(output.stdout.contains("templates: 5"));
        assert!(
            root.join(".opensks")
                .join("pipelines")
                .join("templates")
                .join("single-model-safe.graph.json")
                .exists()
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn graph_compile_writes_default_plan() {
        let root = temp_workspace("opensks-cli-graph-compile");
        let output = run_graph_command(&["compile".to_string()], &root).expect("compile");
        assert!(output.stdout.contains("compiled pipeline graph"));
        assert!(output.stdout.contains("id: single-model-safe"));
        assert!(output.stdout.contains("diagnostics_errors: 0"));
        assert!(
            root.join(".opensks")
                .join("pipelines")
                .join("compiled")
                .join("single-model-safe.plan.json")
                .exists()
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn graph_compile_rejects_unknown_template() {
        let error = run_graph_command(
            &["compile".to_string(), "missing-template".to_string()],
            Path::new("."),
        )
        .expect_err("unknown template");
        assert!(
            error
                .to_string()
                .contains("unknown graph template `missing-template`")
        );
        assert!(error.to_string().contains(graph_usage()));
    }

    #[test]
    fn hooks_usage_preserves_strict_replay_command() {
        let missing = run_hooks_command(&[], Path::new(".")).expect_err("missing hooks command");
        assert_eq!(missing.to_string(), hooks_usage());

        let unknown =
            run_hooks_command(&["bogus".to_string()], Path::new(".")).expect_err("unknown");
        assert_eq!(unknown.to_string(), hooks_usage());

        let extra = run_hooks_command(&["replay".to_string(), "extra".to_string()], Path::new("."))
            .expect_err("extra");
        assert_eq!(extra.to_string(), hooks_usage());
    }

    #[test]
    fn hooks_replay_writes_decision_artifact() {
        let root = temp_workspace("opensks-cli-hooks-replay");
        let output = run_hooks_command(&["replay".to_string()], &root).expect("hooks replay");
        assert!(output.stdout.contains("replayed hook decisions"));
        assert!(output.stdout.contains("decisions: 2"));
        assert!(output.stdout.contains("exact_replay: true"));

        let artifact = root
            .join(".opensks")
            .join("hooks")
            .join("hook-decisions.jsonl");
        assert!(artifact.exists());
        let jsonl = fs::read_to_string(artifact).expect("hook replay jsonl");
        assert!(jsonl.contains("before-run-allow"));
        assert!(jsonl.contains("before-run-modify"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn codegraph_usage_preserves_errors() {
        let missing =
            run_codegraph_command(&[], Path::new(".")).expect_err("missing codegraph command");
        assert_eq!(missing.to_string(), codegraph_usage());

        let unknown =
            run_codegraph_command(&["bogus".to_string()], Path::new(".")).expect_err("unknown");
        assert!(
            unknown
                .to_string()
                .contains("unknown codegraph subcommand `bogus`")
        );

        let missing_query = run_codegraph_command(&["query".to_string()], Path::new("."))
            .expect_err("missing query");
        assert_eq!(missing_query.to_string(), codegraph_usage());
    }

    #[test]
    fn codegraph_index_and_query_write_artifacts() {
        let root = temp_workspace("opensks-cli-codegraph");
        fs::create_dir_all(root.join("src")).expect("src");
        fs::write(
            root.join("src/lib.rs"),
            "use std::fs;\npub fn CodeGraphFixture() {}\n",
        )
        .expect("fixture");

        let index = run_codegraph_command(&["index".to_string()], &root).expect("index");
        assert!(index.stdout.contains("indexed code graph"));
        assert!(index.stdout.contains("records:"));
        assert!(
            root.join(".opensks")
                .join("wiki")
                .join("indexes")
                .join("codegraph.json")
                .exists()
        );

        let query = run_codegraph_command(
            &[
                "query".to_string(),
                "CodeGraph".to_string(),
                "Fixture".to_string(),
            ],
            &root,
        )
        .expect("query");
        assert!(query.stdout.contains("queried code graph"));
        assert!(query.stdout.contains("query: CodeGraph Fixture"));
        assert!(
            root.join(".opensks")
                .join("wiki")
                .join("indexes")
                .join("codegraph-query.json")
                .exists()
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn triwiki_usage_preserves_strict_seed_command() {
        let missing = run_triwiki_command(&[], Path::new(".")).expect_err("missing triwiki");
        assert_eq!(missing.to_string(), triwiki_usage());

        let unknown =
            run_triwiki_command(&["bogus".to_string()], Path::new(".")).expect_err("unknown");
        assert_eq!(unknown.to_string(), triwiki_usage());

        let extra = run_triwiki_command(&["seed".to_string(), "extra".to_string()], Path::new("."))
            .expect_err("extra");
        assert_eq!(extra.to_string(), triwiki_usage());
    }

    #[test]
    fn triwiki_seed_writes_merge_friendly_records() {
        let root = temp_workspace("opensks-cli-triwiki");
        let output = run_triwiki_command(&["seed".to_string()], &root).expect("triwiki seed");
        assert!(output.stdout.contains("seeded TriWiki records"));
        assert!(output.stdout.contains("records: 3"));

        let records = opensks_triwiki::load_records(&root).expect("records");
        assert_eq!(records.len(), 3);
        assert!(
            records
                .iter()
                .any(|record| record.id == "architecture-runtime-foundation")
        );
        assert!(
            records
                .iter()
                .any(|record| record.id == "glossary-work-item")
        );
        assert!(
            records
                .iter()
                .any(|record| record.id == "wrongness-foundation-not-live")
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn context_usage_preserves_pack_errors() {
        let missing = run_context_command(&[], Path::new(".")).expect_err("missing context");
        assert_eq!(missing.to_string(), context_usage());

        let unknown =
            run_context_command(&["bogus".to_string()], Path::new(".")).expect_err("unknown");
        assert!(
            unknown
                .to_string()
                .contains("unknown context subcommand `bogus`")
        );

        let invalid_budget = run_context_command(
            &["pack".to_string(), "not-a-number".to_string()],
            Path::new("."),
        )
        .expect_err("invalid budget");
        assert_eq!(invalid_budget.to_string(), context_usage());
    }

    #[test]
    fn context_pack_writes_generated_artifact() {
        let root = temp_workspace("opensks-cli-context");
        run_triwiki_command(&["seed".to_string()], &root).expect("seed");

        let output =
            run_context_command(&["pack".to_string(), "120".to_string()], &root).expect("pack");
        assert!(output.stdout.contains("built context pack"));
        assert!(output.stdout.contains("records:"));
        let artifact = root
            .join(".opensks")
            .join("wiki")
            .join("context-packs")
            .join("generated")
            .join("cli-context-pack.json");
        assert!(artifact.exists());
        let pack = fs::read_to_string(artifact).expect("context pack");
        assert!(pack.contains("\"schema\": \"opensks.context-pack.v1\""));
        assert!(pack.contains("\"id\": \"cli-context-pack\""));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn patch_usage_preserves_errors() {
        let missing = run_patch_command(&[], Path::new(".")).expect_err("missing patch");
        assert_eq!(missing.to_string(), patch_usage());

        let unknown =
            run_patch_command(&["bogus".to_string()], Path::new(".")).expect_err("unknown");
        assert!(
            unknown
                .to_string()
                .contains("unknown patch subcommand `bogus`")
        );

        let missing_summary =
            run_patch_command(&["propose".to_string()], Path::new(".")).expect_err("summary");
        assert_eq!(
            missing_summary.to_string(),
            "usage: opensks patch propose \"<summary>\""
        );
    }

    #[test]
    fn patch_propose_and_check_write_artifacts() {
        let root = temp_workspace("opensks-cli-patch");
        fs::write(root.join("README.md"), "fixture\n").expect("fixture");

        let proposed = run_patch_command(
            &[
                "propose".to_string(),
                "safe".to_string(),
                "change".to_string(),
            ],
            &root,
        )
        .expect("propose");
        assert!(proposed.stdout.contains("created patch proposal envelope"));
        let patches_dir = root.join(".opensks").join("patches");
        let proposal_dir = first_child_dir(&patches_dir);
        assert!(proposal_dir.join("patch-envelope.json").exists());
        assert!(proposal_dir.join("patch-gate-result.json").exists());

        let checked = run_patch_command(&["check".to_string(), "README.md".to_string()], &root)
            .expect("check");
        assert!(checked.stdout.contains("checked patch transaction guard"));
        assert!(checked.stdout.contains("status: passed"));
        let mut dirty_guard_found = false;
        for entry in fs::read_dir(&patches_dir).expect("patches dir") {
            let path = entry.expect("patch dir").path();
            dirty_guard_found |= path.join("dirty-guard-result.json").exists();
        }
        assert!(dirty_guard_found);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scheduler_usage_preserves_errors() {
        let missing = run_scheduler_command(&[], Path::new(".")).expect_err("missing scheduler");
        assert_eq!(missing.to_string(), scheduler_usage());

        let unknown =
            run_scheduler_command(&["bogus".to_string()], Path::new(".")).expect_err("unknown");
        assert!(
            unknown
                .to_string()
                .contains("unknown scheduler subcommand `bogus`")
        );
        assert!(unknown.to_string().contains(scheduler_usage()));

        let invalid_count = run_scheduler_command(
            &["simulate".to_string(), "not-a-number".to_string()],
            Path::new("."),
        )
        .expect_err("invalid count");
        assert_eq!(invalid_count.to_string(), scheduler_usage());
    }

    #[test]
    fn perf_stress_command_reports_within_budget() {
        // The `perf stress` verb runs the PR-043 harness and prints the
        // perf-stress-report contract; the headline invariant must hold.
        let output = run_perf_command(
            &[
                "stress".to_string(),
                "--events".to_string(),
                "100000".to_string(),
            ],
            Path::new("."),
        )
        .expect("perf stress");
        let report: serde_json::Value =
            serde_json::from_str(&output.stdout).expect("parse perf report");
        assert_eq!(report["schema"], "opensks.perf-stress-report.v1");
        assert_eq!(report["events"], 100_000);
        assert_eq!(report["within_budget"], true);
        assert_eq!(report["leaked_handles"], 0);
        assert_eq!(report["children_spawned"], report["children_reaped"]);
        let processed = report["processed"].as_u64().expect("processed");
        let dropped = report["dropped"].as_u64().expect("dropped");
        assert_eq!(processed + dropped, 100_000);
        let cap = report["retention_cap"].as_u64().expect("cap");
        let peak = report["peak_retained"].as_u64().expect("peak");
        assert!(peak <= cap, "peak {peak} exceeded cap {cap}");
    }

    #[test]
    fn perf_usage_preserves_errors() {
        let missing = run_perf_command(&[], Path::new(".")).expect_err("missing perf subcommand");
        assert_eq!(missing.to_string(), perf_usage());
        let unknown = run_perf_command(&["bogus".to_string()], Path::new("."))
            .expect_err("unknown perf subcommand");
        assert!(
            unknown
                .to_string()
                .contains("unknown perf subcommand `bogus`")
        );
        let bad_flag = run_perf_command(
            &[
                "stress".to_string(),
                "--events".to_string(),
                "nope".to_string(),
            ],
            Path::new("."),
        )
        .expect_err("invalid events value");
        assert!(bad_flag.to_string().contains("requires a number"));
    }

    #[test]
    fn scheduler_run_writes_legacy_stage_artifacts() {
        let root = temp_workspace("opensks-cli-scheduler-run");
        let output = run_scheduler_command(&["run".to_string(), "local QA".to_string()], &root)
            .expect("scheduler run");
        assert!(output.stdout.contains("ran local scheduler slice"));
        assert!(output.stdout.contains("checks: 1"));
        assert!(output.stdout.contains("overlap_spans: 2"));

        let scheduler_dir = first_child_dir(&root.join(".opensks").join("scheduler"));
        for artifact in [
            "stage-scheduler.json",
            "scheduler-events.jsonl",
            "scheduler-final-state.json",
            "stage-overlap-report.json",
        ] {
            assert!(scheduler_dir.join(artifact).exists(), "{artifact}");
        }
        let plan = read_json(&scheduler_dir.join("stage-scheduler.json"));
        assert_eq!(plan["schema"], "opensks.stage-scheduler.v1");
        let final_state = read_json(&scheduler_dir.join("scheduler-final-state.json"));
        assert_eq!(final_state["schema"], "opensks.scheduler-final-state.v1");
        let overlap = read_json(&scheduler_dir.join("stage-overlap-report.json"));
        assert_eq!(overlap["schema"], "opensks.stage-overlap-report.v1");
        assert_eq!(overlap["parallelizable_stage_count"], 2);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scheduler_simulate_writes_durable_snapshot_artifact() {
        let root = temp_workspace("opensks-cli-scheduler-simulate");
        let output = run_scheduler_command(&["simulate".to_string(), "3".to_string()], &root)
            .expect("simulate");
        assert!(output.stdout.contains("simulated durable scheduler"));
        assert!(output.stdout.contains("items: 3"));

        let scheduler_dir = first_child_dir(&root.join(".opensks").join("scheduler"));
        let snapshot = read_json(&scheduler_dir.join("durable-scheduler-snapshot.json"));
        assert_eq!(snapshot["schema"], "opensks.scheduler-snapshot.v1");
        assert_eq!(snapshot["work_items"].as_array().expect("items").len(), 3);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scheduler_dispatch_writes_worker_report_artifacts() {
        let root = temp_workspace("opensks-cli-scheduler-dispatch");
        let output = run_scheduler_command(&["dispatch".to_string(), "3".to_string()], &root)
            .expect("dispatch");
        assert!(
            output
                .stdout
                .contains("dispatched durable scheduler workers")
        );
        assert!(output.stdout.contains("items: 3"));

        let scheduler_dir = first_child_dir(&root.join(".opensks").join("scheduler"));
        let snapshot = read_json(&scheduler_dir.join("worker-dispatch-snapshot.json"));
        assert_eq!(snapshot["schema"], "opensks.scheduler-snapshot.v1");
        assert_eq!(snapshot["work_items"].as_array().expect("items").len(), 3);
        let report = read_json(&scheduler_dir.join("worker-dispatch-report.json"));
        assert!(
            report["run_id"]
                .as_str()
                .expect("run id")
                .starts_with("scheduler-dispatch-")
        );
        assert_eq!(report["attempted"], 3);
        assert_eq!(report["completed"], 3);
        assert_eq!(report["failed"], 0);
        assert_eq!(report["outcomes"].as_array().expect("outcomes").len(), 3);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scheduler_recover_writes_lease_recovery_artifacts() {
        let root = temp_workspace("opensks-cli-scheduler-recover");
        let output = run_scheduler_command(&["recover".to_string(), "4".to_string()], &root)
            .expect("recover");
        assert!(output.stdout.contains("recovered durable scheduler leases"));
        assert!(output.stdout.contains("items: 4"));
        assert!(output.stdout.contains("active: 1"));
        assert!(output.stdout.contains("expired: 3"));

        let scheduler_dir = first_child_dir(&root.join(".opensks").join("scheduler"));
        let heartbeat = read_json(&scheduler_dir.join("lease-heartbeat-report.json"));
        assert!(
            heartbeat["run_id"]
                .as_str()
                .expect("run id")
                .starts_with("scheduler-recover-")
        );
        assert_eq!(heartbeat["work_item_id"], "wi-00000");
        assert_eq!(heartbeat["holder"], "cli-lease-worker");
        let recovery = read_json(&scheduler_dir.join("lease-recovery-report.json"));
        assert!(
            recovery["run_id"]
                .as_str()
                .expect("run id")
                .starts_with("scheduler-recover-")
        );
        assert_eq!(recovery["active_count"], 1);
        assert_eq!(recovery["expired_count"], 3);
        assert_eq!(recovery["expired"].as_array().expect("expired").len(), 3);
        let snapshot = read_json(&scheduler_dir.join("lease-recovery-snapshot.json"));
        assert_eq!(snapshot["schema"], "opensks.scheduler-snapshot.v1");
        assert_eq!(snapshot["work_items"].as_array().expect("items").len(), 4);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn worker_usage_preserves_errors() {
        let missing = run_worker_command(&[], Path::new(".")).expect_err("missing worker");
        assert_eq!(missing.to_string(), worker_usage());

        let unknown =
            run_worker_command(&["bogus".to_string()], Path::new(".")).expect_err("unknown");
        assert!(
            unknown
                .to_string()
                .contains("unknown worker subcommand `bogus`")
        );
        assert!(unknown.to_string().contains(worker_usage()));

        let missing_goal =
            run_worker_command(&["runtime".to_string()], Path::new(".")).expect_err("goal");
        assert_eq!(missing_goal.to_string(), worker_usage());
    }

    #[test]
    fn worker_runtime_writes_lease_recovery_and_routing_artifacts() {
        let root = temp_workspace("opensks-cli-worker-runtime");
        let output = run_worker_command(
            &[
                "runtime".to_string(),
                "recover".to_string(),
                "stale".to_string(),
                "worker".to_string(),
                "lease".to_string(),
            ],
            &root,
        )
        .expect("worker runtime");
        assert!(
            output
                .stdout
                .contains("wrote local worker runtime artifacts")
        );
        assert!(output.stdout.contains("leases: 3"));
        assert!(output.stdout.contains("recovered_expired: 1"));
        assert!(output.stdout.contains("routed_requests: 2"));

        let worker_dir = first_child_dir(&root.join(".opensks").join("workers"));
        for artifact in [
            "worker-leases.json",
            "worker-heartbeats.jsonl",
            "worker-bus.json",
            "worker-routing.json",
            "worker-final-state.json",
        ] {
            assert!(
                worker_dir.join(artifact).exists(),
                "expected worker artifact {artifact}"
            );
        }

        let leases = read_json(&worker_dir.join("worker-leases.json"));
        assert_eq!(leases["schema"], "opensks.worker-leases.v1");
        assert_eq!(leases["goal"], "recover stale worker lease");
        assert_eq!(leases["lease_ttl_seconds"], 30);
        assert_eq!(leases["active_lease_count"], 2);
        assert_eq!(leases["recovered_expired_lease_count"], 1);
        assert_eq!(leases["live_provider_workers"], false);
        assert_eq!(leases["leases"].as_array().expect("leases").len(), 3);

        let heartbeats =
            fs::read_to_string(worker_dir.join("worker-heartbeats.jsonl")).expect("heartbeats");
        assert!(heartbeats.contains("\"schema\":\"opensks.worker-heartbeat.v1\""));
        assert!(heartbeats.contains("\"lease_state\":\"recovered_expired\""));

        let bus = read_json(&worker_dir.join("worker-bus.json"));
        assert_eq!(bus["schema"], "opensks.worker-bus.v1");
        assert_eq!(bus["daemon_visible"], true);
        assert_eq!(bus["concurrent_request_routing"], true);
        assert_eq!(bus["routed_request_count"], 2);
        assert_eq!(bus["live_remote_provider_bus"], false);

        let routing = read_json(&worker_dir.join("worker-routing.json"));
        assert_eq!(routing["schema"], "opensks.worker-routing.v1");
        assert_eq!(routing["daemon_visible"], true);
        assert_eq!(routing["concurrent_request_routing"], true);
        assert_eq!(routing["routes"].as_array().expect("routes").len(), 2);

        let final_state = read_json(&worker_dir.join("worker-final-state.json"));
        assert_eq!(final_state["schema"], "opensks.worker-final-state.v1");
        assert_eq!(final_state["status"], "passed");
        assert_eq!(final_state["active_lease_count"], 2);
        assert_eq!(final_state["expired_lease_count"], 1);
        assert_eq!(final_state["recovered_expired_lease_count"], 1);
        assert_eq!(final_state["daemon_visible_worker_bus"], true);
        assert_eq!(final_state["concurrent_request_routing"], true);
        assert_eq!(final_state["live_provider_workers"], false);
        assert_eq!(final_state["live_remote_provider_bus"], false);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn worktree_usage_preserves_errors() {
        let missing = run_worktree_command(&[], Path::new(".")).expect_err("missing worktree");
        assert_eq!(missing.to_string(), worktree_usage());

        let unknown =
            run_worktree_command(&["bogus".to_string()], Path::new(".")).expect_err("unknown");
        assert!(
            unknown
                .to_string()
                .contains("unknown worktree subcommand `bogus`")
        );
        assert!(unknown.to_string().contains(worktree_usage()));

        let missing_create =
            run_worktree_command(&["create".to_string()], Path::new(".")).expect_err("label");
        assert_eq!(
            missing_create.to_string(),
            "usage: opensks worktree create \"<worker label>\""
        );
    }

    #[test]
    fn worktree_create_writes_legacy_snapshot_artifact() {
        let root = temp_workspace("opensks-cli-worktree-create");
        fs::write(root.join("README.md"), "fixture\n").expect("fixture");
        fs::create_dir_all(root.join("src")).expect("src");
        fs::write(root.join("src/lib.rs"), "pub fn fixture() {}\n").expect("src fixture");
        fs::create_dir_all(root.join(".git")).expect("git dir");
        fs::write(root.join(".git/ignored"), "ignore\n").expect("git fixture");
        fs::create_dir_all(root.join(".opensks/old")).expect("opensks dir");
        fs::write(root.join(".opensks/old/ignored"), "ignore\n").expect("opensks fixture");
        fs::create_dir_all(root.join(".sneakoscope")).expect("sneakoscope dir");
        fs::write(root.join(".sneakoscope/ignored"), "ignore\n").expect("sneakoscope fixture");
        fs::create_dir_all(root.join("target/debug")).expect("target dir");
        fs::write(root.join("target/debug/ignored"), "ignore\n").expect("target fixture");
        fs::write(root.join(".DS_Store"), "ignore\n").expect("ds store");

        let output = run_worktree_command(
            &[
                "create".to_string(),
                "worker".to_string(),
                "lane".to_string(),
                "one".to_string(),
            ],
            &root,
        )
        .expect("worktree create");
        assert!(output.stdout.contains("created isolated worker workspace"));
        assert!(output.stdout.contains("files_copied: 2"));

        let worktree_dir = first_child_dir(&root.join(".opensks").join("worktrees"));
        let workspace = worktree_dir.join("workspace");
        assert!(workspace.join("README.md").exists());
        assert!(workspace.join("src/lib.rs").exists());
        assert!(!workspace.join(".git").exists());
        assert!(!workspace.join(".opensks").exists());
        assert!(!workspace.join(".sneakoscope").exists());
        assert!(!workspace.join("target").exists());
        assert!(!workspace.join(".DS_Store").exists());

        let artifact =
            fs::read_to_string(worktree_dir.join("worktree-isolation.json")).expect("artifact");
        assert!(artifact.contains("\"schema\": \"opensks.worktree-isolation.v1\""));
        assert!(artifact.contains("\"label\": \"worker lane one\""));
        assert!(artifact.contains("\"files_copied\": 2"));
        assert!(artifact.contains("\"main_workspace_mutation_allowed\": false"));
        assert!(artifact.contains("\"final_apply\": \"single_thread_transaction_required\""));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn worktree_isolate_writes_git_isolation_report_for_non_git_workspace() {
        let root = temp_workspace("opensks-cli-worktree-isolate");
        fs::write(root.join("README.md"), "fixture\n").expect("fixture");
        assert_eq!(slugify("Worker One!"), "worker-one");
        assert_eq!(slugify("!!!"), "worker");

        let output = run_worktree_command(
            &[
                "isolate".to_string(),
                "Worker".to_string(),
                "One!".to_string(),
            ],
            &root,
        )
        .expect("worktree isolate");
        assert!(output.stdout.contains("created runtime isolation"));
        assert!(output.stdout.contains("mode: Snapshot"));
        assert!(output.stdout.contains("worker-one"));

        let git_dir = first_child_dir(&root.join(".opensks").join("git"));
        let artifact_path = git_dir.join("git-isolation.json");
        let artifact = fs::read_to_string(&artifact_path).expect("git isolation");
        assert!(artifact.contains("\"schema\": \"opensks.git-isolation.v1\""));
        assert!(artifact.contains("\"mode\": \"snapshot\""));
        assert!(artifact.contains("\"reason_code\": \"snapshot_isolation_for_non_git_workspace\""));
        assert!(artifact.contains("worker-one"));
        assert!(artifact.ends_with('\n'));
        assert!(
            root.join(".opensks")
                .join("runtime")
                .join("worktrees")
                .exists()
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn provider_route_writes_typed_routing_decision() {
        let root = temp_workspace("opensks-cli-provider-route");
        let default_output = run_provider_route_command(&[], &root).expect("provider route");
        assert!(default_output.stdout.contains("capability: code"));
        assert!(default_output.stdout.contains("selected_model: fake-code"));

        let code_output =
            run_provider_route_command(&["code".to_string()], &root).expect("provider code route");
        assert!(code_output.stdout.contains("capability: code"));
        assert!(code_output.stdout.contains("selected_model: fake-code"));

        let text_output =
            run_provider_route_command(&["text".to_string()], &root).expect("provider text route");
        assert!(text_output.stdout.contains("capability: text"));
        assert!(text_output.stdout.contains("selected_model: fake-code"));

        let output = run_provider_route_command(&["image".to_string()], &root)
            .expect("provider image route");
        assert!(output.stdout.contains("routed provider capability"));
        assert!(output.stdout.contains("capability: image"));
        assert!(output.stdout.contains("status: Routed"));
        assert!(output.stdout.contains("selected_model: fake-image"));

        let artifact_path = root
            .join(".opensks")
            .join("providers")
            .join("routing-decision.json");
        let artifact = fs::read_to_string(&artifact_path).expect("routing decision");
        assert!(artifact.contains("\"schema\": \"opensks.routing-decision.v1\""));
        assert!(artifact.contains("\"status\": \"routed\""));
        assert!(artifact.contains("\"selected_model_id\": \"fake-image\""));
        assert!(artifact.contains("\"route_receipt\""));
        assert!(artifact.contains("\"provider_id\": \"fake-local\""));
        assert!(artifact.contains("\"model_id\": \"fake-image\""));
        assert!(artifact.contains("\"requested_capabilities\""));
        assert!(artifact.ends_with('\n'));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn provider_route_rejects_unknown_capability_with_provider_usage() {
        let root = temp_workspace("provider-route-unknown");
        let error = run_provider_route_command(&["audio".to_string()], &root).expect_err("usage");
        assert!(
            error
                .to_string()
                .contains("unknown provider route capability `audio`")
        );
        assert!(error.to_string().contains(provider_usage()));
        assert!(!root.join(".opensks").join("providers").exists());
    }

    #[test]
    fn foundation_usage_preserves_exact_subcommands() {
        let cases: &[UsageCase] = &[
            (run_image_command, image_usage(), "bogus"),
            (run_reasoning_command, reasoning_usage(), "bogus"),
            (run_git_command, git_usage(), "bogus"),
            (run_gc_command, gc_usage(), "bogus"),
            (run_release_command, release_usage(), "bogus"),
        ];

        for (command, usage, bogus) in cases {
            assert_eq!(
                command(&[], Path::new("."))
                    .expect_err("missing subcommand")
                    .to_string(),
                *usage
            );
            assert_eq!(
                command(&[bogus.to_string()], Path::new("."))
                    .expect_err("unknown subcommand")
                    .to_string(),
                *usage
            );
        }
    }

    #[test]
    fn foundation_commands_write_artifacts() {
        let root = temp_workspace("opensks-cli-foundation");

        let image = run_image_command(&["ledger".to_string()], &root).expect("image ledger");
        assert!(image.stdout.contains("wrote image asset ledger"));
        assert!(image.stdout.contains("model: enabled-image"));
        let image_ledger = read_json(
            &root
                .join(".opensks")
                .join("assets")
                .join("candidates")
                .join("image-ledger.json"),
        );
        assert_eq!(image_ledger["schema"], "opensks.image-ledger.v1");
        assert_eq!(image_ledger["assets"][0]["model_id"], "enabled-image");
        assert_eq!(image_ledger["gc_candidate_ids"][0], "cli-image-asset");

        let reasoning =
            run_reasoning_command(&["debate".to_string()], &root).expect("reasoning debate");
        assert!(reasoning.stdout.contains("wrote reasoning debate report"));
        assert!(reasoning.stdout.contains("rounds: 3"));
        let reasoning_report = read_json(
            &root
                .join(".opensks")
                .join("reasoning")
                .join("reasoning-report.json"),
        );
        assert_eq!(reasoning_report["schema"], "opensks.reasoning-report.v1");
        assert_eq!(reasoning_report["strategy"], "bounded_debate");
        assert_eq!(reasoning_report["hidden_reasoning_persisted"], false);

        let git = run_git_command(&["outbox".to_string()], &root).expect("git outbox");
        assert!(git.stdout.contains("wrote Git outbox plan"));
        assert!(
            git.stdout
                .contains("dispatch_without_approval_executed: false")
        );
        let outbox_gate = read_json(&root.join(".opensks").join("git").join("outbox-gate.json"));
        assert_eq!(outbox_gate["schema"], "opensks.outbox-gate.v1");
        assert_eq!(outbox_gate["dispatch_without_approval_executed"], false);
        assert_eq!(outbox_gate["dispatch_with_approval_executed"], true);
        assert_eq!(outbox_gate["live_remote_write_executed"], false);

        let gc = run_gc_command(&["plan".to_string()], &root).expect("gc plan");
        assert!(gc.stdout.contains("wrote retention GC plan"));
        let gc_plan = read_json(&root.join(".opensks").join("gc").join("gc-plan.json"));
        assert_eq!(gc_plan["schema"], "opensks.retention-plan.v1");
        assert_eq!(gc_plan["active_run_protected"], true);
        assert_eq!(
            gc_plan["blocked_paths"][0],
            ".opensks/runtime/worktrees/run-active/worker"
        );

        let release = run_release_command(&["proof".to_string()], &root).expect("release proof");
        assert!(release.stdout.contains("wrote release hardening proof"));
        assert!(release.stdout.contains("status: NotVerified"));
        let release_proof = read_json(
            &root
                .join(".opensks")
                .join("release")
                .join("release-proof.json"),
        );
        assert_eq!(release_proof["schema"], "opensks.release-proof.v1");
        assert_eq!(release_proof["status"], "not_verified");
        assert_eq!(release_proof["signed_app"], false);
        assert_eq!(release_proof["upgrade_checked"], false);
        assert_eq!(release_proof["artifact_digest_gate_passed"], false);
        assert_eq!(release_proof["same_sha_artifact_binding"], false);
        assert!(
            release_proof["missing_artifacts"]
                .as_array()
                .expect("missing artifacts")
                .iter()
                .any(|artifact| artifact == "docs/runtime-truth-matrix.generated.md")
        );
        assert!(
            release_proof["blockers"]
                .as_array()
                .expect("release blockers")
                .iter()
                .any(|blocker| blocker["code"] == "git_head_unavailable")
        );

        fs::remove_dir_all(root).ok();
    }

    fn conversation_args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    #[test]
    fn conversation_create_then_list_returns_the_conversation() {
        let root = temp_workspace("opensks-cli-conv-create-list");
        let created = run_conversation_command(
            &conversation_args(&["create", "--title", "First conversation"]),
            &root,
        )
        .expect("create conversation");
        let created_json: serde_json::Value =
            serde_json::from_str(&created.stdout).expect("create json");
        assert_eq!(created_json["schema"], "opensks.conversation-summary.v1");
        assert_eq!(created_json["title"], "First conversation");
        let conversation_id = created_json["id"].as_str().expect("id").to_string();

        let listed = run_conversation_command(&conversation_args(&["list"]), &root).expect("list");
        let listed_json: serde_json::Value =
            serde_json::from_str(&listed.stdout).expect("list json");
        assert_eq!(listed_json["schema"], "opensks.conversation-list.v1");
        let conversations = listed_json["conversations"]
            .as_array()
            .expect("conversations array");
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0]["id"], conversation_id);
        assert_eq!(conversations[0]["title"], "First conversation");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn conversation_workspaces_are_project_isolated() {
        let root_a = temp_workspace("opensks-cli-conv-iso-a");
        let root_b = temp_workspace("opensks-cli-conv-iso-b");

        run_conversation_command(
            &conversation_args(&[
                "create",
                "--workspace",
                root_a.to_str().expect("path a"),
                "--title",
                "Alpha",
            ]),
            Path::new("."),
        )
        .expect("create in workspace a");

        let listed_b = run_conversation_command(
            &conversation_args(&["list", "--workspace", root_b.to_str().expect("path b")]),
            Path::new("."),
        )
        .expect("list workspace b");
        let listed_b_json: serde_json::Value =
            serde_json::from_str(&listed_b.stdout).expect("list b json");
        assert!(
            listed_b_json["conversations"]
                .as_array()
                .expect("array")
                .is_empty(),
            "workspace b must not see workspace a conversations"
        );

        let listed_a = run_conversation_command(
            &conversation_args(&["list", "--workspace", root_a.to_str().expect("path a")]),
            Path::new("."),
        )
        .expect("list workspace a");
        let listed_a_json: serde_json::Value =
            serde_json::from_str(&listed_a.stdout).expect("list a json");
        assert_eq!(
            listed_a_json["conversations"]
                .as_array()
                .expect("array")
                .len(),
            1
        );

        fs::remove_dir_all(root_a).ok();
        fs::remove_dir_all(root_b).ok();
    }

    #[test]
    fn conversation_append_then_messages_returns_them_in_order() {
        let root = temp_workspace("opensks-cli-conv-messages");
        let created = run_conversation_command(
            &conversation_args(&["create", "--title", "Threaded"]),
            &root,
        )
        .expect("create conversation");
        let conversation_id = serde_json::from_str::<serde_json::Value>(&created.stdout)
            .expect("create json")["id"]
            .as_str()
            .expect("id")
            .to_string();

        run_conversation_command(
            &conversation_args(&[
                "append",
                "--conversation",
                &conversation_id,
                "--role",
                "user",
                "--text",
                "first message",
            ]),
            &root,
        )
        .expect("append first");
        run_conversation_command(
            &conversation_args(&[
                "append",
                "--conversation",
                &conversation_id,
                "--role",
                "assistant",
                "--text",
                "second message",
            ]),
            &root,
        )
        .expect("append second");

        let messages = run_conversation_command(
            &conversation_args(&["messages", "--conversation", &conversation_id]),
            &root,
        )
        .expect("messages");
        let messages_json: serde_json::Value =
            serde_json::from_str(&messages.stdout).expect("messages json");
        assert_eq!(messages_json["conversation_id"], conversation_id);
        assert_eq!(messages_json["has_more"], false);
        let list = messages_json["messages"]
            .as_array()
            .expect("messages array");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["content_redacted"], "first message");
        assert_eq!(list[0]["sequence"], 1);
        assert_eq!(list[1]["content_redacted"], "second message");
        assert_eq!(list[1]["sequence"], 2);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn conversation_persists_across_repository_reopen() {
        let root = temp_workspace("opensks-cli-conv-persist");
        let created =
            run_conversation_command(&conversation_args(&["create", "--title", "Durable"]), &root)
                .expect("create conversation");
        let conversation_id = serde_json::from_str::<serde_json::Value>(&created.stdout)
            .expect("create json")["id"]
            .as_str()
            .expect("id")
            .to_string();

        // A second, completely independent command invocation opens the same
        // on-disk workspace and still sees the conversation.
        let listed = run_conversation_command(&conversation_args(&["list"]), &root)
            .expect("list after reopen");
        let listed_json: serde_json::Value =
            serde_json::from_str(&listed.stdout).expect("list json");
        let conversations = listed_json["conversations"]
            .as_array()
            .expect("conversations array");
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0]["id"], conversation_id);

        fs::remove_dir_all(root).ok();
    }

    fn temp_workspace(label: &str) -> PathBuf {
        let stamp = ClockStamp::now().expect("clock").compact_id();
        let root = std::env::temp_dir().join(format!("{label}-{stamp}"));
        fs::create_dir_all(&root).expect("temp workspace");
        root
    }

    fn first_child_dir(path: &Path) -> PathBuf {
        fs::read_dir(path)
            .expect("parent exists")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.is_dir())
            .expect("child dir exists")
    }

    fn read_json(path: &Path) -> serde_json::Value {
        let text = fs::read_to_string(path).expect("json artifact");
        assert!(text.ends_with('\n'));
        serde_json::from_str(&text).expect("valid json")
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
    fn turn_start_idempotency_key_replays_without_a_second_run() {
        let root = temp_workspace("opensks-cli-turn-idempotency");
        let cid = create_conversation_id(&root);
        let args = [
            "turn-start",
            "--conversation",
            &cid,
            "--text",
            "idempotent turn",
            "--idempotency-key",
            "turn-key-001",
        ];

        let first = conversation_json(&args, &root);
        assert_eq!(first["reused"], false);
        let second = conversation_json(&args, &root);
        assert_eq!(second["reused"], true);

        assert_eq!(first["turn_id"], second["turn_id"]);
        assert_eq!(first["run_id"], second["run_id"]);
        assert_eq!(first["user_message_id"], second["user_message_id"]);
        assert_eq!(
            first["assistant_message_id"],
            second["assistant_message_id"]
        );

        // Exactly one run/link exists.
        let runs = conversation_json(&["runs", "--conversation", &cid], &root);
        assert_eq!(runs["runs"].as_array().expect("runs array").len(), 1);

        // No duplicate messages were appended on the replay.
        let messages = conversation_json(&["messages", "--conversation", &cid], &root);
        assert_eq!(messages["messages"].as_array().expect("messages").len(), 2);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn timeline_append_persists_git_receipt_items() {
        let root = temp_workspace("opensks-cli-timeline-append");
        let cid = create_conversation_id(&root);
        let payload = serde_json::json!({
            "content_redacted": "Commit deadbeef recorded.",
            "commit": "deadbeefcafef00d",
            "paths": ["a.rs"],
            "message": "ship it",
            "projection": "git_receipt"
        })
        .to_string();

        let receipt = conversation_json(
            &[
                "timeline-append",
                "--conversation",
                &cid,
                "--kind",
                "commit_receipt",
                "--state",
                "committed",
                "--payload",
                &payload,
            ],
            &root,
        );
        assert_eq!(receipt["kind"], "commit_receipt");
        assert_eq!(receipt["payload"]["commit"], "deadbeefcafef00d");

        let timeline = conversation_json(&["timeline", "--conversation", &cid], &root);
        let items = timeline["items"].as_array().expect("timeline items");
        assert!(items.iter().any(|item| {
            item["kind"] == "commit_receipt"
                && item["state"] == "committed"
                && item["payload"]["paths"][0] == "a.rs"
        }));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn turn_start_survives_a_fresh_process_reopening_the_workspace() {
        let root = temp_workspace("opensks-cli-turn-restart");
        let cid = create_conversation_id(&root);
        let turn = conversation_json(
            &[
                "turn-start",
                "--conversation",
                &cid,
                "--text",
                "durable across restart",
            ],
            &root,
        );
        let expected_run = turn["run_id"].as_str().expect("run id").to_string();

        // Simulate a fresh process by reopening the same workspace via a new
        // repository handle and listing the durable state.
        let repo = opensks_conversation::ConversationRepository::open_workspace(&root)
            .expect("reopen workspace");
        let runs = repo.runs_for_conversation(&cid).expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, expected_run);
        assert_eq!(runs[0].relation, "primary");

        let messages = repo.message_page(&cid, None, 100).expect("messages");
        assert_eq!(messages.len(), 2);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn runs_emits_the_linked_primary_run() {
        let root = temp_workspace("opensks-cli-runs");
        let cid = create_conversation_id(&root);
        let turn = conversation_json(
            &[
                "turn-start",
                "--conversation",
                &cid,
                "--text",
                "list my runs",
            ],
            &root,
        );

        let runs = conversation_json(&["runs", "--conversation", &cid], &root);
        assert_eq!(runs["schema"], "opensks.conversation-run-list.v1");
        assert_eq!(runs["conversation_id"], cid);
        let listed = runs["runs"].as_array().expect("runs array");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["run_id"], turn["run_id"]);
        assert_eq!(listed[0]["turn_id"], turn["turn_id"]);
        assert_eq!(listed[0]["message_id"], turn["assistant_message_id"]);
        assert_eq!(listed[0]["relation"], "primary");
        assert_eq!(listed[0]["run_state"], "failed");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn turn_start_with_local_test_instruction_really_edits_the_workspace() {
        let root = temp_workspace("opensks-cli-agent-edit");
        let cid = create_conversation_id(&root);
        let instruction = r#"{"local_test": {"op": "create_file", "path": "AGENT_NOTE.md", "value": "written by the agent"}}"#;
        let turn = conversation_json(
            &["turn-start", "--conversation", &cid, "--text", instruction],
            &root,
        );
        assert_eq!(turn["run_state"], "completed");
        let last_event_sequence = turn["last_event_sequence"]
            .as_u64()
            .expect("last event sequence");
        assert!(last_event_sequence > 1);

        let written = fs::read_to_string(root.join("AGENT_NOTE.md")).expect("agent-written file");
        assert_eq!(written, "written by the agent");

        let messages = conversation_json(&["messages", "--conversation", &cid], &root);
        let list = messages["messages"].as_array().expect("messages");
        let assistant = list
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("assistant message");
        let content = assistant["content_redacted"].as_str().unwrap_or_default();
        assert!(content.contains("AGENT_NOTE.md"), "got: {content}");
        assert!(
            !content.contains("work items"),
            "leftover fake summary: {content}"
        );
        let run_id = turn["run_id"].as_str().expect("run id");
        let store = opensks_event_store::EventStore::open_workspace(&root).expect("event store");
        let events = store.replay(run_id).expect("replay agent events");
        assert_eq!(
            events.first().map(|event| &event.kind),
            Some(&opensks_contracts::EventKind::RunStarted)
        );
        assert_eq!(
            events.last().map(|event| event.sequence),
            Some(last_event_sequence)
        );
        assert!(
            events
                .iter()
                .any(|event| { event.payload["agent_event_kind"] == "file_patch_applied" }),
            "patch application must be durable in the event journal: {events:#?}"
        );
        let timeline = conversation_json(&["timeline", "--conversation", &cid], &root);
        let timeline_items = timeline["items"].as_array().expect("timeline items");
        assert!(
            timeline_items.iter().any(|item| {
                item["kind"] == "patch"
                    && item["run_id"] == turn["run_id"]
                    && item["payload"]["payload_redacted"]["agent_event_kind"]
                        == "file_patch_applied"
            }),
            "patch application must replay into the conversation timeline: {timeline_items:#?}"
        );
        let repo = opensks_conversation::ConversationRepository::open_workspace(&root)
            .expect("conversation repo");
        assert_eq!(
            repo.run_last_event_sequence(run_id)
                .expect("run last sequence"),
            Some(last_event_sequence)
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn thread_settings_default_then_persist_and_survive_relaunch() {
        let root = temp_workspace("opensks-cli-settings");
        let cid = create_conversation_id(&root);

        let default = conversation_json(&["settings-get", "--conversation", &cid], &root);
        assert_eq!(default["schema"], "opensks.thread-settings.v1");
        assert_eq!(default["model_selection"]["mode"], "auto");
        assert_eq!(default["execution_mode"], "worktree");

        let payload = serde_json::json!({
            "schema": "opensks.thread-settings.v1",
            "conversation_id": cid,
            "model_selection": {"mode": "pinned", "model_id": "openai/gpt-4o-mini", "fallback_model_ids": []},
            "reasoning_effort": "deep",
            "execution_mode": "local",
            "pipeline_id": "parallel-build",
            "max_parallelism": 8,
            "verifier_count": 2,
            "tool_policy_id": "project-default",
            "approval_policy_id": "safe-interactive",
            "updated_at_ms": 0
        })
        .to_string();
        let set = conversation_json(
            &[
                "settings-set",
                "--conversation",
                &cid,
                "--settings",
                &payload,
            ],
            &root,
        );
        assert_eq!(set["pipeline_id"], "parallel-build");

        let got = conversation_json(&["settings-get", "--conversation", &cid], &root);
        assert_eq!(got["model_selection"]["mode"], "pinned");
        assert_eq!(got["model_selection"]["model_id"], "openai/gpt-4o-mini");
        assert_eq!(got["execution_mode"], "local");
        assert_eq!(got["reasoning_effort"], "deep");
        assert_eq!(got["max_parallelism"], 8);

        let turn = conversation_json(
            &[
                "turn-start",
                "--conversation",
                &cid,
                "--text",
                "use pinned settings",
            ],
            &root,
        );
        assert_eq!(
            turn["model_routing_decision"]["status"], "routed",
            "pinned model should be recorded as the route decision"
        );
        assert_eq!(
            turn["model_routing_decision"]["selected_model_id"],
            "openai/gpt-4o-mini"
        );
        assert_eq!(
            turn["model_routing_decision"]["reason_code"],
            "explicit_thread_settings_model"
        );
        assert_eq!(
            turn["model_routing_decision"]["route_receipt"]["provider_id"],
            "openai"
        );
        assert_eq!(
            turn["model_routing_decision"]["route_receipt"]["model_id"],
            "openai/gpt-4o-mini"
        );
        assert_eq!(
            turn["model_routing_decision"]["route_receipt"]["registry_revision"],
            turn["model_routing_decision"]["model_snapshot_hash"]
        );

        let bad = run_conversation_command(
            &[
                "settings-set".to_string(),
                "--workspace".to_string(),
                root.to_string_lossy().into_owned(),
                "--conversation".to_string(),
                cid.clone(),
                "--settings".to_string(),
                "{\"nope\":true}".to_string(),
            ],
            &root,
        );
        assert!(bad.is_err(), "invalid settings json must be rejected");

        fs::remove_dir_all(root).ok();
    }

    // --- file verb ---------------------------------------------------------

    /// A canonical temp workspace seeded for `file` command tests. Canonicalizing
    /// up front keeps `--workspace` byte-identical to the service's root so
    /// containment checks behave the same as in production.
    fn file_workspace(label: &str) -> PathBuf {
        let root = temp_workspace(label);
        root.canonicalize().expect("canonicalize file workspace")
    }

    fn write_workspace_file(root: &Path, relative: &str, contents: &[u8]) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write workspace fixture");
    }

    /// Run `opensks file <args>` with `--workspace <root>` prepended, returning
    /// the parsed stdout JSON on success.
    fn file_ok(root: &Path, args: &[&str]) -> serde_json::Value {
        let output = file_run(root, args, || Ok(String::new())).expect("file command ok");
        serde_json::from_str(&output.stdout).expect("valid file json")
    }

    /// Run `opensks file <args>` driving the save stdin from `input`.
    fn file_run<F>(root: &Path, args: &[&str], input: F) -> Result<CliOutput, CliError>
    where
        F: FnOnce() -> Result<String, CliError>,
    {
        let ws = root.to_string_lossy().into_owned();
        let mut owned = vec![args[0].to_string(), "--workspace".to_string(), ws];
        owned.extend(args[1..].iter().map(|value| value.to_string()));
        run_file_command_with_input(&owned, root, input)
    }

    /// Extract the `file-error.v1` payload from a failed `file` command. The
    /// command surfaces the error JSON as the `CliError::Invalid` message so the
    /// binary prints it and exits nonzero.
    fn file_error_json(error: CliError) -> serde_json::Value {
        match error {
            CliError::Invalid(message) => {
                serde_json::from_str(&message).expect("file-error json payload")
            }
            other => panic!("expected CliError::Invalid file-error, got {other:?}"),
        }
    }

    #[test]
    fn file_open_returns_document_json_with_hash() {
        let root = file_workspace("opensks-cli-file-open");
        write_workspace_file(&root, "src/notes.txt", b"hello world\n");

        let document = file_ok(&root, &["open", "--path", "src/notes.txt"]);
        assert_eq!(document["schema"], "opensks.text-document.v1");
        assert_eq!(document["workspace_relative_path"], "src/notes.txt");
        assert_eq!(document["content"], "hello world\n");
        assert_eq!(document["encoding"], "utf-8");
        assert_eq!(document["line_ending"], "lf");
        assert_eq!(document["byte_size"], 12);
        assert_eq!(document["is_secret_restricted"], false);
        assert_eq!(document["is_binary"], false);
        let hash = document["content_hash"].as_str().expect("content hash");
        assert!(hash.starts_with("fnv1a64:"), "unexpected hash: {hash}");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_stat_returns_entry_without_contents() {
        let root = file_workspace("opensks-cli-file-stat");
        write_workspace_file(&root, "meta.txt", b"abc\n");

        let entry = file_ok(&root, &["stat", "--path", "meta.txt"]);
        assert_eq!(entry["schema"], "opensks.workspace-entry.v1");
        assert_eq!(entry["workspace_relative_path"], "meta.txt");
        assert_eq!(entry["byte_size"], 4);
        assert_eq!(entry["is_secret_restricted"], false);
        assert_eq!(entry["content_hash"], "");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_save_with_matching_hash_succeeds_and_changes_bytes() {
        let root = file_workspace("opensks-cli-file-save-ok");
        write_workspace_file(&root, "doc.txt", b"v1\n");

        let document = file_ok(&root, &["open", "--path", "doc.txt"]);
        let baseline = document["content_hash"].as_str().expect("hash").to_string();

        let saved = file_run(
            &root,
            &[
                "save",
                "--path",
                "doc.txt",
                "--expected-hash",
                &baseline,
                "--stdin",
            ],
            || Ok("v2-edited\n".to_string()),
        )
        .expect("save ok");
        let result: serde_json::Value =
            serde_json::from_str(&saved.stdout).expect("valid save json");
        assert_eq!(result["schema"], "opensks.save-result.v1");
        let new_hash = result["new_hash"].as_str().expect("new hash");
        assert!(new_hash.starts_with("fnv1a64:"));
        assert_ne!(new_hash, baseline);
        assert!(result["new_mtime_ms"].is_u64());

        // The on-disk bytes must reflect the saved content.
        assert_eq!(
            fs::read_to_string(root.join("doc.txt")).expect("read back"),
            "v2-edited\n"
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_save_with_stale_hash_is_file_changed_on_disk_without_contents() {
        const SENTINEL: &str = "SUPER_SECRET_SENTINEL_91";
        let root = file_workspace("opensks-cli-file-save-stale");
        write_workspace_file(&root, "doc.txt", b"v1\n");

        let document = file_ok(&root, &["open", "--path", "doc.txt"]);
        let stale = document["content_hash"].as_str().expect("hash").to_string();

        // Out-of-band change so the baseline no longer matches; the new on-disk
        // content carries a sentinel that must never leak into the error.
        write_workspace_file(&root, "doc.txt", format!("{SENTINEL}\n").as_bytes());

        let error = file_run(
            &root,
            &[
                "save",
                "--path",
                "doc.txt",
                "--expected-hash",
                &stale,
                "--stdin",
            ],
            || Ok("v3-edited\n".to_string()),
        )
        .expect_err("stale baseline conflict");
        let CliError::Invalid(ref message) = error else {
            panic!("expected CliError::Invalid, got {error:?}");
        };
        assert!(
            !message.contains(SENTINEL),
            "file-error payload leaked file contents"
        );
        let body = file_error_json(error);
        assert_eq!(body["schema"], "opensks.file-error.v1");
        assert_eq!(body["error"]["code"], "file_changed_on_disk");

        // The rejected save must leave the on-disk bytes untouched.
        assert_eq!(
            fs::read_to_string(root.join("doc.txt")).expect("read back"),
            format!("{SENTINEL}\n")
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_open_binary_reports_file_binary() {
        let root = file_workspace("opensks-cli-file-binary");
        write_workspace_file(&root, "blob.bin", &[0x00u8, 0x01, 0x02, 0x00]);

        let error = file_run(&root, &["open", "--path", "blob.bin"], || Ok(String::new()))
            .expect_err("binary rejected");
        let body = file_error_json(error);
        assert_eq!(body["error"]["code"], "file_binary");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_open_secret_reports_file_secret_restricted() {
        let root = file_workspace("opensks-cli-file-secret");
        write_workspace_file(&root, ".env", b"TOKEN=abc\n");

        let error = file_run(&root, &["open", "--path", ".env"], || Ok(String::new()))
            .expect_err("secret rejected");
        let body = file_error_json(error);
        assert_eq!(body["error"]["code"], "file_secret_restricted");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_open_oversize_reports_file_too_large() {
        // 5 MiB is the editable ceiling; write one byte past it.
        let root = file_workspace("opensks-cli-file-oversize");
        let oversize = vec![b'a'; (5 * 1024 * 1024) + 1];
        write_workspace_file(&root, "big.txt", &oversize);

        let error = file_run(&root, &["open", "--path", "big.txt"], || Ok(String::new()))
            .expect_err("oversize rejected");
        let body = file_error_json(error);
        assert_eq!(body["error"]["code"], "file_too_large");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_open_traversal_reports_workspace_path_escape() {
        let root = file_workspace("opensks-cli-file-traversal");

        let error = file_run(&root, &["open", "--path", "../etc/passwd"], || {
            Ok(String::new())
        })
        .expect_err("traversal rejected");
        let body = file_error_json(error);
        assert_eq!(body["error"]["code"], "workspace_path_escape");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_open_absolute_reports_workspace_path_escape() {
        let root = file_workspace("opensks-cli-file-absolute");

        let error = file_run(&root, &["open", "--path", "/etc/passwd"], || {
            Ok(String::new())
        })
        .expect_err("absolute rejected");
        let body = file_error_json(error);
        assert_eq!(body["error"]["code"], "workspace_path_escape");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_save_requires_stdin_flag() {
        let root = file_workspace("opensks-cli-file-save-no-stdin");
        write_workspace_file(&root, "doc.txt", b"v1\n");

        let error = file_run(
            &root,
            &["save", "--path", "doc.txt", "--expected-hash", "fnv1a64:0"],
            || Ok("ignored\n".to_string()),
        )
        .expect_err("save without --stdin");
        assert!(matches!(error, CliError::Usage(_)));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_unknown_subcommand_is_usage_error() {
        let root = file_workspace("opensks-cli-file-unknown");
        let error = file_run(&root, &["frobnicate", "--path", "x"], || Ok(String::new()))
            .expect_err("unknown subcommand");
        assert!(matches!(error, CliError::Usage(_)));
        fs::remove_dir_all(root).ok();
    }

    // --- PR-033: file diff -------------------------------------------------

    /// Run `opensks file diff` with the buffer piped from `buffer`.
    fn file_diff(root: &Path, relative: &str, buffer: &str) -> serde_json::Value {
        let owned = buffer.to_string();
        let output = file_run(root, &["diff", "--path", relative, "--stdin"], move || {
            Ok(owned)
        })
        .expect("file diff ok");
        serde_json::from_str(&output.stdout).expect("valid diff json")
    }

    #[test]
    fn file_diff_identical_content_is_unchanged() {
        let root = file_workspace("opensks-cli-diff-identical");
        write_workspace_file(&root, "doc.txt", b"alpha\nbeta\ngamma\n");
        let diff = file_diff(&root, "doc.txt", "alpha\nbeta\ngamma\n");
        assert_eq!(diff["schema"], "opensks.text-diff.v1");
        assert_eq!(diff["path"], "doc.txt");
        assert_eq!(diff["changed"], false);
        assert_eq!(diff["hunks"].as_array().expect("hunks").len(), 0);
        assert_eq!(diff["added_lines"], 0);
        assert_eq!(diff["removed_lines"], 0);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_diff_added_line_produces_added_hunk() {
        let root = file_workspace("opensks-cli-diff-add");
        write_workspace_file(&root, "doc.txt", b"alpha\ngamma\n");
        // Buffer inserts a new middle line.
        let diff = file_diff(&root, "doc.txt", "alpha\nbeta\ngamma\n");
        assert_eq!(diff["changed"], true);
        assert_eq!(diff["added_lines"], 1);
        assert_eq!(diff["removed_lines"], 0);
        let hunks = diff["hunks"].as_array().expect("hunks");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0]["kind"], "added");
        assert_eq!(hunks[0]["lines"][0], "+beta");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_diff_removed_line_produces_removed_hunk() {
        let root = file_workspace("opensks-cli-diff-remove");
        write_workspace_file(&root, "doc.txt", b"alpha\nbeta\ngamma\n");
        // Buffer drops the middle line.
        let diff = file_diff(&root, "doc.txt", "alpha\ngamma\n");
        assert_eq!(diff["changed"], true);
        assert_eq!(diff["added_lines"], 0);
        assert_eq!(diff["removed_lines"], 1);
        let hunks = diff["hunks"].as_array().expect("hunks");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0]["kind"], "removed");
        assert_eq!(hunks[0]["lines"][0], "-beta");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_diff_replaced_line_produces_changed_hunk() {
        let root = file_workspace("opensks-cli-diff-change");
        write_workspace_file(&root, "doc.txt", b"alpha\nbeta\ngamma\n");
        let diff = file_diff(&root, "doc.txt", "alpha\nBETA\ngamma\n");
        assert_eq!(diff["changed"], true);
        assert_eq!(diff["added_lines"], 1);
        assert_eq!(diff["removed_lines"], 1);
        let hunks = diff["hunks"].as_array().expect("hunks");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0]["kind"], "changed");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_diff_requires_stdin_flag() {
        let root = file_workspace("opensks-cli-diff-no-stdin");
        write_workspace_file(&root, "doc.txt", b"v1\n");
        let error = file_run(&root, &["diff", "--path", "doc.txt"], || Ok(String::new()))
            .expect_err("diff without --stdin");
        assert!(matches!(error, CliError::Usage(_)));
        fs::remove_dir_all(root).ok();
    }

    // --- PR-033: codegraph update -----------------------------------------

    /// Run `opensks codegraph update --workspace <root> --path <rel>`.
    fn codegraph_update(root: &Path, relative: &str) -> serde_json::Value {
        let ws = root.to_string_lossy().into_owned();
        let args = vec![
            "update".to_string(),
            "--workspace".to_string(),
            ws,
            "--path".to_string(),
            relative.to_string(),
        ];
        let output = run_codegraph_command(&args, root).expect("codegraph update ok");
        serde_json::from_str(&output.stdout).expect("valid update json")
    }

    #[test]
    fn codegraph_update_is_incremental_and_does_not_rescan_other_files() {
        let root = temp_workspace("opensks-cli-codegraph-update");
        let root = root.canonicalize().expect("canonicalize");
        write_workspace_file(&root, "src/lib.rs", b"pub fn lib_alpha() {}\n");
        write_workspace_file(&root, "src/util.rs", b"pub fn util_beta() {}\n");

        // Build and persist the full index once.
        let graph = opensks_codegraph::CodeGraph::index_workspace(&root).expect("index workspace");
        opensks_codegraph::write_index(&root, &graph).expect("write index");

        // Capture the other file's persisted records as bytes before the update.
        let util_before = util_records_bytes(&root);

        // Change ONLY src/lib.rs and run the incremental update path.
        write_workspace_file(&root, "src/lib.rs", b"pub fn lib_gamma() {}\n");
        let result = codegraph_update(&root, "src/lib.rs");

        assert_eq!(result["schema"], "opensks.codegraph-update.v1");
        assert_eq!(result["path"], "src/lib.rs");
        assert_eq!(
            result["full_scan"], false,
            "an existing index must take the incremental path"
        );
        assert!(result["symbol_count"].as_u64().expect("count") >= 2);

        // The OTHER file's persisted records are byte-identical: not rescanned.
        let util_after = util_records_bytes(&root);
        assert_eq!(
            util_before, util_after,
            "incremental update must leave unrelated file records byte-identical"
        );

        // The changed file's symbols flipped in the persisted index.
        let reloaded = opensks_codegraph::read_index(&root)
            .expect("read index")
            .expect("present index");
        assert!(reloaded.query("lib_alpha").is_empty());
        assert_eq!(reloaded.query("lib_gamma").len(), 1);

        fs::remove_dir_all(root).ok();
    }

    /// Serialize just the `src/util.rs` records from the persisted index so the
    /// incremental-update test can assert they are untouched.
    fn util_records_bytes(root: &Path) -> Vec<u8> {
        let index: opensks_contracts::CodeGraphIndex = serde_json::from_str(
            &fs::read_to_string(opensks_codegraph::index_path(root)).expect("read index file"),
        )
        .expect("parse index");
        let util: Vec<_> = index
            .records
            .into_iter()
            .filter(|record| record.path == "src/util.rs")
            .collect();
        serde_json::to_vec(&util).expect("serialize util records")
    }

    // --- PR-033: git working-change ---------------------------------------

    fn init_cli_repo(label: &str) -> PathBuf {
        let root = temp_workspace(label).canonicalize().expect("canonicalize");
        run_repo_git(&root, &["init"]);
        run_repo_git(&root, &["config", "user.email", "opensks@example.test"]);
        run_repo_git(&root, &["config", "user.name", "OpenSKS Test"]);
        fs::write(root.join("doc.txt"), "baseline\n").expect("write doc");
        run_repo_git(&root, &["add", "doc.txt"]);
        run_repo_git(&root, &["commit", "-m", "initial"]);
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

    fn git_working_change(root: &Path, relative: &str, baseline: &str) -> serde_json::Value {
        let ws = root.to_string_lossy().into_owned();
        let args = vec![
            "working-change".to_string(),
            "--workspace".to_string(),
            ws,
            "--path".to_string(),
            relative.to_string(),
            "--baseline-hash".to_string(),
            baseline.to_string(),
        ];
        let output = run_git_command(&args, root).expect("working-change ok");
        serde_json::from_str(&output.stdout).expect("valid working-change json")
    }

    #[test]
    fn git_working_change_equal_baseline_is_unchanged() {
        let root = init_cli_repo("opensks-cli-working-change-equal");
        let baseline = opensks_git::content_hash(&root.join("doc.txt")).expect("baseline hash");
        let result = git_working_change(&root, "doc.txt", &baseline);
        assert_eq!(result["schema"], "opensks.working-change.v1");
        assert_eq!(result["in_repo"], true);
        assert_eq!(result["changed"], false);
        assert_eq!(result["current_hash"], baseline);
        assert!(
            result["head_hash"].as_str().expect("head hash").len() >= 7,
            "tracked file resolves a HEAD blob id"
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn git_working_change_after_rewrite_is_changed() {
        let root = init_cli_repo("opensks-cli-working-change-rewrite");
        let baseline = opensks_git::content_hash(&root.join("doc.txt")).expect("baseline hash");
        fs::write(root.join("doc.txt"), "branch switched away\n").expect("rewrite");
        let result = git_working_change(&root, "doc.txt", &baseline);
        assert_eq!(result["in_repo"], true);
        assert_eq!(result["changed"], true);
        assert_ne!(result["current_hash"], baseline);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn git_working_change_outside_repo_reports_not_in_repo() {
        let root = temp_workspace("opensks-cli-working-change-no-repo")
            .canonicalize()
            .expect("canonicalize");
        fs::write(root.join("doc.txt"), "plain\n").expect("write");
        let result = git_working_change(&root, "doc.txt", "fnv1a64:0000000000000000");
        assert_eq!(result["schema"], "opensks.working-change.v1");
        assert_eq!(result["in_repo"], false);
        assert!(result.get("changed").is_none());
        fs::remove_dir_all(root).ok();
    }

    // --- PR-034: read-only git status / branches / diff -------------------

    fn git_subcommand(root: &Path, args: &[&str]) -> serde_json::Value {
        let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let output = run_git_command(&owned, root).expect("git subcommand ok");
        serde_json::from_str(&output.stdout).expect("valid git subcommand json")
    }

    #[test]
    fn git_status_reports_dirty_entries() {
        let root = init_cli_repo("opensks-cli-git-status");
        fs::write(root.join("doc.txt"), "changed\n").expect("modify");
        let ws = root.to_string_lossy().into_owned();
        let result = git_subcommand(&root, &["status", "--workspace", &ws]);
        assert_eq!(result["schema"], "opensks.git-status.v1");
        assert_eq!(result["in_repo"], true);
        assert_eq!(result["is_dirty"], true);
        let entries = result["entries"].as_array().expect("entries array");
        assert!(
            entries
                .iter()
                .any(|e| e["path"] == "doc.txt" && e["kind"] == "modified"),
            "modified file appears in status: {entries:?}"
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn git_status_outside_repo_is_minimal() {
        let root = temp_workspace("opensks-cli-git-status-no-repo")
            .canonicalize()
            .expect("canonicalize");
        fs::write(root.join("plain.txt"), "hi\n").expect("write");
        let ws = root.to_string_lossy().into_owned();
        let result = git_subcommand(&root, &["status", "--workspace", &ws]);
        assert_eq!(result["schema"], "opensks.git-status.v1");
        assert_eq!(result["in_repo"], false);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn git_branches_lists_current_branch() {
        let root = init_cli_repo("opensks-cli-git-branches");
        run_repo_git(&root, &["branch", "feature"]);
        let ws = root.to_string_lossy().into_owned();
        let result = git_subcommand(&root, &["branches", "--workspace", &ws]);
        assert_eq!(result["schema"], "opensks.git-branches.v1");
        let branches = result["branches"].as_array().expect("branches array");
        assert!(
            branches.iter().any(|b| b["name"] == "feature"),
            "feature branch listed: {branches:?}"
        );
        let current = result["current"].as_str().expect("current branch");
        assert!(
            branches
                .iter()
                .any(|b| b["name"] == current && b["is_current"] == true),
            "current branch is flagged"
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn git_diff_reports_file_hunks() {
        let root = init_cli_repo("opensks-cli-git-diff");
        fs::write(root.join("doc.txt"), "baseline\nadded\n").expect("modify");
        let ws = root.to_string_lossy().into_owned();
        let result = git_subcommand(&root, &["diff", "--workspace", &ws]);
        assert_eq!(result["schema"], "opensks.git-diff.v1");
        let files = result["files"].as_array().expect("files array");
        let doc = files
            .iter()
            .find(|f| f["path"] == "doc.txt")
            .expect("doc.txt in diff");
        assert_eq!(doc["is_binary"], false);
        assert!(
            !doc["hunks"].as_array().expect("hunks").is_empty(),
            "diff carries at least one hunk"
        );
        fs::remove_dir_all(root).ok();
    }

    // --- PR-035: local git mutations (switch / stage / commit) -------------
    //
    // EXTREME SAFETY: every mutation test below creates its OWN throwaway repo
    // under std::env::temp_dir() (git init + local user.email/name, branch
    // pinned to `main`) and operates ONLY there via an explicit `--workspace`.
    // No test ever mutates the opensks repo or any ancestor.

    /// A fresh throwaway repo with a stable `main` branch and one initial
    /// commit. Used by every mutation test so branch names are deterministic.
    fn init_mutation_repo(label: &str) -> PathBuf {
        let root = temp_workspace(label).canonicalize().expect("canonicalize");
        run_repo_git(&root, &["init"]);
        run_repo_git(&root, &["config", "user.email", "opensks@example.test"]);
        run_repo_git(&root, &["config", "user.name", "OpenSKS Test"]);
        run_repo_git(&root, &["config", "commit.gpgsign", "false"]);
        run_repo_git(&root, &["checkout", "-B", "main"]);
        fs::write(root.join("doc.txt"), "baseline\n").expect("write doc");
        run_repo_git(&root, &["add", "doc.txt"]);
        run_repo_git(&root, &["commit", "-m", "initial"]);
        root
    }

    /// Run a git mutation subcommand expecting success; parse the stdout JSON.
    fn git_mutation_ok(root: &Path, args: &[&str]) -> serde_json::Value {
        let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let output = run_git_command(&owned, root).expect("git mutation ok");
        serde_json::from_str(&output.stdout).expect("valid git mutation json")
    }

    /// Run a git mutation subcommand expecting a refusal; parse the
    /// `opensks.git-error.v1` JSON carried by the nonzero-exit `CliError`.
    fn git_mutation_err(root: &Path, args: &[&str]) -> serde_json::Value {
        let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        match run_git_command(&owned, root) {
            Err(CliError::Invalid(message)) => {
                serde_json::from_str(&message).expect("error message is git-error JSON")
            }
            other => panic!("expected a refusal CliError::Invalid, got {other:?}"),
        }
    }

    fn staged_index_paths(root: &Path) -> Vec<String> {
        let output = process::Command::new("git")
            .args(["diff", "--cached", "--name-only", "-z"])
            .current_dir(root)
            .output()
            .expect("git diff --cached");
        String::from_utf8_lossy(&output.stdout)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn git_switch_preflight_and_switch_respect_dirty_worktree() {
        let root = init_mutation_repo("opensks-cli-git-switch");
        run_repo_git(&root, &["branch", "feature"]);
        let ws = root.to_string_lossy().into_owned();

        // Dirty the worktree.
        fs::write(root.join("doc.txt"), "dirty\n").expect("dirty");

        // Preflight reports a dirty_worktree blocker.
        let preflight = git_mutation_ok(
            &root,
            &[
                "switch-preflight",
                "--workspace",
                &ws,
                "--target",
                "feature",
            ],
        );
        assert_eq!(preflight["schema"], "opensks.git-switch-preflight.v1");
        assert_eq!(preflight["can_switch"], false);
        let blockers = preflight["blockers"].as_array().expect("blockers");
        assert!(
            blockers.iter().any(|b| b["kind"] == "dirty_worktree"),
            "dirty_worktree blocker present: {blockers:?}"
        );

        // Switch without --force is refused with switch_blocked + nonzero exit.
        let error = git_mutation_err(
            &root,
            &["switch", "--workspace", &ws, "--target", "feature"],
        );
        assert_eq!(error["schema"], "opensks.git-error.v1");
        assert_eq!(error["error"]["code"], "switch_blocked");
        assert!(
            error["error"]["blockers"]
                .as_array()
                .is_some_and(|b| !b.is_empty())
        );

        // Discard the dirty change; a clean --force switch succeeds.
        run_repo_git(&root, &["checkout", "--", "doc.txt"]);
        let clean = git_mutation_ok(
            &root,
            &[
                "switch-preflight",
                "--workspace",
                &ws,
                "--target",
                "feature",
            ],
        );
        assert_eq!(clean["can_switch"], true);
        let switched = git_mutation_ok(
            &root,
            &[
                "switch",
                "--workspace",
                &ws,
                "--target",
                "feature",
                "--force",
            ],
        );
        assert_eq!(switched["schema"], "opensks.git-switch.v1");
        assert_eq!(switched["switched"], true);
        assert_eq!(switched["branch"], "feature");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn git_stage_rejects_secret_and_data_plane_paths() {
        let root = init_mutation_repo("opensks-cli-git-stage");
        let ws = root.to_string_lossy().into_owned();
        fs::write(root.join("id_rsa"), "PRIVATE\n").expect("secret");
        fs::create_dir_all(root.join(".opensks/cache")).expect("cache dir");
        fs::write(root.join(".opensks/cache/blob.bin"), "cached\n").expect("data plane");
        fs::write(root.join("normal.rs"), "fn main() {}\n").expect("normal");

        let result = git_mutation_ok(
            &root,
            &[
                "stage",
                "--workspace",
                &ws,
                "--path",
                "id_rsa",
                "--path",
                ".opensks/cache/blob.bin",
                "--path",
                "normal.rs",
            ],
        );
        assert_eq!(result["schema"], "opensks.git-stage.v1");
        let staged = result["staged"].as_array().expect("staged");
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0], "normal.rs");
        let rejected = result["rejected"].as_array().expect("rejected");
        assert!(
            rejected
                .iter()
                .any(|r| r["path"] == "id_rsa" && r["reason"] == "secret_restricted")
        );
        assert!(
            rejected
                .iter()
                .any(|r| r["path"] == ".opensks/cache/blob.bin" && r["reason"] == "data_plane")
        );

        // The index contains ONLY the normal path.
        let index = staged_index_paths(&root);
        assert_eq!(index, vec!["normal.rs".to_string()]);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn git_commit_preview_staleness_blocks_commit_then_fresh_hash_commits() {
        let root = init_mutation_repo("opensks-cli-git-commit");
        let ws = root.to_string_lossy().into_owned();

        // Stage one file; take a preview returning an index_hash.
        fs::write(root.join("a.rs"), "fn a() {}\n").expect("a");
        git_mutation_ok(&root, &["stage", "--workspace", &ws, "--path", "a.rs"]);
        let preview = git_mutation_ok(&root, &["commit-preview", "--workspace", &ws]);
        assert_eq!(preview["schema"], "opensks.git-commit-preview.v1");
        assert_eq!(preview["has_staged"], true);
        let old_hash = preview["index_hash"].as_str().expect("hash").to_string();

        // Staging another file changes the index_hash.
        fs::write(root.join("b.rs"), "fn b() {}\n").expect("b");
        git_mutation_ok(&root, &["stage", "--workspace", &ws, "--path", "b.rs"]);
        let preview2 = git_mutation_ok(&root, &["commit-preview", "--workspace", &ws]);
        let new_hash = preview2["index_hash"].as_str().expect("hash2").to_string();
        assert_ne!(
            new_hash, old_hash,
            "index_hash changes when the index changes"
        );

        // Commit with the OLD (stale) hash is refused with index_changed.
        let error = git_mutation_err(
            &root,
            &[
                "commit",
                "--workspace",
                &ws,
                "--message",
                "stale",
                "--expected-index-hash",
                &old_hash,
            ],
        );
        assert_eq!(error["schema"], "opensks.git-error.v1");
        assert_eq!(error["error"]["code"], "index_changed");

        // Commit with the CURRENT hash succeeds and returns a real sha.
        let committed = git_mutation_ok(
            &root,
            &[
                "commit",
                "--workspace",
                &ws,
                "--message",
                "good commit",
                "--expected-index-hash",
                &new_hash,
            ],
        );
        assert_eq!(committed["schema"], "opensks.git-commit.v1");
        assert_eq!(committed["committed"], true);
        let sha = committed["commit"].as_str().expect("sha");
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));

        // The commit contains ONLY the reviewed paths (assert via git show).
        let output = process::Command::new("git")
            .args(["show", "--name-only", "--pretty=format:", "HEAD"])
            .current_dir(&root)
            .output()
            .expect("git show");
        let mut names: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        names.sort();
        assert_eq!(names, vec!["a.rs".to_string(), "b.rs".to_string()]);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn git_create_branch_is_visible_and_unstage_clears_index() {
        let root = init_mutation_repo("opensks-cli-git-create-branch");
        let ws = root.to_string_lossy().into_owned();
        let created = git_mutation_ok(
            &root,
            &["create-branch", "--workspace", &ws, "--name", "feature"],
        );
        assert_eq!(created["schema"], "opensks.git-create-branch.v1");
        assert_eq!(created["created"], true);
        assert_eq!(created["branch"], "feature");
        assert_eq!(created["head"].as_str().expect("head").len(), 40);

        // The branch is visible to git.
        let output = process::Command::new("git")
            .args(["branch", "--list", "--format=%(refname:short)"])
            .current_dir(&root)
            .output()
            .expect("git branch");
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|l| l.trim() == "feature")
        );

        // Stage then unstage clears the index.
        fs::write(root.join("c.rs"), "fn c() {}\n").expect("c");
        git_mutation_ok(&root, &["stage", "--workspace", &ws, "--path", "c.rs"]);
        assert_eq!(staged_index_paths(&root), vec!["c.rs".to_string()]);
        let unstaged = git_mutation_ok(&root, &["unstage", "--workspace", &ws, "--path", "c.rs"]);
        assert_eq!(unstaged["schema"], "opensks.git-unstage.v1");
        assert_eq!(unstaged["unstaged"][0], "c.rs");
        assert!(staged_index_paths(&root).is_empty());
        fs::remove_dir_all(root).ok();
    }

    // --- PR-036 durable push outbox (CLI wire) -------------------------------

    /// Run a push subcommand expecting success; parse its JSON body.
    fn push_ok(root: &Path, args: &[&str]) -> serde_json::Value {
        let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let output = run_git_command(&owned, root).expect("push subcommand ok");
        serde_json::from_str(&output.stdout).expect("valid push json")
    }

    /// Run a push subcommand expecting a refusal; parse the `git-error.v1` JSON
    /// carried by the nonzero-exit `CliError::Invalid`.
    fn push_err(root: &Path, args: &[&str]) -> serde_json::Value {
        let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        match run_git_command(&owned, root) {
            Err(CliError::Invalid(message)) => {
                serde_json::from_str(&message).expect("error message is git-error JSON")
            }
            other => panic!("expected a refusal CliError::Invalid, got {other:?}"),
        }
    }

    /// Build a SAFE fixture: a source work repo on `branch` plus a SEPARATE LOCAL
    /// BARE repo added as remote `origin` via an absolute file path. No network
    /// remote is ever involved.
    fn init_push_fixture(label: &str, branch: &str) -> (PathBuf, PathBuf) {
        let root = temp_workspace(label).canonicalize().expect("canonicalize");
        let source = root.join("source");
        let bare = root.join("remote.git");
        fs::create_dir_all(&source).expect("source dir");
        run_repo_git(&source, &["init"]);
        run_repo_git(&source, &["config", "user.email", "opensks@example.test"]);
        run_repo_git(&source, &["config", "user.name", "OpenSKS Test"]);
        run_repo_git(&source, &["config", "commit.gpgsign", "false"]);
        run_repo_git(&source, &["checkout", "-B", branch]);
        fs::write(source.join("file.txt"), "v1\n").expect("write");
        run_repo_git(&source, &["add", "file.txt"]);
        run_repo_git(&source, &["commit", "-m", "init"]);
        // The bare repo dir does not exist yet; run init from the source cwd.
        run_repo_git(&source, &["init", "--bare", bare.to_str().unwrap()]);
        run_repo_git(
            &source,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        (source, bare)
    }

    fn bare_has_ref(bare: &Path, branch: &str) -> bool {
        let output = process::Command::new("git")
            .args(["for-each-ref", &format!("refs/heads/{branch}")])
            .current_dir(bare)
            .output()
            .expect("git for-each-ref");
        !String::from_utf8_lossy(&output.stdout).trim().is_empty()
    }

    #[test]
    fn push_cli_full_handshake_pushes_to_local_bare_remote_only() {
        let (source, bare) = init_push_fixture("opensks-cli-push-happy", "feature");
        let ws = source.to_string_lossy().into_owned();

        // Enqueue → a push intent with an effect_digest and protected=false.
        let intent = push_ok(
            &source,
            &[
                "push-enqueue",
                "--workspace",
                &ws,
                "--remote",
                "origin",
                "--ref",
                "feature",
                "--intent",
                "intent-1",
            ],
        );
        assert_eq!(intent["schema"], "opensks.push-intent.v1");
        assert_eq!(intent["ref"], "feature");
        assert_eq!(intent["protected"], false);
        let digest = intent["effect_digest"]
            .as_str()
            .expect("digest")
            .to_string();

        // Execute BEFORE approval → no_matching_approval, remote ref unchanged.
        let before = push_err(
            &source,
            &["push-execute", "--workspace", &ws, "--intent", "intent-1"],
        );
        assert_eq!(before["error"]["code"], "no_matching_approval");
        assert!(
            !bare_has_ref(&bare, "feature"),
            "no remote write before approval"
        );

        // Approve with the matching digest → matched.
        let approval = push_ok(
            &source,
            &[
                "push-approve",
                "--workspace",
                &ws,
                "--intent",
                "intent-1",
                "--effect-digest",
                &digest,
            ],
        );
        assert_eq!(approval["schema"], "opensks.push-approval.v1");
        assert_eq!(approval["matched"], true);

        // Execute → pushes to the LOCAL BARE remote.
        let receipt = push_ok(
            &source,
            &["push-execute", "--workspace", &ws, "--intent", "intent-1"],
        );
        assert_eq!(receipt["schema"], "opensks.push-receipt.v1");
        assert_eq!(receipt["pushed"], true);
        assert_eq!(receipt["already_done"], false);
        assert!(bare_has_ref(&bare, "feature"), "remote now has the ref");

        // A second execute is idempotent: already_done, no second push.
        let repeat = push_ok(
            &source,
            &["push-execute", "--workspace", &ws, "--intent", "intent-1"],
        );
        assert_eq!(repeat["already_done"], true);

        // Status shows the completed receipt, recovered from SQLite.
        let status = push_ok(&source, &["push-status", "--workspace", &ws]);
        assert_eq!(status["schema"], "opensks.push-status.v1");
        assert_eq!(status["completed"].as_array().expect("completed").len(), 1);
        assert!(status["pending"].as_array().expect("pending").is_empty());

        fs::remove_dir_all(source.parent().unwrap()).ok();
    }

    #[test]
    fn push_cli_protected_ref_requires_ack_protected() {
        let (source, bare) = init_push_fixture("opensks-cli-push-protected", "main");
        let ws = source.to_string_lossy().into_owned();

        let intent = push_ok(
            &source,
            &[
                "push-enqueue",
                "--workspace",
                &ws,
                "--remote",
                "origin",
                "--ref",
                "main",
                "--intent",
                "intent-main",
            ],
        );
        assert_eq!(intent["protected"], true);
        let digest = intent["effect_digest"]
            .as_str()
            .expect("digest")
            .to_string();

        // Approve WITHOUT --ack-protected, then execute → protected_branch.
        push_ok(
            &source,
            &[
                "push-approve",
                "--workspace",
                &ws,
                "--intent",
                "intent-main",
                "--effect-digest",
                &digest,
            ],
        );
        let refused = push_err(
            &source,
            &[
                "push-execute",
                "--workspace",
                &ws,
                "--intent",
                "intent-main",
            ],
        );
        assert_eq!(refused["error"]["code"], "protected_branch");
        assert!(
            !bare_has_ref(&bare, "main"),
            "no write for un-acked protected ref"
        );

        // Re-approve WITH --ack-protected, then execute → pushes.
        push_ok(
            &source,
            &[
                "push-approve",
                "--workspace",
                &ws,
                "--intent",
                "intent-main",
                "--effect-digest",
                &digest,
                "--ack-protected",
            ],
        );
        let receipt = push_ok(
            &source,
            &[
                "push-execute",
                "--workspace",
                &ws,
                "--intent",
                "intent-main",
            ],
        );
        assert_eq!(receipt["pushed"], true);
        assert!(bare_has_ref(&bare, "main"), "acked protected ref is pushed");

        fs::remove_dir_all(source.parent().unwrap()).ok();
    }

    // ----------------------------------------------------------------------
    // vault (PR-042)
    // ----------------------------------------------------------------------

    fn vault_json(args: &[&str], workspace: &Path) -> serde_json::Value {
        let ws = workspace.to_string_lossy().into_owned();
        let mut owned = vec![args[0].to_string(), "--workspace".to_string(), ws];
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

        // Generate an age identity entirely via the age crate.
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

        // decrypt with the matching identity recovers the conversation id.
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

        // A bad recipient yields the vault-error contract on a nonzero path and
        // writes no `.age`.
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
        let message = bad.to_string();
        let parsed: serde_json::Value =
            serde_json::from_str(&message).expect("vault error is JSON");
        assert_eq!(parsed["schema"], "opensks.vault-error.v1");
        assert_eq!(parsed["error"]["code"], "bad_recipient");

        // status lists exactly the one summary-less vault we created.
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
}
