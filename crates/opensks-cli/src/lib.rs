use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use thiserror::Error;

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
        other => Err(CliError::Usage(format!(
            "unknown codegraph subcommand `{other}`\n\n{}",
            codegraph_usage()
        ))),
    }
}

pub fn codegraph_usage() -> &'static str {
    concat!(
        "usage: opensks codegraph index\n",
        "       opensks codegraph query <text>\n"
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
    "usage: opensks git outbox\n"
}

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
    let proof = opensks_retention::release_proof("0.1.0", false, false, true, true, false);
    let dir = cwd.join(".opensks").join("release");
    fs::create_dir_all(&dir)?;
    write_text_atomic(
        &dir.join("release-proof.json"),
        &(serde_json::to_string_pretty(&proof)
            .map_err(|error| CliError::Invalid(format!("serialize release proof: {error}")))?
            + "\n"),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote release hardening proof\nstatus: {:?}\nsigned_app: {}\nartifact: {}\n",
            proof.status,
            proof.signed_app,
            dir.join("release-proof.json").display()
        ),
    })
}

pub fn release_usage() -> &'static str {
    "usage: opensks release proof\n"
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

#[derive(Debug, Default)]
struct ConversationCommandOptions {
    workspace: Option<PathBuf>,
    conversation: Option<String>,
    title: Option<String>,
    filter: Option<String>,
    limit: Option<usize>,
    before_sequence: Option<i64>,
    after_sequence: Option<i64>,
    role: Option<String>,
    text: Option<String>,
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
        other => Err(CliError::Usage(format!(
            "unknown conversation subcommand `{other}`\n\n{}",
            conversation_usage()
        ))),
    }
}

pub fn conversation_usage() -> &'static str {
    concat!(
        "usage: opensks conversation list --workspace <path> [--filter all|running|pinned|archived] [--limit N]\n",
        "       opensks conversation create --workspace <path> --title \"<title>\"\n",
        "       opensks conversation rename --workspace <path> --conversation <id> --title \"<title>\"\n",
        "       opensks conversation pin|unpin|archive|unarchive --workspace <path> --conversation <id>\n",
        "       opensks conversation delete --workspace <path> --conversation <id>\n",
        "       opensks conversation fork --workspace <path> --conversation <id> [--after-sequence S]\n",
        "       opensks conversation messages --workspace <path> --conversation <id> [--before-sequence S] [--limit N]\n",
        "       opensks conversation append --workspace <path> --conversation <id> --role user|assistant|system --text \"<text>\"\n"
    )
}

fn parse_conversation_options(args: &[String]) -> Result<ConversationCommandOptions, CliError> {
    let mut options = ConversationCommandOptions::default();
    let mut idx = 0;
    while idx < args.len() {
        let flag = args[idx].as_str();
        match flag {
            "--workspace" => {
                options.workspace = Some(PathBuf::from(conversation_flag_value(args, idx, flag)?));
                idx += 2;
            }
            "--conversation" => {
                options.conversation = Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--title" => {
                options.title = Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--filter" => {
                options.filter = Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--limit" => {
                options.limit = Some(conversation_parse_usize(args, idx, flag)?);
                idx += 2;
            }
            "--before-sequence" => {
                options.before_sequence = Some(conversation_parse_i64(args, idx, flag)?);
                idx += 2;
            }
            "--after-sequence" => {
                options.after_sequence = Some(conversation_parse_i64(args, idx, flag)?);
                idx += 2;
            }
            "--role" => {
                options.role = Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--text" => {
                options.text = Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown conversation argument `{other}`\n\n{}",
                    conversation_usage()
                )));
            }
        }
    }
    Ok(options)
}

fn conversation_flag_value<'a>(
    args: &'a [String],
    idx: usize,
    flag: &str,
) -> Result<&'a str, CliError> {
    args.get(idx + 1).map(String::as_str).ok_or_else(|| {
        CliError::Usage(format!(
            "conversation flag `{flag}` requires a value\n\n{}",
            conversation_usage()
        ))
    })
}

fn conversation_parse_usize(args: &[String], idx: usize, flag: &str) -> Result<usize, CliError> {
    conversation_flag_value(args, idx, flag)?
        .parse::<usize>()
        .map_err(|_| {
            CliError::Usage(format!(
                "conversation flag `{flag}` expects a non-negative integer\n\n{}",
                conversation_usage()
            ))
        })
}

fn conversation_parse_i64(args: &[String], idx: usize, flag: &str) -> Result<i64, CliError> {
    conversation_flag_value(args, idx, flag)?
        .parse::<i64>()
        .map_err(|_| {
            CliError::Usage(format!(
                "conversation flag `{flag}` expects an integer\n\n{}",
                conversation_usage()
            ))
        })
}

fn require_conversation_field<'a>(value: Option<&'a str>, flag: &str) -> Result<&'a str, CliError> {
    value.ok_or_else(|| {
        CliError::Usage(format!(
            "conversation command requires `{flag}`\n\n{}",
            conversation_usage()
        ))
    })
}

fn parse_conversation_filter(
    value: Option<&str>,
) -> Result<opensks_contracts::ConversationFilter, CliError> {
    match value.unwrap_or("all") {
        "all" => Ok(opensks_contracts::ConversationFilter::All),
        "running" => Ok(opensks_contracts::ConversationFilter::Running),
        "pinned" => Ok(opensks_contracts::ConversationFilter::Pinned),
        "archived" => Ok(opensks_contracts::ConversationFilter::Archived),
        other => Err(CliError::Usage(format!(
            "unknown conversation filter `{other}`\n\n{}",
            conversation_usage()
        ))),
    }
}

fn parse_conversation_role(
    value: Option<&str>,
) -> Result<opensks_contracts::MessageRole, CliError> {
    match require_conversation_field(value, "--role")? {
        "system" => Ok(opensks_contracts::MessageRole::System),
        "user" => Ok(opensks_contracts::MessageRole::User),
        "assistant" => Ok(opensks_contracts::MessageRole::Assistant),
        "tool" => Ok(opensks_contracts::MessageRole::Tool),
        "event" => Ok(opensks_contracts::MessageRole::Event),
        other => Err(CliError::Usage(format!(
            "unknown conversation role `{other}`\n\n{}",
            conversation_usage()
        ))),
    }
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
    let handles = active
        .iter()
        .enumerate()
        .map(|(index, lease)| {
            let lane = lease.lane.clone();
            let worker_id = lease.worker_id.clone();
            let lease_id = lease.lease_id.clone();
            std::thread::spawn(move || {
                let queued_at_ms = origin.elapsed().as_millis();
                let dispatched_at_ms = origin.elapsed().as_millis();
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
}
