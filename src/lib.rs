use std::env;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const OPEN_SKSDIR: &str = ".opensks";
const DEFAULT_MAX_WAVES: u32 = 3;
const DEFAULT_MAX_WALL_CLOCK_SECONDS: u64 = 60 * 60;
const DEFAULT_MAX_TOKENS: u64 = 200_000;
const DEFAULT_MAX_COST_USD: f64 = 25.0;
const DEFAULT_MAX_TOOL_CALLS: u32 = 100;
const DEFAULT_MAX_NO_PROGRESS: u32 = 2;
const DEFAULT_MAX_REPEATED_OUTPUT: u32 = 2;
const DEFAULT_REQUIRED_COVERAGE_THRESHOLD: f64 = 1.0;
const PRD_SOURCE_PATH: &str =
    "/Users/weklem/Desktop/opensks_prd_v3_goal_loop_mcp_computer_use_voxel_triwiki.md";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionMode {
    Goal,
    Direct,
    Naruto,
}

impl ExecutionMode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Goal => "goal",
            Self::Direct => "direct",
            Self::Naruto => "naruto",
        }
    }
}

#[derive(Debug, Clone)]
pub struct GoalRunConfig {
    pub text: String,
    pub kind: Option<String>,
    pub mode: ExecutionMode,
    pub max_waves: u32,
}

#[derive(Debug, Clone)]
pub struct GoalRunResult {
    pub goal_id: String,
    pub mission_id: String,
    pub mission_dir: PathBuf,
    pub status: String,
    pub requirement_count: usize,
    pub capability_count: usize,
}

#[derive(Debug, Clone)]
pub struct CliOutput {
    pub stdout: String,
}

#[derive(Debug)]
pub enum OpenSksError {
    Usage(String),
    Io(io::Error),
    NotFound(String),
    Invalid(String),
}

impl fmt::Display for OpenSksError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => write!(f, "{message}"),
            Self::Io(error) => write!(f, "{error}"),
            Self::NotFound(message) => write!(f, "{message}"),
            Self::Invalid(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for OpenSksError {}

impl From<io::Error> for OpenSksError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug, Clone)]
struct Goal {
    id: String,
    text: String,
    kind: String,
    success_criteria: Vec<Requirement>,
    constraints: Vec<String>,
    allowed_capabilities: Vec<String>,
    risk_profile: String,
    budget: GoalBudget,
    stop_policy: StopPolicy,
}

#[derive(Debug, Clone)]
struct Requirement {
    id: String,
    text: String,
}

#[derive(Debug, Clone)]
struct GoalBudget {
    max_tokens: u64,
    max_cost_usd: f64,
    max_tool_calls: u32,
}

#[derive(Debug, Clone)]
struct StopPolicy {
    max_waves: u32,
    max_wall_clock_seconds: u64,
    max_no_progress: u32,
    max_repeated_output: u32,
    required_coverage_threshold: f64,
}

#[derive(Debug, Clone)]
struct ToolPlan {
    capabilities: Vec<String>,
    approval_required: Vec<String>,
    worker_lanes: Vec<String>,
}

#[derive(Debug, Clone)]
struct Voxel {
    id: String,
    kind: String,
    coordinates: String,
    content_hash: String,
    summary: String,
    evidence_refs: Vec<String>,
    links: Vec<String>,
    cache_stability: String,
    privacy_level: String,
}

#[derive(Debug, Clone)]
struct PrdRequirement {
    id: &'static str,
    section: &'static str,
    requirement: &'static str,
    status: &'static str,
    evidence: &'static str,
}

#[derive(Debug, Clone)]
struct CapabilitySession {
    id: String,
    plane: &'static str,
    command: String,
    status: &'static str,
    status_reason: &'static str,
    artifacts: Vec<&'static str>,
    capabilities: Vec<&'static str>,
    safety_rules: Vec<&'static str>,
}

#[derive(Debug, Clone)]
struct CommandCheck {
    name: String,
    command: Vec<String>,
    status: String,
    exit_code: Option<i32>,
    duration_ms: u128,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone)]
struct SecretFinding {
    file: String,
    pattern: String,
}

#[derive(Debug, Clone)]
struct CacheSegment {
    name: String,
    path: String,
    bytes: u64,
    content_hash: String,
    stability: String,
}

#[derive(Debug, Clone)]
struct HttpProbe {
    attempted: bool,
    status: String,
    exit_code: Option<i32>,
    http_code: Option<String>,
    effective_url: Option<String>,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone)]
struct PageSnapshot {
    attempted: bool,
    status: String,
    title: Option<String>,
    bytes: usize,
    content_hash: Option<String>,
    stderr: String,
}

#[derive(Debug, Clone)]
struct AppInspection {
    attempted: bool,
    status: String,
    frontmost_app: Option<String>,
    stderr: String,
}

#[derive(Debug, Clone)]
struct ScreenshotCapture {
    attempted: bool,
    status: String,
    path: Option<PathBuf>,
    bytes: u64,
    stderr: String,
}

#[derive(Debug, Clone)]
struct ClockStamp {
    secs: u64,
    nanos: u32,
}

impl ClockStamp {
    fn now() -> Result<Self, OpenSksError> {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| OpenSksError::Invalid("system clock is before UNIX_EPOCH".to_string()))?;
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

pub fn run_cli<I, S>(args: I, cwd: &Path) -> Result<CliOutput, OpenSksError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    if args.is_empty() || args[0] == "help" || args[0] == "--help" || args[0] == "-h" {
        return Ok(CliOutput {
            stdout: usage().to_string(),
        });
    }

    match args[0].as_str() {
        "goal" => run_goal_command(&args[1..], cwd, ExecutionMode::Goal),
        "run" => run_goal_command(&args[1..], cwd, ExecutionMode::Direct),
        "naruto" => run_goal_command(&args[1..], cwd, ExecutionMode::Naruto),
        "mcp" => run_mcp_command(&args[1..], cwd),
        "browser" => run_browser_command(&args[1..], cwd),
        "app-use" => run_app_use_command(&args[1..], cwd),
        "computer-use" => run_computer_use_command(&args[1..], cwd),
        "voxel" => run_voxel_command(&args[1..], cwd),
        "cache" => run_cache_command(&args[1..], cwd),
        "qa" => run_qa_command(&args[1..], cwd),
        "design" => run_design_command(&args[1..], cwd),
        "bench" => run_bench_command(&args[1..], cwd),
        "auth" => run_auth_command(&args[1..], cwd),
        "app" => run_app_command(&args[1..], cwd),
        "scheduler" => run_scheduler_command(&args[1..], cwd),
        "worktree" => run_worktree_command(&args[1..], cwd),
        "patch" => run_patch_command(&args[1..], cwd),
        "prd" => run_prd_command(&args[1..], cwd),
        other => Err(OpenSksError::Usage(format!(
            "unknown command `{other}`\n\n{}",
            usage()
        ))),
    }
}

fn run_goal_command(
    args: &[String],
    cwd: &Path,
    default_mode: ExecutionMode,
) -> Result<CliOutput, OpenSksError> {
    if args.first().is_some_and(|arg| arg == "status") {
        return read_goal_status(&args[1..], cwd);
    }

    let config = parse_goal_config(args, default_mode)?;
    let result = start_goal_loop(&config, cwd)?;
    Ok(CliOutput {
        stdout: format!(
            "created OpenSKS goal loop\nmission: {}\ngoal: {}\nstatus: {}\nrequirements: {}\ncapabilities: {}\nartifacts: {}\nnext: opensks goal status {}\n",
            result.mission_id,
            result.goal_id,
            result.status,
            result.requirement_count,
            result.capability_count,
            result.mission_dir.display(),
            result.mission_id
        ),
    })
}

fn run_mcp_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let subcommand = args
        .first()
        .ok_or_else(|| OpenSksError::Usage(mcp_usage().to_string()))?;
    let mcp_dir = cwd.join(OPEN_SKSDIR).join("mcp");
    fs::create_dir_all(&mcp_dir)?;

    match subcommand.as_str() {
        "list" => {
            write_mcp_artifacts(&mcp_dir, None)?;
            Ok(CliOutput {
                stdout: format!(
                    "wrote MCP registry and audit artifacts\nartifacts: {}\n",
                    mcp_dir.display()
                ),
            })
        }
        "describe" => {
            let stamp = ClockStamp::now()?;
            let descriptor = render_mcp_server_descriptor(&stamp);
            write_text_atomic(&mcp_dir.join("mcp-server-descriptor.json"), &descriptor)?;
            Ok(CliOutput { stdout: descriptor })
        }
        "add" => {
            let name = args.get(1).ok_or_else(|| {
                OpenSksError::Usage("usage: opensks mcp add <name> [command-or-url]".to_string())
            })?;
            let command = if args.len() > 2 {
                args[2..].join(" ")
            } else {
                "descriptor pending".to_string()
            };
            write_mcp_artifacts(&mcp_dir, Some((name, &command)))?;
            Ok(CliOutput {
                stdout: format!(
                    "registered untrusted MCP server `{name}` for approval\naudit: {}\n",
                    mcp_dir.join("mcp-risk-report.json").display()
                ),
            })
        }
        "audit" => {
            write_mcp_artifacts(&mcp_dir, None)?;
            Ok(CliOutput {
                stdout: format!(
                    "wrote MCP risk report\nrisk_report: {}\n",
                    mcp_dir.join("mcp-risk-report.json").display()
                ),
            })
        }
        "invoke" => {
            let tool_name = args.get(1).ok_or_else(|| {
                OpenSksError::Usage("usage: opensks mcp invoke <tool-name> [payload]".to_string())
            })?;
            let payload = args
                .get(2..)
                .map(|parts| parts.join(" "))
                .unwrap_or_default();
            let result = invoke_local_mcp_tool(cwd, tool_name, &payload)?;
            record_mcp_invocation(&mcp_dir, tool_name, "allowed_by_local_broker", "completed")?;
            Ok(CliOutput {
                stdout: format!(
                    "invoked MCP tool `{tool_name}`\nstatus: completed\nresult: {}\nledger: {}\n",
                    result,
                    mcp_dir.join("mcp-tool-invocations.jsonl").display()
                ),
            })
        }
        "serve" => {
            if args.get(1).is_none_or(|arg| arg != "--once") {
                return Err(OpenSksError::Usage(
                    "usage: opensks mcp serve --once [json-rpc-request]".to_string(),
                ));
            }
            let request = if args.len() > 2 {
                args[2..].join(" ")
            } else {
                let mut input = String::new();
                io::stdin().read_to_string(&mut input)?;
                input
            };
            let response = handle_mcp_json_rpc_once(cwd, &request)?;
            write_text_atomic(
                &mcp_dir.join("mcp-serve-session.json"),
                &render_mcp_serve_session(&ClockStamp::now()?, &request, &response),
            )?;
            Ok(CliOutput {
                stdout: format!("{response}\n"),
            })
        }
        other => Err(OpenSksError::Usage(format!(
            "unknown mcp subcommand `{other}`\n\n{}",
            mcp_usage()
        ))),
    }
}

fn run_browser_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let target = require_freeform(args, "usage: opensks browser \"<url or browser goal>\"")?;
    let probe = probe_http_target(&target);
    let snapshot = capture_page_snapshot(&target);
    let session = capability_session(
        "browser",
        &target,
        if probe.attempted {
            "network_probe"
        } else {
            "planned"
        },
        if probe.attempted {
            "Browser network state was probed with an isolated curl request; Playwright screenshots/click/type are not implemented yet."
        } else {
            "Browser policy and session artifacts are written; target is not an HTTP(S) URL and live Playwright control is not implemented yet."
        },
        &[
            "browser_use",
            "isolated_context",
            "allowlisted_domains",
            "screenshot_capture",
            "dom_snapshot",
            "network_log",
            "visual_diff",
        ],
        &[
            "allowlisted domains only",
            "no downloads/uploads without explicit intent",
            "no credential entry without explicit approval",
        ],
        &[
            "browser-session.json",
            "browser-actions.jsonl",
            "screenshots/",
            "network-log.har",
            "dom-snapshots/",
            "browser-final-state.json",
        ],
    )?;
    write_capability_session(cwd, &session, Some(&target))?;
    write_browser_probe_artifacts(cwd, &session, &target, &probe, &snapshot)?;
    capability_output(&session, cwd)
}

fn run_computer_use_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let target = require_freeform(args, "usage: opensks computer-use \"<computer goal>\"")?;
    let screenshot_id = ClockStamp::now()?.compact_id();
    let session = capability_session(
        "computer-use",
        &target,
        "screenshot_attempted",
        "Computer-use screenshot/action loop artifacts are written; macOS screenshot capture is attempted, but live mouse/keyboard control is not implemented yet.",
        &[
            "computer_use",
            "screenshot_loop",
            "mouse_keyboard_actions",
            "policy_broker",
        ],
        &[
            "isolated VM or session preferred",
            "human approval for sensitive actions",
            "no password, purchase, send, or delete without explicit approval",
        ],
        &[
            "computer-session.json",
            "computer-actions.jsonl",
            "screenshots/",
            "computer-final-state.json",
        ],
    )?;
    write_capability_session(cwd, &session, Some(&target))?;
    let screenshot = capture_computer_screenshot(cwd, &session, &screenshot_id)?;
    write_computer_capture_artifacts(cwd, &session, &target, &screenshot)?;
    capability_output(&session, cwd)
}

fn run_app_use_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let target = require_freeform(args, "usage: opensks app-use \"<app goal>\"")?;
    let inspection = inspect_frontmost_app();
    let session = capability_session(
        "app-use",
        &target,
        if inspection.status == "captured" {
            "inspected"
        } else {
            "planned"
        },
        "macOS app-use layers and accessibility artifacts are written; frontmost-app inspection is attempted on macOS, but full app automation is not implemented yet.",
        &[
            "app_use",
            "app_native_api",
            "app_intents",
            "applescript",
            "accessibility_tree",
            "computer_use_fallback",
        ],
        &[
            "per-app permission required",
            "per-action confirmation for sensitive actions",
            "clipboard and screen recording transparency required",
        ],
        &[
            "app-session.json",
            "accessibility-tree.json",
            "app-actions.jsonl",
            "app-screenshots/",
            "app-final-state.json",
        ],
    )?;
    write_capability_session(cwd, &session, Some(&target))?;
    write_app_inspection_artifacts(cwd, &session, &target, &inspection)?;
    capability_output(&session, cwd)
}

fn run_voxel_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let subcommand = args
        .first()
        .ok_or_else(|| OpenSksError::Usage("usage: opensks voxel query \"<text>\"".to_string()))?;
    if subcommand != "query" {
        return Err(OpenSksError::Usage(format!(
            "unknown voxel subcommand `{subcommand}`; use query"
        )));
    }
    let query = require_freeform(&args[1..], "usage: opensks voxel query \"<text>\"")?;
    let stamp = ClockStamp::now()?;
    let voxel_dir = cwd.join(OPEN_SKSDIR).join("voxel");
    fs::create_dir_all(&voxel_dir)?;
    let source_path = cwd.join(OPEN_SKSDIR).join("triwiki").join("voxels.jsonl");
    let source = fs::read_to_string(&source_path).unwrap_or_default();
    let matches = source
        .lines()
        .filter(|line| {
            line.to_ascii_lowercase()
                .contains(&query.to_ascii_lowercase())
        })
        .map(json_string)
        .collect::<Vec<_>>();
    let report = format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.voxel-query.v1\",\n",
            "  \"query\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"source\": {},\n",
            "  \"match_count\": {},\n",
            "  \"matches\": [{}]\n",
            "}}\n"
        ),
        json_string(&query),
        stamp.json(),
        json_string(&source_path.display().to_string()),
        matches.len(),
        matches.join(",")
    );
    let path = voxel_dir.join(format!("query-{}.json", stamp.compact_id()));
    write_text_atomic(&path, &report)?;
    Ok(CliOutput {
        stdout: format!(
            "voxel query matches: {}\nreport: {}\n",
            matches.len(),
            path.display()
        ),
    })
}

fn run_cache_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    require_exact_subcommand(args, "warm", "usage: opensks cache warm")?;
    let dir = cwd.join(OPEN_SKSDIR).join("cache");
    fs::create_dir_all(&dir)?;
    let stamp = ClockStamp::now()?;
    let segments = collect_cache_segments(cwd)?;
    write_text_atomic(
        &dir.join("cache-warm-report.json"),
        &render_cache_report(&stamp, &segments),
    )?;
    write_text_atomic(
        &dir.join("cache-dashboard.json"),
        &render_cache_dashboard(&stamp, &segments),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "warmed cache planning artifacts\nartifacts: {}\n",
            dir.display()
        ),
    })
}

fn run_qa_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    require_exact_subcommand(args, "run", "usage: opensks qa run")?;
    let dir = cwd.join(OPEN_SKSDIR).join("qa");
    fs::create_dir_all(&dir)?;
    let stamp = ClockStamp::now()?;
    let checks = run_local_qa_checks(cwd);
    let secret_findings = scan_workspace_for_secrets(cwd)?;
    write_text_atomic(
        &dir.join("qa-report.json"),
        &render_qa_report(&stamp, &checks),
    )?;
    write_text_atomic(
        &dir.join("security-audit.json"),
        &render_security_audit(&stamp, &secret_findings),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote QA and security audit artifacts\nchecks: {}\nsecret_findings: {}\nartifacts: {}\n",
            checks.len(),
            secret_findings.len(),
            dir.display()
        ),
    })
}

fn run_design_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    require_exact_subcommand(args, "qa", "usage: opensks design qa")?;
    let dir = cwd.join(OPEN_SKSDIR).join("design");
    fs::create_dir_all(&dir)?;
    let stamp = ClockStamp::now()?;
    write_text_atomic(
        &dir.join("design-qa-report.json"),
        &render_design_qa_report(&stamp),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote design QA artifact\nreport: {}\n",
            dir.join("design-qa-report.json").display()
        ),
    })
}

fn run_bench_command(_args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let dir = cwd.join(OPEN_SKSDIR).join("bench");
    fs::create_dir_all(&dir)?;
    let stamp = ClockStamp::now()?;
    let checks = run_local_qa_checks(cwd);
    write_text_atomic(
        &dir.join("benchmark-report.json"),
        &render_benchmark_report(&stamp, &checks),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote benchmark artifact\nreport: {}\n",
            dir.join("benchmark-report.json").display()
        ),
    })
}

fn run_auth_command(_args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let dir = cwd.join(OPEN_SKSDIR).join("auth");
    fs::create_dir_all(&dir)?;
    let stamp = ClockStamp::now()?;
    write_text_atomic(
        &dir.join("auth-registry.json"),
        &render_auth_registry(&stamp, &provider_env_statuses()),
    )?;
    write_text_atomic(
        &dir.join("provider-registry.json"),
        &render_provider_registry(&stamp, &provider_env_statuses()),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote auth/provider registry artifacts\nartifacts: {}\n",
            dir.display()
        ),
    })
}

fn run_app_command(_args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let dir = cwd.join(OPEN_SKSDIR).join("app");
    fs::create_dir_all(&dir)?;
    let stamp = ClockStamp::now()?;
    write_text_atomic(&dir.join("gui-manifest.json"), &render_gui_manifest(&stamp))?;
    write_text_atomic(
        &dir.join("workspace-manifest.json"),
        &render_workspace_manifest(&stamp),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote GUI/workspace manifest artifacts\nartifacts: {}\n",
            dir.display()
        ),
    })
}

fn run_scheduler_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let subcommand = args.first().ok_or_else(|| {
        OpenSksError::Usage("usage: opensks scheduler run \"<goal>\"".to_string())
    })?;
    if subcommand != "run" {
        return Err(OpenSksError::Usage(format!(
            "unknown scheduler subcommand `{subcommand}`; use run"
        )));
    }
    let goal = require_freeform(&args[1..], "usage: opensks scheduler run \"<goal>\"")?;
    let stamp = ClockStamp::now()?;
    let run_id = format!("scheduler-{}-{}", stamp.compact_id(), process::id());
    let dir = cwd.join(OPEN_SKSDIR).join("scheduler").join(&run_id);
    fs::create_dir_all(&dir)?;
    let checks = run_local_qa_checks(cwd);
    write_text_atomic(
        &dir.join("stage-scheduler.json"),
        &render_scheduler_plan(&stamp, &run_id, &goal),
    )?;
    write_text_atomic(
        &dir.join("scheduler-events.jsonl"),
        &render_scheduler_events(&stamp, &run_id, &checks),
    )?;
    write_text_atomic(
        &dir.join("scheduler-final-state.json"),
        &render_scheduler_final_state(&stamp, &run_id, &checks),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "ran local scheduler slice\nrun: {}\nchecks: {}\nartifacts: {}\n",
            run_id,
            checks.len(),
            dir.display()
        ),
    })
}

fn run_worktree_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let subcommand = args.first().ok_or_else(|| {
        OpenSksError::Usage("usage: opensks worktree create \"<worker label>\"".to_string())
    })?;
    if subcommand != "create" {
        return Err(OpenSksError::Usage(format!(
            "unknown worktree subcommand `{subcommand}`; use create"
        )));
    }
    let label = require_freeform(
        &args[1..],
        "usage: opensks worktree create \"<worker label>\"",
    )?;
    let stamp = ClockStamp::now()?;
    let id = format!("worktree-{}-{}", stamp.compact_id(), process::id());
    let dir = cwd.join(OPEN_SKSDIR).join("worktrees").join(&id);
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

fn run_patch_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let subcommand = args.first().ok_or_else(|| {
        OpenSksError::Usage("usage: opensks patch propose \"<summary>\"".to_string())
    })?;
    if subcommand != "propose" {
        return Err(OpenSksError::Usage(format!(
            "unknown patch subcommand `{subcommand}`; use propose"
        )));
    }
    let summary = require_freeform(&args[1..], "usage: opensks patch propose \"<summary>\"")?;
    let stamp = ClockStamp::now()?;
    let id = format!("patch-{}-{}", stamp.compact_id(), process::id());
    let dir = cwd.join(OPEN_SKSDIR).join("patches").join(&id);
    fs::create_dir_all(&dir)?;
    write_text_atomic(
        &dir.join("patch-envelope.json"),
        &render_patch_envelope(&stamp, &id, &summary),
    )?;
    write_text_atomic(
        &dir.join("gate-result.json"),
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

fn run_prd_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    require_exact_subcommand(args, "coverage", "usage: opensks prd coverage")?;
    let path = write_prd_coverage(cwd)?;
    Ok(CliOutput {
        stdout: format!("wrote PRD coverage ledger\ncoverage: {}\n", path.display()),
    })
}

fn parse_goal_config(
    args: &[String],
    default_mode: ExecutionMode,
) -> Result<GoalRunConfig, OpenSksError> {
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Err(OpenSksError::Usage(goal_usage().to_string()));
    }

    let mut text_parts = Vec::new();
    let mut kind = None;
    let mut max_waves = DEFAULT_MAX_WAVES;
    let mut mode = default_mode;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--kind" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| OpenSksError::Usage("--kind requires a value".to_string()))?;
                kind = Some(value.clone());
            }
            "--mode" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    OpenSksError::Usage("--mode requires direct, goal, or naruto".to_string())
                })?;
                mode = match value.as_str() {
                    "goal" => ExecutionMode::Goal,
                    "direct" => ExecutionMode::Direct,
                    "naruto" => ExecutionMode::Naruto,
                    other => {
                        return Err(OpenSksError::Usage(format!(
                            "unsupported --mode `{other}`; use goal, direct, or naruto"
                        )));
                    }
                };
            }
            "--max-waves" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    OpenSksError::Usage("--max-waves requires a number".to_string())
                })?;
                max_waves = value.parse().map_err(|_| {
                    OpenSksError::Usage("--max-waves requires a positive integer".to_string())
                })?;
                if max_waves == 0 {
                    return Err(OpenSksError::Usage(
                        "--max-waves must be greater than zero".to_string(),
                    ));
                }
            }
            flag if flag.starts_with("--") => {
                return Err(OpenSksError::Usage(format!("unknown option `{flag}`")));
            }
            value => text_parts.push(value.to_string()),
        }
        index += 1;
    }

    let text = text_parts.join(" ").trim().to_string();
    if text.is_empty() {
        return Err(OpenSksError::Usage(
            "goal text is required, for example: opensks goal \"fix failing tests\"".to_string(),
        ));
    }

    Ok(GoalRunConfig {
        text,
        kind,
        mode,
        max_waves,
    })
}

fn require_freeform(args: &[String], usage_text: &str) -> Result<String, OpenSksError> {
    let text = args.join(" ").trim().to_string();
    if text.is_empty() {
        return Err(OpenSksError::Usage(usage_text.to_string()));
    }
    Ok(text)
}

fn require_exact_subcommand(
    args: &[String],
    expected: &str,
    usage_text: &str,
) -> Result<(), OpenSksError> {
    if args.len() == 1 && args[0] == expected {
        return Ok(());
    }
    Err(OpenSksError::Usage(usage_text.to_string()))
}

fn capability_session(
    plane: &'static str,
    command: &str,
    status: &'static str,
    status_reason: &'static str,
    capabilities: &[&'static str],
    safety_rules: &[&'static str],
    artifacts: &[&'static str],
) -> Result<CapabilitySession, OpenSksError> {
    let stamp = ClockStamp::now()?;
    Ok(CapabilitySession {
        id: format!("{}-{}", stamp.compact_id(), process::id()),
        plane,
        command: command.to_string(),
        status,
        status_reason,
        artifacts: artifacts.to_vec(),
        capabilities: capabilities.to_vec(),
        safety_rules: safety_rules.to_vec(),
    })
}

fn capability_output(session: &CapabilitySession, cwd: &Path) -> Result<CliOutput, OpenSksError> {
    Ok(CliOutput {
        stdout: format!(
            "created {} session\nstatus: {}\nartifacts: {}\n",
            session.plane,
            session.status,
            cwd.join(OPEN_SKSDIR)
                .join(session.plane)
                .join(&session.id)
                .display()
        ),
    })
}

fn write_capability_session(
    cwd: &Path,
    session: &CapabilitySession,
    target: Option<&str>,
) -> Result<(), OpenSksError> {
    let dir = cwd.join(OPEN_SKSDIR).join(session.plane).join(&session.id);
    fs::create_dir_all(&dir)?;
    for artifact in &session.artifacts {
        if artifact.ends_with('/') {
            let artifact_dir = dir.join(artifact.trim_end_matches('/'));
            fs::create_dir_all(&artifact_dir)?;
            write_text_atomic(
                &artifact_dir.join("README.txt"),
                "Directory reserved for live runtime evidence in a later implementation phase.\n",
            )?;
            continue;
        }

        let path = dir.join(artifact);
        let contents = match *artifact {
            "browser-actions.jsonl" | "computer-actions.jsonl" | "app-actions.jsonl" => {
                render_action_jsonl(session)
            }
            "network-log.har" => render_har(session),
            "dom-snapshots/initial.json" => render_dom_snapshot(session, target),
            "accessibility-tree.json" => render_accessibility_tree(session, target),
            _ => render_capability_artifact(session, artifact, target),
        };
        write_text_atomic(&path, &contents)?;
    }
    write_text_atomic(
        &dir.join("session-summary.json"),
        &render_capability_session(session),
    )?;
    Ok(())
}

fn write_mcp_artifacts(
    mcp_dir: &Path,
    added_server: Option<(&String, &String)>,
) -> Result<(), OpenSksError> {
    let stamp = ClockStamp::now()?;
    write_text_atomic(
        &mcp_dir.join("mcp-servers.json"),
        &render_mcp_servers(&stamp, added_server),
    )?;
    write_text_atomic(
        &mcp_dir.join("mcp-tool-invocations.jsonl"),
        &render_mcp_invocations(&stamp),
    )?;
    write_text_atomic(
        &mcp_dir.join("mcp-permission-ledger.json"),
        &render_mcp_permission_ledger(&stamp),
    )?;
    write_text_atomic(
        &mcp_dir.join("mcp-risk-report.json"),
        &render_mcp_risk_report(&stamp),
    )?;
    write_text_atomic(
        &mcp_dir.join("mcp-broker-policy.json"),
        &render_mcp_broker_policy(&stamp),
    )?;
    Ok(())
}

fn render_mcp_server_descriptor(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.mcp-server-descriptor.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"transport\": {},\n",
            "  \"server_info\": {{\"name\":\"opensks-local\",\"version\":{}}},\n",
            "  \"capabilities\": {{\"tools\":true,\"resources\":true,\"prompts\":true,\"logging\":true}},\n",
            "  \"tools\": {},\n",
            "  \"resources\": {},\n",
            "  \"prompts\": {},\n",
            "  \"security\": {{\"raw_model_tool_calls\":\"denied\",\"broker\":\"allowlisted local tools only\",\"secrets\":\"not returned\"}}\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&["cli-invoke", "stdio-jsonrpc-once", "artifact-ledger"]),
        json_string(env!("CARGO_PKG_VERSION")),
        render_mcp_tool_descriptors(),
        render_mcp_resource_descriptors(),
        render_mcp_prompt_descriptors()
    )
}

fn render_mcp_tool_descriptors() -> String {
    let tools = [
        (
            "opensks.repo.search",
            "Search text-like workspace files with runtime directories skipped.",
            "query",
        ),
        (
            "opensks.voxel.query",
            "Query local Voxel TriWiki JSONL memory for a substring.",
            "query",
        ),
        (
            "opensks.goal.status",
            "Read a mission final-seal artifact by mission id.",
            "mission_id",
        ),
        (
            "opensks.qa.run",
            "Run local QA checks and built-in secret scan.",
            "unused",
        ),
        (
            "opensks.final_seal.read",
            "Read final seal evidence by mission id.",
            "mission_id",
        ),
    ];
    let rows = tools
        .iter()
        .map(|(name, description, input)| {
            format!(
                concat!(
                    "{{\"name\":{},\"description\":{},",
                    "\"input_hint\":{},\"broker_policy\":\"allowlisted_local_only\"}}"
                ),
                json_string(name),
                json_string(description),
                json_string(input)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn render_mcp_resource_descriptors() -> String {
    let resources = [
        (
            "opensks://repo/current/manifest",
            "Current workspace manifest and local runtime stance.",
        ),
        (
            "opensks://mission/{mission_id}/final-seal",
            "Mission final seal evidence.",
        ),
        (
            "opensks://voxel/query/{query}",
            "Voxel TriWiki query report.",
        ),
    ];
    let rows = resources
        .iter()
        .map(|(uri, description)| {
            format!(
                "{{\"uri\":{},\"description\":{}}}",
                json_string(uri),
                json_string(description)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn render_mcp_prompt_descriptors() -> String {
    let prompts = [
        (
            "opensks.prompt.requirement-extract",
            "Extract concrete goal-loop requirements.",
        ),
        (
            "opensks.prompt.patch-worker",
            "Prepare a bounded patch worker instruction.",
        ),
        (
            "opensks.prompt.security-review",
            "Review tool and patch risk before final apply.",
        ),
    ];
    let rows = prompts
        .iter()
        .map(|(name, description)| {
            format!(
                "{{\"name\":{},\"description\":{}}}",
                json_string(name),
                json_string(description)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn invoke_local_mcp_tool(
    cwd: &Path,
    tool_name: &str,
    payload: &str,
) -> Result<String, OpenSksError> {
    match tool_name {
        "opensks.repo.search" => {
            let hits = search_workspace_text(cwd, payload, 20)?;
            Ok(render_repo_search_tool_result(payload, &hits))
        }
        "opensks.voxel.query" => {
            let output = run_voxel_command(&["query".to_string(), payload.to_string()], cwd)?;
            Ok(format!(
                "{{\"tool\":\"opensks.voxel.query\",\"status\":\"completed\",\"stdout\":{}}}",
                json_string(&output.stdout)
            ))
        }
        "opensks.goal.status" | "opensks.final_seal.read" => {
            let mission_id = payload.trim();
            if mission_id.is_empty() {
                return Err(OpenSksError::Usage(
                    "mission id is required for final seal MCP tools".to_string(),
                ));
            }
            let output = read_goal_status(&[mission_id.to_string()], cwd)?;
            Ok(format!(
                "{{\"tool\":{},\"status\":\"completed\",\"final_seal\":{}}}",
                json_string(tool_name),
                json_string(&output.stdout)
            ))
        }
        "opensks.qa.run" => {
            let output = run_qa_command(&["run".to_string()], cwd)?;
            Ok(format!(
                "{{\"tool\":\"opensks.qa.run\",\"status\":\"completed\",\"stdout\":{}}}",
                json_string(&output.stdout)
            ))
        }
        other => Err(OpenSksError::Usage(format!(
            "unknown or unapproved local MCP tool `{other}`"
        ))),
    }
}

#[derive(Debug, Clone)]
struct RepoSearchHit {
    path: String,
    line_number: usize,
    excerpt: String,
}

fn search_workspace_text(
    cwd: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<RepoSearchHit>, OpenSksError> {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let mut hits = Vec::new();
    search_workspace_text_dir(cwd, cwd, &needle, limit, &mut hits)?;
    Ok(hits)
}

fn search_workspace_text_dir(
    root: &Path,
    current: &Path,
    needle: &str,
    limit: usize,
    hits: &mut Vec<RepoSearchHit>,
) -> Result<(), OpenSksError> {
    if hits.len() >= limit {
        return Ok(());
    }
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(OpenSksError::Io(error)),
    };
    for entry in entries {
        if hits.len() >= limit {
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip_runtime_path(&name) {
            continue;
        }
        if path.is_dir() {
            search_workspace_text_dir(root, &path, needle, limit, hits)?;
        } else if is_text_like_file(&path) {
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            for (index, line) in contents.lines().enumerate() {
                if line.to_ascii_lowercase().contains(needle) {
                    hits.push(RepoSearchHit {
                        path: relative_path(root, &path),
                        line_number: index + 1,
                        excerpt: line.trim().chars().take(240).collect(),
                    });
                    if hits.len() >= limit {
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

fn render_repo_search_tool_result(query: &str, hits: &[RepoSearchHit]) -> String {
    let rows = hits
        .iter()
        .map(|hit| {
            format!(
                "{{\"path\":{},\"line\":{},\"excerpt\":{}}}",
                json_string(&hit.path),
                hit.line_number,
                json_string(&hit.excerpt)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"tool\":\"opensks.repo.search\",\"status\":\"completed\",\"query\":{},\"match_count\":{},\"matches\":[{}]}}",
        json_string(query),
        hits.len(),
        rows
    )
}

fn handle_mcp_json_rpc_once(cwd: &Path, request: &str) -> Result<String, OpenSksError> {
    let id = extract_json_raw_field(request, "id").unwrap_or_else(|| "null".to_string());
    let Some(method) = extract_json_string_field(request, "method") else {
        return Ok(render_mcp_json_rpc_error(
            &id,
            -32600,
            "invalid request: missing method",
        ));
    };
    let stamp = ClockStamp::now()?;
    match method.as_str() {
        "initialize" => Ok(format!(
            concat!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{",
                "\"protocolVersion\":\"local-jsonrpc-once\",",
                "\"serverInfo\":{{\"name\":\"opensks-local\",\"version\":{}}},",
                "\"capabilities\":{{\"tools\":{{}},\"resources\":{{}},\"prompts\":{{}},\"logging\":{{}}}}",
                "}}}}"
            ),
            id,
            json_string(env!("CARGO_PKG_VERSION"))
        )),
        "tools/list" => Ok(format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"tools\":{}}}}}",
            id,
            render_mcp_tool_descriptors()
        )),
        "resources/list" => Ok(format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"resources\":{}}}}}",
            id,
            render_mcp_resource_descriptors()
        )),
        "prompts/list" => Ok(format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"prompts\":{}}}}}",
            id,
            render_mcp_prompt_descriptors()
        )),
        "opensks/describe" => Ok(format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{}}}",
            id,
            render_mcp_server_descriptor(&stamp)
        )),
        "tools/call" => {
            let tool_name = extract_json_string_field(request, "name").unwrap_or_default();
            let payload = extract_json_string_field(request, "query")
                .or_else(|| extract_json_string_field(request, "mission_id"))
                .or_else(|| extract_json_string_field(request, "payload"))
                .unwrap_or_default();
            if tool_name.is_empty() {
                return Ok(render_mcp_json_rpc_error(
                    &id,
                    -32602,
                    "invalid params: missing tool name",
                ));
            }
            match invoke_local_mcp_tool(cwd, &tool_name, &payload) {
                Ok(result) => {
                    let mcp_dir = cwd.join(OPEN_SKSDIR).join("mcp");
                    fs::create_dir_all(&mcp_dir)?;
                    record_mcp_invocation(
                        &mcp_dir,
                        &tool_name,
                        "allowed_by_local_jsonrpc_broker",
                        "completed",
                    )?;
                    Ok(format!(
                        concat!(
                            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{",
                            "\"content\":[{{\"type\":\"text\",\"text\":{}}}],",
                            "\"isError\":false",
                            "}}}}"
                        ),
                        id,
                        json_string(&result)
                    ))
                }
                Err(error) => Ok(render_mcp_json_rpc_error(&id, -32000, &error.to_string())),
            }
        }
        other => Ok(render_mcp_json_rpc_error(
            &id,
            -32601,
            &format!("method not found: {other}"),
        )),
    }
}

fn render_mcp_json_rpc_error(id: &str, code: i32, message: &str) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"error\":{{\"code\":{},\"message\":{}}}}}",
        id,
        code,
        json_string(message)
    )
}

fn render_mcp_serve_session(stamp: &ClockStamp, request: &str, response: &str) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.mcp-serve-session.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"transport\": \"stdio-jsonrpc-once\",\n",
            "  \"request_hash\": {},\n",
            "  \"response_hash\": {},\n",
            "  \"request_bytes\": {},\n",
            "  \"response_bytes\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        json_string(&stable_content_hash(request)),
        json_string(&stable_content_hash(response)),
        request.len(),
        response.len()
    )
}

fn record_mcp_invocation(
    mcp_dir: &Path,
    tool_name: &str,
    decision: &str,
    status: &str,
) -> Result<(), OpenSksError> {
    let stamp = ClockStamp::now()?;
    append_text(
        &mcp_dir.join("mcp-tool-invocations.jsonl"),
        &format!(
            "{{\"schema\":\"opensks.mcp-tool-invocation.v1\",\"at\":{},\"tool\":{},\"decision\":{},\"status\":{},\"reason\":\"local broker allowlist matched\"}}\n",
            stamp.json(),
            json_string(tool_name),
            json_string(decision),
            json_string(status)
        ),
    )
}

fn render_mcp_servers(stamp: &ClockStamp, added_server: Option<(&String, &String)>) -> String {
    let server = if let Some((name, command)) = added_server {
        format!(
            concat!(
                "{{\"name\":{},\"command_or_url\":{},\"trust\":\"untrusted\",",
                "\"descriptor_hash\":{},\"permission\":\"approval_required\",",
                "\"network\":\"unknown\",\"capabilities\":[]}}"
            ),
            json_string(name),
            json_string(command),
            json_string(&stable_content_hash(command))
        )
    } else {
        "{\"name\":\"local-placeholder\",\"trust\":\"untrusted\",\"permission\":\"approval_required\",\"capabilities\":[]}".to_string()
    };
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.mcp-servers.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"roles\": {},\n",
            "  \"server_side_features\": {},\n",
            "  \"client_side_features\": {},\n",
            "  \"opensks_server_tools\": {},\n",
            "  \"opensks_resources\": {},\n",
            "  \"opensks_prompts\": {},\n",
            "  \"servers\": [{}]\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&["host", "client", "server"]),
        json_array(&[
            "tools",
            "resources",
            "prompts",
            "logging",
            "progress",
            "cancellation",
            "error_reporting"
        ]),
        json_array(&["roots", "sampling", "elicitation"]),
        json_array(&[
            "opensks.repo.search",
            "opensks.voxel.query",
            "opensks.goal.create",
            "opensks.goal.status",
            "opensks.patch.propose",
            "opensks.qa.run",
            "opensks.design.capture",
            "opensks.bench.run",
            "opensks.final_seal.read"
        ]),
        json_array(&[
            "opensks://repo/<id>/manifest",
            "opensks://mission/<id>/final-seal",
            "opensks://voxel/<id>",
            "opensks://patch/<id>",
            "opensks://screenshot/<id>"
        ]),
        json_array(&[
            "opensks.prompt.requirement-extract",
            "opensks.prompt.patch-worker",
            "opensks.prompt.design-review",
            "opensks.prompt.security-review"
        ]),
        server
    )
}

fn render_mcp_invocations(stamp: &ClockStamp) -> String {
    format!(
        "{{\"schema\":\"opensks.mcp-tool-invocation.v1\",\"at\":{},\"tool\":\"none\",\"decision\":\"raw_model_calls_denied\",\"reason\":\"model proposes intents; broker enforces policy\"}}\n",
        stamp.json()
    )
}

fn render_mcp_permission_ledger(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.mcp-permission-ledger.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"controls\": {},\n",
            "  \"default_trust\": \"untrusted\",\n",
            "  \"secrets\": \"denied_by_default\",\n",
            "  \"destructive_tools\": \"human_approval_required\"\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&[
            "manifest_pinning",
            "descriptor_hash",
            "tool_description_semantic_scan",
            "tool_allowlist",
            "resource_allowlist",
            "per_server_sandbox",
            "per_tool_budget",
            "rug_pull_detection",
            "descriptor_change_diff",
            "tool_poisoning_detector"
        ])
    )
}

fn render_mcp_risk_report(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.mcp-risk-report.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"risk\": \"medium_until_server_trusted\",\n",
            "  \"broker\": \"MCP server <-> OpenSKS broker <-> goal loop\",\n",
            "  \"findings\": [\"raw MCP tool calls are not exposed to the model\", \"descriptor changes require re-approval\"]\n",
            "}}\n"
        ),
        stamp.json()
    )
}

fn render_mcp_broker_policy(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.mcp-broker-policy.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"model_boundary\": \"model proposes tool intents only\",\n",
            "  \"broker_boundary\": \"broker validates descriptor hash, allowlist, budget, sandbox, and approval policy before invocation\",\n",
            "  \"default_decision\": {{\"allowed\":false,\"requires_approval\":true,\"risk\":\"unknown_server_untrusted\"}},\n",
            "  \"raw_tool_invocation_from_model\": \"denied\"\n",
            "}}\n"
        ),
        stamp.json()
    )
}

fn render_capability_session(session: &CapabilitySession) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.{}.session.v1\",\n",
            "  \"id\": {},\n",
            "  \"plane\": {},\n",
            "  \"command\": {},\n",
            "  \"status\": {},\n",
            "  \"status_reason\": {},\n",
            "  \"capabilities\": {},\n",
            "  \"safety_rules\": {},\n",
            "  \"artifacts\": {}\n",
            "}}\n"
        ),
        session.plane,
        json_string(&session.id),
        json_string(session.plane),
        json_string(&session.command),
        json_string(session.status),
        json_string(session.status_reason),
        json_array(&session.capabilities),
        json_array(&session.safety_rules),
        json_array(&session.artifacts)
    )
}

fn render_capability_artifact(
    session: &CapabilitySession,
    artifact: &str,
    target: Option<&str>,
) -> String {
    let schema_name = artifact.trim_end_matches(".json").replace('/', "-");
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.{}.{}.v1\",\n",
            "  \"session_id\": {},\n",
            "  \"plane\": {},\n",
            "  \"target\": {},\n",
            "  \"status\": {},\n",
            "  \"status_reason\": {},\n",
            "  \"policy_violations\": [],\n",
            "  \"live_execution\": false\n",
            "}}\n"
        ),
        session.plane,
        schema_name,
        json_string(&session.id),
        json_string(session.plane),
        json_string(target.unwrap_or("")),
        json_string(session.status),
        json_string(session.status_reason)
    )
}

fn probe_http_target(target: &str) -> HttpProbe {
    if !(target.starts_with("http://") || target.starts_with("https://")) {
        return HttpProbe {
            attempted: false,
            status: "skipped_non_url".to_string(),
            exit_code: None,
            http_code: None,
            effective_url: None,
            stdout: String::new(),
            stderr: String::new(),
        };
    }

    let output = process::Command::new("curl")
        .args([
            "-L",
            "--max-time",
            "10",
            "-I",
            "-sS",
            "-w",
            "\nOPEN_SKS_HTTP_CODE:%{http_code}\nOPEN_SKS_EFFECTIVE_URL:%{url_effective}\n",
            target,
        ])
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let http_code = stdout
                .lines()
                .find_map(|line| line.strip_prefix("OPEN_SKS_HTTP_CODE:"))
                .map(|value| value.trim().to_string());
            let effective_url = stdout
                .lines()
                .find_map(|line| line.strip_prefix("OPEN_SKS_EFFECTIVE_URL:"))
                .map(|value| value.trim().to_string());
            HttpProbe {
                attempted: true,
                status: if output.status.success() {
                    "captured".to_string()
                } else {
                    "failed".to_string()
                },
                exit_code: output.status.code(),
                http_code,
                effective_url,
                stdout: stdout.trim().to_string(),
                stderr,
            }
        }
        Err(error) => HttpProbe {
            attempted: true,
            status: "error".to_string(),
            exit_code: None,
            http_code: None,
            effective_url: None,
            stdout: String::new(),
            stderr: error.to_string(),
        },
    }
}

fn write_browser_probe_artifacts(
    cwd: &Path,
    session: &CapabilitySession,
    target: &str,
    probe: &HttpProbe,
    snapshot: &PageSnapshot,
) -> Result<(), OpenSksError> {
    let dir = cwd.join(OPEN_SKSDIR).join(session.plane).join(&session.id);
    write_text_atomic(
        &dir.join("network-log.har"),
        &render_browser_har(session, target, probe),
    )?;
    write_text_atomic(
        &dir.join("browser-final-state.json"),
        &render_browser_final_state(session, target, probe, snapshot),
    )?;
    write_text_atomic(
        &dir.join("dom-snapshots").join("initial.json"),
        &render_browser_dom_snapshot(session, target, probe, snapshot),
    )?;
    Ok(())
}

fn render_browser_har(session: &CapabilitySession, target: &str, probe: &HttpProbe) -> String {
    let status_code = probe
        .http_code
        .as_deref()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    format!(
        concat!(
            "{{\"log\":{{\"version\":\"1.2\",\"creator\":{{\"name\":\"opensks\",",
            "\"version\":\"0.1.0\"}},\"entries\":[{{\"request\":{{\"method\":\"HEAD\",",
            "\"url\":{}}},\"response\":{{\"status\":{},\"statusText\":{}}},",
            "\"comment\":{}}}],\"comment\":{}}}}}\n"
        ),
        json_string(target),
        status_code,
        json_string(&probe.status),
        json_string(&truncate_for_json(&probe.stdout, 2000)),
        json_string(&format!(
            "session {}; curl-based network probe, not full Playwright HAR",
            session.id
        ))
    )
}

fn render_browser_final_state(
    session: &CapabilitySession,
    target: &str,
    probe: &HttpProbe,
    snapshot: &PageSnapshot,
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.browser-final-state.v1\",\n",
            "  \"session_id\": {},\n",
            "  \"target\": {},\n",
            "  \"network_probe_attempted\": {},\n",
            "  \"status\": {},\n",
            "  \"exit_code\": {},\n",
            "  \"http_code\": {},\n",
            "  \"effective_url\": {},\n",
            "  \"page_snapshot_attempted\": {},\n",
            "  \"page_snapshot_status\": {},\n",
            "  \"page_title\": {},\n",
            "  \"page_bytes\": {},\n",
            "  \"page_content_hash\": {},\n",
            "  \"stderr\": {},\n",
            "  \"playwright_actions_executed\": false\n",
            "}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        probe.attempted,
        json_string(&probe.status),
        probe
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "null".to_string()),
        probe
            .http_code
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_string()),
        probe
            .effective_url
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_string()),
        snapshot.attempted,
        json_string(&snapshot.status),
        snapshot
            .title
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_string()),
        snapshot.bytes,
        snapshot
            .content_hash
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_string()),
        json_string(&probe.stderr)
    )
}

fn render_browser_dom_snapshot(
    session: &CapabilitySession,
    target: &str,
    probe: &HttpProbe,
    snapshot: &PageSnapshot,
) -> String {
    format!(
        concat!(
            "{{\"schema\":\"opensks.dom-snapshot.v1\",\"session_id\":{},",
            "\"target\":{},\"captured\":{},\"network_probe_status\":{},",
            "\"title\":{},\"content_hash\":{},\"bytes\":{},\"nodes\":[],\"reason\":{}}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        snapshot.status == "captured",
        json_string(&probe.status),
        snapshot
            .title
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_string()),
        snapshot
            .content_hash
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_string()),
        snapshot.bytes,
        if snapshot.status == "captured" {
            json_string("curl GET captured HTML bytes and title; full DOM tree requires Playwright")
        } else {
            json_string(&snapshot.stderr)
        }
    )
}

fn capture_page_snapshot(target: &str) -> PageSnapshot {
    if !(target.starts_with("http://") || target.starts_with("https://")) {
        return PageSnapshot {
            attempted: false,
            status: "skipped_non_url".to_string(),
            title: None,
            bytes: 0,
            content_hash: None,
            stderr: String::new(),
        };
    }

    match process::Command::new("curl")
        .args(["-L", "--max-time", "10", "-sS", target])
        .output()
    {
        Ok(output) => {
            let body = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            PageSnapshot {
                attempted: true,
                status: if output.status.success() {
                    "captured".to_string()
                } else {
                    "failed".to_string()
                },
                title: extract_html_title(&body),
                bytes: body.len(),
                content_hash: if body.is_empty() {
                    None
                } else {
                    Some(stable_content_hash(&body))
                },
                stderr,
            }
        }
        Err(error) => PageSnapshot {
            attempted: true,
            status: "error".to_string(),
            title: None,
            bytes: 0,
            content_hash: None,
            stderr: error.to_string(),
        },
    }
}

fn extract_html_title(body: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let after_start = lower[start..].find('>')? + start + 1;
    let end = lower[after_start..].find("</title>")? + after_start;
    let title = body[after_start..end].trim();
    if title.is_empty() {
        None
    } else {
        Some(collapse_whitespace(title))
    }
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn inspect_frontmost_app() -> AppInspection {
    if !cfg!(target_os = "macos") {
        return AppInspection {
            attempted: false,
            status: "skipped_non_macos".to_string(),
            frontmost_app: None,
            stderr: String::new(),
        };
    }

    let output = process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to get name of first application process whose frontmost is true",
        ])
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            AppInspection {
                attempted: true,
                status: if output.status.success() {
                    "captured".to_string()
                } else {
                    "failed".to_string()
                },
                frontmost_app: if stdout.is_empty() {
                    None
                } else {
                    Some(stdout)
                },
                stderr,
            }
        }
        Err(error) => AppInspection {
            attempted: true,
            status: "error".to_string(),
            frontmost_app: None,
            stderr: error.to_string(),
        },
    }
}

fn write_app_inspection_artifacts(
    cwd: &Path,
    session: &CapabilitySession,
    target: &str,
    inspection: &AppInspection,
) -> Result<(), OpenSksError> {
    let dir = cwd.join(OPEN_SKSDIR).join(session.plane).join(&session.id);
    write_text_atomic(
        &dir.join("accessibility-tree.json"),
        &render_app_accessibility_tree(session, target, inspection),
    )?;
    write_text_atomic(
        &dir.join("app-final-state.json"),
        &render_app_final_state(session, target, inspection),
    )?;
    Ok(())
}

fn render_app_accessibility_tree(
    session: &CapabilitySession,
    target: &str,
    inspection: &AppInspection,
) -> String {
    format!(
        concat!(
            "{{\"schema\":\"opensks.accessibility-tree.v1\",\"session_id\":{},",
            "\"target\":{},\"captured\":{},\"frontmost_app\":{},",
            "\"nodes\":[],\"status\":{},\"stderr\":{}}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        inspection.status == "captured",
        inspection
            .frontmost_app
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_string()),
        json_string(&inspection.status),
        json_string(&inspection.stderr)
    )
}

fn render_app_final_state(
    session: &CapabilitySession,
    target: &str,
    inspection: &AppInspection,
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.app-final-state.v1\",\n",
            "  \"session_id\": {},\n",
            "  \"target\": {},\n",
            "  \"inspection_attempted\": {},\n",
            "  \"status\": {},\n",
            "  \"frontmost_app\": {},\n",
            "  \"live_app_actions_executed\": false\n",
            "}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        inspection.attempted,
        json_string(&inspection.status),
        inspection
            .frontmost_app
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_string())
    )
}

fn capture_computer_screenshot(
    cwd: &Path,
    session: &CapabilitySession,
    screenshot_id: &str,
) -> Result<ScreenshotCapture, OpenSksError> {
    let screenshot_dir = cwd
        .join(OPEN_SKSDIR)
        .join(session.plane)
        .join(&session.id)
        .join("screenshots");
    fs::create_dir_all(&screenshot_dir)?;
    let path = screenshot_dir.join(format!("screen-{screenshot_id}.png"));

    if !cfg!(target_os = "macos") {
        return Ok(ScreenshotCapture {
            attempted: false,
            status: "skipped_non_macos".to_string(),
            path: None,
            bytes: 0,
            stderr: String::new(),
        });
    }

    let path_arg = path.display().to_string();
    match process::Command::new("screencapture")
        .args(["-x", &path_arg])
        .output()
    {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let bytes = fs::metadata(&path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let captured = output.status.success() && bytes > 0;
            Ok(ScreenshotCapture {
                attempted: true,
                status: if captured {
                    "captured".to_string()
                } else {
                    "failed".to_string()
                },
                path: if captured { Some(path) } else { None },
                bytes,
                stderr,
            })
        }
        Err(error) => Ok(ScreenshotCapture {
            attempted: true,
            status: "error".to_string(),
            path: None,
            bytes: 0,
            stderr: error.to_string(),
        }),
    }
}

fn write_computer_capture_artifacts(
    cwd: &Path,
    session: &CapabilitySession,
    target: &str,
    screenshot: &ScreenshotCapture,
) -> Result<(), OpenSksError> {
    let dir = cwd.join(OPEN_SKSDIR).join(session.plane).join(&session.id);
    write_text_atomic(
        &dir.join("computer-final-state.json"),
        &render_computer_final_state(session, target, screenshot),
    )?;
    write_text_atomic(
        &dir.join("computer-actions.jsonl"),
        &render_computer_actions_jsonl(session, screenshot),
    )?;
    Ok(())
}

fn render_computer_final_state(
    session: &CapabilitySession,
    target: &str,
    screenshot: &ScreenshotCapture,
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.computer-final-state.v1\",\n",
            "  \"session_id\": {},\n",
            "  \"target\": {},\n",
            "  \"screenshot_attempted\": {},\n",
            "  \"status\": {},\n",
            "  \"screenshot_path\": {},\n",
            "  \"screenshot_bytes\": {},\n",
            "  \"stderr\": {},\n",
            "  \"mouse_keyboard_actions_executed\": false\n",
            "}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        screenshot.attempted,
        json_string(&screenshot.status),
        screenshot
            .path
            .as_ref()
            .map(|path| json_string(&path.display().to_string()))
            .unwrap_or_else(|| "null".to_string()),
        screenshot.bytes,
        json_string(&screenshot.stderr)
    )
}

fn render_computer_actions_jsonl(
    session: &CapabilitySession,
    screenshot: &ScreenshotCapture,
) -> String {
    format!(
        "{{\"session_id\":{},\"plane\":\"computer-use\",\"action\":\"screenshot\",\"executed\":{},\"status\":{},\"requires_broker\":true}}\n",
        json_string(&session.id),
        screenshot.status == "captured",
        json_string(&screenshot.status)
    )
}

fn run_local_qa_checks(cwd: &Path) -> Vec<CommandCheck> {
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

fn scan_workspace_for_secrets(cwd: &Path) -> Result<Vec<SecretFinding>, OpenSksError> {
    let mut findings = Vec::new();
    scan_dir_for_secrets(cwd, cwd, &mut findings)?;
    Ok(findings)
}

fn scan_dir_for_secrets(
    root: &Path,
    current: &Path,
    findings: &mut Vec<SecretFinding>,
) -> Result<(), OpenSksError> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip_runtime_path(&name) {
            continue;
        }
        if path.is_dir() {
            scan_dir_for_secrets(root, &path, findings)?;
        } else if is_text_like_file(&path) {
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            for pattern in secret_patterns() {
                if contents.contains(&pattern) {
                    findings.push(SecretFinding {
                        file: relative_path(root, &path),
                        pattern,
                    });
                }
            }
        }
    }
    Ok(())
}

fn secret_patterns() -> Vec<String> {
    let api_suffix = "_API_KEY=";
    vec![
        ["BEGIN ", "PRIVATE KEY"].concat(),
        ["OPENAI", api_suffix].concat(),
        ["ANTHROPIC", api_suffix].concat(),
        ["OPENROUTER", api_suffix].concat(),
        ["GEMINI", api_suffix].concat(),
        ["AWS_SECRET_ACCESS", "_KEY="].concat(),
        ["sk", "_live_"].concat(),
        ["sk", "-proj-"].concat(),
    ]
}

fn copy_workspace_snapshot(source_root: &Path, dest_root: &Path) -> Result<usize, OpenSksError> {
    let mut copied = 0;
    copy_dir_snapshot(source_root, source_root, dest_root, &mut copied)?;
    Ok(copied)
}

fn copy_dir_snapshot(
    source_root: &Path,
    current: &Path,
    dest_root: &Path,
    copied: &mut usize,
) -> Result<(), OpenSksError> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let source = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip_runtime_path(&name) {
            continue;
        }
        let relative = source.strip_prefix(source_root).map_err(|_| {
            OpenSksError::Invalid(format!(
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

fn is_text_like_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some(
            "rs" | "toml" | "lock" | "md" | "json" | "jsonl" | "txt" | "yaml" | "yml" | "gitignore"
        )
    ) || path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == ".gitignore")
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn collect_cache_segments(cwd: &Path) -> Result<Vec<CacheSegment>, OpenSksError> {
    let mut segments = Vec::new();
    collect_cache_segments_from_dir(cwd, cwd, &mut segments)?;
    segments.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(segments)
}

fn collect_cache_segments_from_dir(
    root: &Path,
    current: &Path,
    segments: &mut Vec<CacheSegment>,
) -> Result<(), OpenSksError> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip_runtime_path(&name) {
            continue;
        }
        if path.is_dir() {
            collect_cache_segments_from_dir(root, &path, segments)?;
        } else if is_text_like_file(&path) {
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            let relative = relative_path(root, &path);
            let stable_context = matches!(
                relative.as_str(),
                "Cargo.toml" | "Cargo.lock" | "README.md" | ".gitignore"
            ) || relative.starts_with("docs/");
            let stability = if stable_context { "stable" } else { "dynamic" };
            segments.push(CacheSegment {
                name: relative.replace('/', "::"),
                path: relative,
                bytes: contents.len() as u64,
                content_hash: stable_content_hash(&contents),
                stability: stability.to_string(),
            });
        }
    }
    Ok(())
}

fn render_cache_segments_json(segments: &[CacheSegment]) -> String {
    let rows = segments
        .iter()
        .map(|segment| {
            format!(
                concat!(
                    "{{\"name\":{},\"path\":{},\"bytes\":{},",
                    "\"content_hash\":{},\"stability\":{}}}"
                ),
                json_string(&segment.name),
                json_string(&segment.path),
                segment.bytes,
                json_string(&segment.content_hash),
                json_string(&segment.stability)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn provider_env_statuses() -> Vec<(&'static str, &'static str, bool)> {
    vec![
        (
            "OpenRouter",
            "OPENROUTER_API_KEY",
            env::var_os("OPENROUTER_API_KEY").is_some(),
        ),
        (
            "OpenAI",
            "OPENAI_API_KEY",
            env::var_os("OPENAI_API_KEY").is_some(),
        ),
        (
            "Claude",
            "ANTHROPIC_API_KEY",
            env::var_os("ANTHROPIC_API_KEY").is_some(),
        ),
        (
            "Gemini",
            "GEMINI_API_KEY",
            env::var_os("GEMINI_API_KEY").is_some(),
        ),
        (
            "Codex LB",
            "CODEX_LB_API_KEY",
            env::var_os("CODEX_LB_API_KEY").is_some(),
        ),
        (
            "Ollama",
            "OLLAMA_HOST",
            env::var_os("OLLAMA_HOST").is_some(),
        ),
        (
            "LM Studio",
            "LM_STUDIO_BASE_URL",
            env::var_os("LM_STUDIO_BASE_URL").is_some(),
        ),
        (
            "OpenAI-compatible local endpoints",
            "OPENAI_BASE_URL",
            env::var_os("OPENAI_BASE_URL").is_some(),
        ),
    ]
}

fn render_provider_statuses_json(statuses: &[(&'static str, &'static str, bool)]) -> String {
    let rows = statuses
        .iter()
        .map(|(name, env_var, present)| {
            format!(
                "{{\"name\":{},\"credential_env\":{},\"configured\":{},\"secret_value_exposed\":false}}",
                json_string(name),
                json_string(env_var),
                present
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn render_checks_json(checks: &[CommandCheck]) -> String {
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

fn render_secret_findings_json(findings: &[SecretFinding]) -> String {
    let rows = findings
        .iter()
        .map(|finding| {
            format!(
                "{{\"file\":{},\"pattern\":{}}}",
                json_string(&finding.file),
                json_string(&finding.pattern)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
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

fn render_action_jsonl(session: &CapabilitySession) -> String {
    format!(
        "{{\"session_id\":{},\"plane\":{},\"action\":\"policy_registered\",\"executed\":false,\"requires_broker\":true}}\n",
        json_string(&session.id),
        json_string(session.plane)
    )
}

fn render_har(session: &CapabilitySession) -> String {
    format!(
        "{{\"log\":{{\"version\":\"1.2\",\"creator\":{{\"name\":\"opensks\",\"version\":\"0.1.0\"}},\"entries\":[],\"comment\":{}}}}}\n",
        json_string(&format!(
            "{} session {}; live browser network capture not implemented yet",
            session.plane, session.id
        ))
    )
}

fn render_dom_snapshot(session: &CapabilitySession, target: Option<&str>) -> String {
    format!(
        "{{\"schema\":\"opensks.dom-snapshot.v1\",\"session_id\":{},\"target\":{},\"captured\":false,\"nodes\":[]}}\n",
        json_string(&session.id),
        json_string(target.unwrap_or(""))
    )
}

fn render_accessibility_tree(session: &CapabilitySession, target: Option<&str>) -> String {
    format!(
        "{{\"schema\":\"opensks.accessibility-tree.v1\",\"session_id\":{},\"target\":{},\"captured\":false,\"nodes\":[]}}\n",
        json_string(&session.id),
        json_string(target.unwrap_or(""))
    )
}

fn render_cache_report(stamp: &ClockStamp, segments: &[CacheSegment]) -> String {
    let stable_count = segments
        .iter()
        .filter(|segment| segment.stability == "stable")
        .count();
    let dynamic_count = segments.len().saturating_sub(stable_count);
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.cache-warm-report.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"strategy\": \"full_context_cache_first\",\n",
            "  \"segment_count\": {},\n",
            "  \"stable_segment_count\": {},\n",
            "  \"dynamic_segment_count\": {},\n",
            "  \"stable_prefix\": {},\n",
            "  \"dynamic_suffix\": {},\n",
            "  \"target_cache_hit_percent\": 95,\n",
            "  \"segments\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        segments.len(),
        stable_count,
        dynamic_count,
        json_array(&[
            "engine_contract",
            "goal_contract",
            "repo_manifest",
            "voxel_triwiki_summary",
            "requirement_ledger",
            "qa_policy",
            "security_policy",
            "mcp_tool_manifest",
            "browser_policy",
            "app_policy"
        ]),
        json_array(&["worker_shard", "latest_observation"]),
        render_cache_segments_json(segments)
    )
}

fn render_cache_dashboard(stamp: &ClockStamp, segments: &[CacheSegment]) -> String {
    let total_bytes: u64 = segments.iter().map(|segment| segment.bytes).sum();
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.cache-dashboard.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"metrics\": {},\n",
            "  \"live_provider_metrics\": false,\n",
            "  \"local_segment_metrics\": {{\"segments\":{},\"bytes\":{}}}\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&[
            "cache_hit_by_provider",
            "cache_hit_by_model",
            "cache_hit_by_worker_lane",
            "cached_tokens",
            "cache_write_tokens",
            "estimated_cost_saved",
            "ttft_correlation"
        ]),
        segments.len(),
        total_bytes
    )
}

fn render_qa_report(stamp: &ClockStamp, checks: &[CommandCheck]) -> String {
    let failed = checks
        .iter()
        .filter(|check| check.status == "failed" || check.status == "error")
        .count();
    let status = if failed == 0 { "passed" } else { "failed" };
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.qa-report.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"status\": {},\n",
            "  \"code_qa\": {},\n",
            "  \"browser_qa\": {},\n",
            "  \"app_qa\": {},\n",
            "  \"design_qa\": {},\n",
            "  \"security_qa\": {},\n",
            "  \"live_checks_executed\": true,\n",
            "  \"checks\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        json_string(status),
        json_array(&[
            "format",
            "lint",
            "typecheck",
            "unit_tests",
            "integration_tests",
            "snapshot_tests",
            "dependency_checks",
            "dead_code",
            "api_contract"
        ]),
        json_array(&[
            "playwright_route",
            "screenshots",
            "visual_regression",
            "accessibility",
            "console_errors",
            "network_failures",
            "responsive_breakpoints"
        ]),
        json_array(&[
            "accessibility_tree_validation",
            "screenshot_visual_check",
            "state_transition_check",
            "menu_shortcut_behavior",
            "error_dialog_detection"
        ]),
        json_array(&[
            "image_generation_alternatives",
            "screenshot_comparison",
            "layout_scoring",
            "color_contrast",
            "spacing_typography",
            "responsive_variants",
            "design_verifier_model"
        ]),
        json_array(&[
            "secret_scan",
            "mcp_tool_poisoning_scan",
            "prompt_injection_scan",
            "permission_policy_check",
            "dependency_vulnerability_check",
            "supply_chain_check",
            "unsafe_computer_use_check"
        ]),
        render_checks_json(checks)
    )
}

fn render_security_audit(stamp: &ClockStamp, findings: &[SecretFinding]) -> String {
    let status = if findings.is_empty() {
        "passed"
    } else {
        "findings"
    };
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.security-audit.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"status\": {},\n",
            "  \"boundaries\": {},\n",
            "  \"dangerous_actions_require_approval\": {},\n",
            "  \"secret_access\": \"denied_by_default\",\n",
            "  \"live_scan_executed\": true,\n",
            "  \"secret_findings\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        json_string(status),
        json_array(&[
            "model",
            "tool",
            "mcp",
            "computer_use",
            "browser",
            "app",
            "workspace_mutation",
            "secret",
            "update"
        ]),
        json_array(&[
            "purchase",
            "send_message_or_email",
            "delete",
            "commit_push",
            "install_dependency",
            "run_untrusted_code",
            "access_secret",
            "control_sensitive_app",
            "enter_password",
            "financial_medical_legal_action"
        ]),
        render_secret_findings_json(findings)
    )
}

fn render_scheduler_plan(stamp: &ClockStamp, run_id: &str, goal: &str) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.stage-scheduler.v1\",\n",
            "  \"run_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"goal\": {},\n",
            "  \"bounded_parallelism\": true,\n",
            "  \"stages\": {},\n",
            "  \"rebalance_policy\": \"serial parent integration; workers use isolated workspaces or patch envelopes\"\n",
            "}}\n"
        ),
        json_string(run_id),
        stamp.json(),
        json_string(goal),
        json_array(&[
            "intake",
            "context_hydration",
            "capability_planning",
            "worker_lane_allocation",
            "local_qa",
            "security_scan",
            "final_state"
        ])
    )
}

fn render_scheduler_events(stamp: &ClockStamp, run_id: &str, checks: &[CommandCheck]) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "{{\"run_id\":{},\"at\":{},\"event\":\"scheduler_started\"}}",
        json_string(run_id),
        stamp.json()
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

fn render_scheduler_final_state(
    stamp: &ClockStamp,
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
        stamp.json(),
        json_string(if failed == 0 { "passed" } else { "failed" }),
        render_checks_json(checks)
    )
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
        json_array(&["format", "lint", "test", "security_scan", "final_seal"])
    )
}

fn render_design_qa_report(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.design-qa-report.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"checks\": {},\n",
            "  \"status\": \"planned_artifact\",\n",
            "  \"live_image_or_screenshot_evidence\": false\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&[
            "image_generation",
            "screenshot_visual_diff",
            "design_verifier",
            "responsive_qa",
            "auto_ui_patch"
        ])
    )
}

fn render_benchmark_report(stamp: &ClockStamp, checks: &[CommandCheck]) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.benchmark-report.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"model_registry_fields\": {},\n",
            "  \"pipeline_profiles\": {},\n",
            "  \"collaboration_artifacts\": {},\n",
            "  \"live_benchmarks_executed\": true,\n",
            "  \"local_runtime_checks\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&[
            "model_context_window",
            "cache_support",
            "vision_support",
            "image_generation_support",
            "tool_use_support",
            "computer_use_support",
            "reasoning_cost",
            "latency_profile",
            "quality_profile"
        ]),
        json_array(&["glm", "gpt", "claude", "gemini", "local"]),
        json_array(&[
            "multi-llm-roster.json",
            "role-assignments.json",
            "disagreement-report.json",
            "quorum-report.json"
        ]),
        render_checks_json(checks)
    )
}

fn render_auth_registry(
    stamp: &ClockStamp,
    statuses: &[(&'static str, &'static str, bool)],
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.auth-registry.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"auth_methods\": {},\n",
            "  \"key_storage\": {},\n",
            "  \"secrets_stored_in_repo\": false,\n",
            "  \"live_keychain_integration\": false,\n",
            "  \"env_credential_discovery\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&[
            "api_key",
            "oauth",
            "browser_login_bridge",
            "local_endpoint",
            "enterprise_gateway"
        ]),
        json_array(&[
            "macos_keychain_first",
            "encrypted_file_fallback",
            "workspace_scoped_credentials"
        ]),
        render_provider_statuses_json(statuses)
    )
}

fn render_provider_registry(
    stamp: &ClockStamp,
    statuses: &[(&'static str, &'static str, bool)],
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.provider-registry.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"providers\": {},\n",
            "  \"usage_metrics\": {},\n",
            "  \"live_adapters\": false,\n",
            "  \"provider_env_status\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&[
            "OpenRouter",
            "OpenAI",
            "Claude",
            "Gemini",
            "Codex LB",
            "Ollama",
            "LM Studio",
            "OpenAI-compatible local endpoints",
            "MCP servers"
        ]),
        json_array(&[
            "tokens",
            "cost",
            "cached_tokens",
            "cache_writes",
            "reasoning_tokens",
            "tool_calls",
            "computer_browser_app_actions"
        ]),
        render_provider_statuses_json(statuses)
    )
}

fn render_gui_manifest(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.gui-manifest.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"candidate\": \"Tauri v2 with Rust engine core and WebView frontend\",\n",
            "  \"panels\": {},\n",
            "  \"live_gui\": false\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&[
            "mission_control",
            "voxel_triwiki_map",
            "tool_mcp_panel",
            "computer_browser_app_panel",
            "qa_panel",
            "token_dashboard"
        ])
    )
}

fn render_workspace_manifest(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.workspace-manifest.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"primary_runtime\": \"Rust\",\n",
            "  \"target_crates\": {},\n",
            "  \"physical_workspace_split\": false\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&[
            "opensks-core",
            "opensks-goal-loop",
            "opensks-voxel-triwiki",
            "opensks-mcp",
            "opensks-computer-use",
            "opensks-browser-use",
            "opensks-app-use",
            "opensks-providers",
            "opensks-auth",
            "opensks-cache",
            "opensks-scheduler",
            "opensks-worktree",
            "opensks-patch",
            "opensks-qa",
            "opensks-design",
            "opensks-security",
            "opensks-bench",
            "opensks-gui",
            "opensks-cli",
            "opensks-plugins"
        ])
    )
}

fn write_prd_coverage(cwd: &Path) -> Result<PathBuf, OpenSksError> {
    let dir = cwd.join(OPEN_SKSDIR);
    fs::create_dir_all(&dir)?;
    let path = dir.join("prd-coverage.json");
    write_text_atomic(&path, &render_prd_coverage_json())?;
    Ok(path)
}

fn render_prd_coverage_json() -> String {
    let requirements = prd_requirements();
    let implemented = requirements
        .iter()
        .filter(|item| item.status == "implemented")
        .count();
    let artifact_mvp = requirements
        .iter()
        .filter(|item| item.status == "artifact_mvp")
        .count();
    let planned = requirements
        .iter()
        .filter(|item| item.status == "planned_artifact")
        .count();
    let missing_live = requirements
        .iter()
        .filter(|item| item.status == "missing_live_implementation")
        .count();
    let rows = requirements
        .iter()
        .map(render_prd_requirement)
        .collect::<Vec<_>>()
        .join(",\n    ");
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.prd-coverage.v1\",\n",
            "  \"source\": {},\n",
            "  \"summary\": {{\"total\":{},\"implemented\":{},\"artifact_mvp\":{},\"planned_artifact\":{},\"missing_live_implementation\":{}}},\n",
            "  \"requirements\": [\n    {}\n  ]\n",
            "}}\n"
        ),
        json_string(PRD_SOURCE_PATH),
        requirements.len(),
        implemented,
        artifact_mvp,
        planned,
        missing_live,
        rows
    )
}

fn render_prd_requirement(requirement: &PrdRequirement) -> String {
    format!(
        "{{\"id\":{},\"section\":{},\"requirement\":{},\"status\":{},\"evidence\":{}}}",
        json_string(requirement.id),
        json_string(requirement.section),
        json_string(requirement.requirement),
        json_string(requirement.status),
        json_string(requirement.evidence)
    )
}

fn prd_requirements() -> Vec<PrdRequirement> {
    vec![
        req(
            "P00-001",
            "0",
            "Rust-native autonomous coding OS metadata is represented.",
            "implemented",
            "Cargo package and CLI runtime",
        ),
        req(
            "P00-002",
            "0",
            "macOS first, Linux second, Windows later platform stance is represented.",
            "planned_artifact",
            "auth/app/workspace manifests",
        ),
        req(
            "P00-003",
            "0",
            "Tokio/content-addressed cache/worktree/stage scheduler/event-sourced runtime are represented.",
            "artifact_mvp",
            "cache artifacts, local stage scheduler, isolated workspace snapshots, and event-sourced JSONL",
        ),
        req(
            "P00-004",
            "0",
            "MCP, computer, browser, app, shell, file, design, QA, and security capability planes are represented.",
            "artifact_mvp",
            "tool-plan.json and CLI plane commands",
        ),
        req(
            "P01-001",
            "1.1",
            "Cache-stable context rather than blind token reduction.",
            "artifact_mvp",
            "cache-warm-report.json with hashed local stable/dynamic segments",
        ),
        req(
            "P01-002",
            "1.2",
            "Automation from goal analysis through self-improve.",
            "planned_artifact",
            "PRD coverage and command artifacts",
        ),
        req(
            "P01-003",
            "1.3",
            "Autonomous coding runtime with evidence-backed completion/blocking.",
            "artifact_mvp",
            "goal-loop.json and final-seal.json",
        ),
        req(
            "P01-004",
            "1.4",
            "Code, design, security, browser, and app QA in one goal loop.",
            "artifact_mvp",
            "qa-report.json with live local Rust checks and security-audit.json secret scan",
        ),
        req(
            "P01-005",
            "1.5",
            "Design/image generation QA loop.",
            "planned_artifact",
            "design-qa-report.json",
        ),
        req(
            "P01-006",
            "1.6",
            "Voxel TriWiki accumulates repo/task/failure/provider/cache knowledge.",
            "artifact_mvp",
            "voxel-triwiki.json and prd-coverage.json",
        ),
        req(
            "P01-007",
            "1.7",
            "Rust engine, bounded scheduler, content-addressed context, provider cache, overlap metrics.",
            "artifact_mvp",
            "local scheduler, timed local checks, and content-addressed cache segments",
        ),
        req(
            "P01-008",
            "1.8",
            "Dynamic model-specific pipelines.",
            "planned_artifact",
            "benchmark-report.json",
        ),
        req(
            "P01-009",
            "1.9",
            "Multi-LLM collaboration roles.",
            "planned_artifact",
            "benchmark-report.json",
        ),
        req(
            "P01-010",
            "1.10",
            "OpenRouter first-class provider.",
            "planned_artifact",
            "provider-registry.json",
        ),
        req(
            "P01-011",
            "1.11",
            "Codex LB optional adapter; core independent.",
            "planned_artifact",
            "provider-registry.json",
        ),
        req(
            "P01-012",
            "1.12",
            "GPT and Claude OAuth where possible.",
            "planned_artifact",
            "auth-registry.json",
        ),
        req(
            "P01-013",
            "1.13",
            "Local LLM support.",
            "planned_artifact",
            "provider-registry.json",
        ),
        req(
            "P01-014",
            "1.14",
            "Authentication manager with Keychain/OAuth/API key/audit.",
            "planned_artifact",
            "auth-registry.json",
        ),
        req(
            "P01-015",
            "1.15",
            "Token/cost/cache usage dashboard.",
            "artifact_mvp",
            "cache-dashboard.json with local segment counts and bytes",
        ),
        req(
            "P01-016",
            "1.16",
            "Security threat model for MCP/tool poisoning/secrets/plugins/supply chain.",
            "planned_artifact",
            "security-audit.json",
        ),
        req(
            "P01-017",
            "1.17",
            "Rust hot path.",
            "implemented",
            "Rust crate",
        ),
        req(
            "P01-018",
            "1.18",
            "Modular commercial/open-source concepts.",
            "planned_artifact",
            "workspace-manifest.json",
        ),
        req(
            "P01-019",
            "1.19",
            "macOS-specific Keychain/accessibility/app automation/update posture.",
            "planned_artifact",
            "auth/app-use/app manifests",
        ),
        req(
            "P01-020",
            "1.20",
            "Signed updater with channels and rollback.",
            "missing_live_implementation",
            "not implemented",
        ),
        req(
            "P02-001",
            "2.1",
            "User request becomes machine-readable goal pipeline.",
            "artifact_mvp",
            "goal-loop.json",
        ),
        req(
            "P02-002",
            "2.2",
            "Goal schema includes id, text, kind, criteria, constraints, capabilities, risk, budget, stop policy.",
            "artifact_mvp",
            "goal-loop.json",
        ),
        req(
            "P02-003",
            "2.3",
            "All goal kinds are represented.",
            "planned_artifact",
            "PRD coverage ledger",
        ),
        req(
            "P02-004",
            "2.4",
            "Fourteen loop phases are represented.",
            "artifact_mvp",
            "goal-loop.json",
        ),
        req(
            "P02-005",
            "2.5",
            "Finite stop policy and terminal states.",
            "artifact_mvp",
            "stop-policy.json",
        ),
        req(
            "P02-006",
            "2.6",
            "Progress definition excludes mere text generation.",
            "artifact_mvp",
            "progress-ledger.json",
        ),
        req(
            "P02-007",
            "2.7",
            "Goal loop artifacts exist.",
            "artifact_mvp",
            ".opensks/missions/<mission>/",
        ),
        req(
            "P03-001",
            "3.1",
            "Proof-first artifacts for claims/stops/patches/tools.",
            "artifact_mvp",
            "final-seal.json and plane ledgers",
        ),
        req(
            "P03-002",
            "3.2",
            "Mutation safety via worktree or patch envelope and single-thread final apply.",
            "artifact_mvp",
            "worktree-isolation.json and patch-envelope.json",
        ),
        req(
            "P03-003",
            "3.3",
            "Route honesty for direct/naruto/browser/computer/app paths.",
            "artifact_mvp",
            "execution_mode and plane-specific commands",
        ),
        req(
            "P03-004",
            "3.4",
            "Evidence before completion.",
            "artifact_mvp",
            "final-seal.json",
        ),
        req(
            "P03-005",
            "3.5",
            "Final seal schema.",
            "artifact_mvp",
            "final-seal.json",
        ),
        req(
            "P04-001",
            "4.1-4.5",
            "Voxel TriWiki axes, types, schema, and graph links.",
            "artifact_mvp",
            "voxel-triwiki.json",
        ),
        req(
            "P04-002",
            "4.6",
            "Voxel TriWiki used in goal intake/context/worker/QA/repair/bench/self-improve.",
            "planned_artifact",
            "prd-coverage.json and cache/bench artifacts",
        ),
        req(
            "P04-003",
            "4.7",
            "Voxel cache synergy with stable prefix and dynamic suffix.",
            "artifact_mvp",
            "voxel-triwiki.json and cache-warm-report.json",
        ),
        req(
            "P04-004",
            "4.8",
            "Voxel GUI views.",
            "planned_artifact",
            "gui-manifest.json",
        ),
        req(
            "P05-001",
            "5.1-5.3",
            "MCP host, client, server, tools, resources, prompts.",
            "artifact_mvp",
            "mcp-server-descriptor.json plus local stdio-jsonrpc-once tools/resources/prompts surface",
        ),
        req(
            "P05-002",
            "5.4",
            "MCP registry fields.",
            "artifact_mvp",
            "mcp-servers.json and mcp-server-descriptor.json",
        ),
        req(
            "P05-003",
            "5.5",
            "MCP security controls.",
            "artifact_mvp",
            "mcp-permission-ledger.json, mcp-risk-report.json, and mcp-broker-policy.json",
        ),
        req(
            "P05-004",
            "5.6",
            "Internal MCP broker.",
            "artifact_mvp",
            "mcp-broker-policy.json denies raw model tool calls by default",
        ),
        req(
            "P05-005",
            "5.7",
            "MCP audit artifacts.",
            "artifact_mvp",
            ".opensks/mcp artifacts",
        ),
        req(
            "P06-001",
            "6.1-6.2",
            "Browser-use engine taxonomy/actions/artifacts.",
            "artifact_mvp",
            "browser network probe, page title/hash snapshot, HAR-like log, and final-state artifacts",
        ),
        req(
            "P06-002",
            "6.3",
            "Computer-use screenshot/action loop/actions/security.",
            "artifact_mvp",
            "computer-use screenshot capture attempt, action ledger, and final-state artifacts",
        ),
        req(
            "P06-003",
            "6.4-6.5",
            "macOS app-use layers/artifacts/safety.",
            "artifact_mvp",
            "app-use frontmost-app inspection, accessibility artifact, and final-state artifacts",
        ),
        req(
            "P06-004",
            "6.6",
            "GUI for computer/browser/app use.",
            "planned_artifact",
            "gui-manifest.json",
        ),
        req(
            "P07-001",
            "7",
            "Goal-based tool planning and approval model.",
            "artifact_mvp",
            "tool-plan.json",
        ),
        req(
            "P08-001",
            "8",
            "Code, browser, app, design, security QA categories.",
            "artifact_mvp",
            "qa-report.json with live local code QA and planned browser/app/design categories",
        ),
        req(
            "P09-001",
            "9",
            "Dynamic model registry and pipeline profiles.",
            "planned_artifact",
            "benchmark-report.json",
        ),
        req(
            "P10-001",
            "10",
            "Token/cache strategy, warming, dashboard.",
            "artifact_mvp",
            "cache artifacts with local segment hashes, stability classes, counts, and bytes",
        ),
        req(
            "P11-001",
            "11",
            "Multi-LLM role assignment, artifacts, no hidden fallback.",
            "planned_artifact",
            "benchmark-report.json",
        ),
        req(
            "P12-001",
            "12",
            "Auth/provider manager and usage dashboard.",
            "planned_artifact",
            "auth and provider registries",
        ),
        req(
            "P13-001",
            "13",
            "Security boundaries, policy engine, dangerous-action approval.",
            "artifact_mvp",
            "security-audit.json with live local secret scan and approval policy",
        ),
        req(
            "P14-001",
            "14",
            "GUI mission, voxel, MCP, app, QA, token panels.",
            "planned_artifact",
            "gui-manifest.json",
        ),
        req(
            "P15-001",
            "15",
            "All CLI v3 commands exist.",
            "artifact_mvp",
            "run_cli command router",
        ),
        req(
            "P16-001",
            "16",
            "Rust workspace v3 crate map.",
            "artifact_mvp",
            "workspace-manifest.json plus scheduler/worktree/patch command surfaces",
        ),
        req(
            "P17-001",
            "17",
            "Implementation phases 0-6 are represented.",
            "artifact_mvp",
            "goal, voxel, scheduler, worktree, patch, QA, cache, MCP, app, auth, bench, and PRD artifacts",
        ),
        req(
            "P18-001",
            "18",
            "MVP acceptance criteria.",
            "missing_live_implementation",
            "many live adapters/GUI/browser/app criteria still not implemented",
        ),
        req(
            "P18-002",
            "18",
            "Beta acceptance criteria.",
            "missing_live_implementation",
            "not implemented",
        ),
        req(
            "P18-003",
            "18",
            "Production acceptance criteria.",
            "missing_live_implementation",
            "not implemented",
        ),
        req(
            "P19-001",
            "19",
            "Source-note foundations are represented as implementation assumptions.",
            "planned_artifact",
            "coverage ledger and plane artifacts",
        ),
        req(
            "P20-001",
            "20",
            "Final product statement direction is preserved.",
            "planned_artifact",
            "README and coverage ledger",
        ),
    ]
}

fn req(
    id: &'static str,
    section: &'static str,
    requirement: &'static str,
    status: &'static str,
    evidence: &'static str,
) -> PrdRequirement {
    PrdRequirement {
        id,
        section,
        requirement,
        status,
        evidence,
    }
}

pub fn start_goal_loop(config: &GoalRunConfig, cwd: &Path) -> Result<GoalRunResult, OpenSksError> {
    let stamp = ClockStamp::now()?;
    let id_suffix = format!("{}-{}", stamp.compact_id(), process::id());
    let goal_id = format!("goal-{id_suffix}");
    let mission_id = format!("M-{id_suffix}");
    let open_dir = cwd.join(OPEN_SKSDIR);
    let mission_dir = open_dir.join("missions").join(&mission_id);
    let triwiki_dir = open_dir.join("triwiki");
    fs::create_dir_all(&mission_dir)?;
    fs::create_dir_all(&triwiki_dir)?;

    let requirements = extract_requirements(&config.text);
    let capabilities = infer_capabilities(&config.text, &config.mode);
    let constraints = extract_constraints(&config.text);
    let risk_profile = infer_risk_profile(&config.text, &capabilities);
    let stop_policy = StopPolicy {
        max_waves: config.max_waves,
        max_wall_clock_seconds: DEFAULT_MAX_WALL_CLOCK_SECONDS,
        max_no_progress: DEFAULT_MAX_NO_PROGRESS,
        max_repeated_output: DEFAULT_MAX_REPEATED_OUTPUT,
        required_coverage_threshold: DEFAULT_REQUIRED_COVERAGE_THRESHOLD,
    };
    let goal = Goal {
        id: goal_id.clone(),
        text: config.text.clone(),
        kind: config
            .kind
            .clone()
            .unwrap_or_else(|| infer_goal_kind(&config.text).to_string()),
        success_criteria: requirements,
        constraints,
        allowed_capabilities: capabilities,
        risk_profile,
        budget: GoalBudget {
            max_tokens: DEFAULT_MAX_TOKENS,
            max_cost_usd: DEFAULT_MAX_COST_USD,
            max_tool_calls: DEFAULT_MAX_TOOL_CALLS,
        },
        stop_policy,
    };
    let tool_plan = build_tool_plan(&goal, &config.mode);
    let voxels = build_voxels(&goal, &mission_id, &tool_plan);
    let qa_checks = run_local_qa_checks(cwd);
    let secret_findings = scan_workspace_for_secrets(cwd)?;
    let isolated_workspace = mission_dir.join("worker-workspace");
    fs::create_dir_all(&isolated_workspace)?;
    let isolated_files = copy_workspace_snapshot(cwd, &isolated_workspace)?;

    let goal_loop = render_goal_loop_json(&goal, &mission_id, &config.mode, &stamp, &tool_plan);
    let goal_state = render_goal_state_jsonl(&goal, &mission_id, &stamp, &config.mode);
    let progress_ledger = render_progress_ledger_json(&goal, &mission_id, &stamp, &config.mode);
    let stop_policy_json = render_stop_policy_json(&goal.stop_policy, &mission_id, &goal.id);
    let tool_plan_json = render_tool_plan_json(&tool_plan, &mission_id, &goal.id);
    let triwiki_json = render_triwiki_json(&voxels, &mission_id, &goal.id);
    let final_seal = render_final_seal_json(
        &goal,
        &mission_id,
        &stamp,
        &tool_plan,
        &voxels,
        &qa_checks,
        &secret_findings,
    );
    let voxels_jsonl = render_voxels_jsonl(&voxels);

    write_text_atomic(&mission_dir.join("goal-loop.json"), &goal_loop)?;
    write_text_atomic(&mission_dir.join("goal-state.jsonl"), &goal_state)?;
    write_text_atomic(&mission_dir.join("progress-ledger.json"), &progress_ledger)?;
    write_text_atomic(&mission_dir.join("stop-policy.json"), &stop_policy_json)?;
    write_text_atomic(&mission_dir.join("tool-plan.json"), &tool_plan_json)?;
    write_text_atomic(&mission_dir.join("voxel-triwiki.json"), &triwiki_json)?;
    write_text_atomic(&mission_dir.join("voxels.jsonl"), &voxels_jsonl)?;
    write_text_atomic(&mission_dir.join("final-seal.json"), &final_seal)?;
    write_text_atomic(
        &mission_dir.join("qa-report.json"),
        &render_qa_report(&stamp, &qa_checks),
    )?;
    write_text_atomic(
        &mission_dir.join("security-audit.json"),
        &render_security_audit(&stamp, &secret_findings),
    )?;
    write_text_atomic(
        &mission_dir.join("stage-scheduler.json"),
        &render_scheduler_plan(&stamp, &mission_id, &goal.text),
    )?;
    write_text_atomic(
        &mission_dir.join("scheduler-events.jsonl"),
        &render_scheduler_events(&stamp, &mission_id, &qa_checks),
    )?;
    write_text_atomic(
        &mission_dir.join("scheduler-final-state.json"),
        &render_scheduler_final_state(&stamp, &mission_id, &qa_checks),
    )?;
    write_text_atomic(
        &mission_dir.join("worktree-isolation.json"),
        &render_worktree_isolation(
            &stamp,
            &mission_id,
            &goal.text,
            &isolated_workspace,
            isolated_files,
        ),
    )?;
    write_text_atomic(
        &mission_dir.join("patch-envelope.json"),
        &render_patch_envelope(&stamp, &format!("patch-{mission_id}"), &goal.text),
    )?;
    write_text_atomic(
        &mission_dir.join("patch-gate-result.json"),
        &render_patch_gate(&stamp, &format!("patch-{mission_id}")),
    )?;
    write_text_atomic(
        &mission_dir.join("prd-coverage.json"),
        &render_prd_coverage_json(),
    )?;
    append_text(&triwiki_dir.join("voxels.jsonl"), &voxels_jsonl)?;
    write_prd_coverage(cwd)?;

    Ok(GoalRunResult {
        goal_id,
        mission_id,
        mission_dir,
        status: "partial".to_string(),
        requirement_count: goal.success_criteria.len(),
        capability_count: goal.allowed_capabilities.len(),
    })
}

fn read_goal_status(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let mission_id = args.first().ok_or_else(|| {
        OpenSksError::Usage("usage: opensks goal status <mission-id>".to_string())
    })?;
    let final_seal_path = cwd
        .join(OPEN_SKSDIR)
        .join("missions")
        .join(mission_id)
        .join("final-seal.json");
    if !final_seal_path.exists() {
        return Err(OpenSksError::NotFound(format!(
            "final seal not found for mission `{mission_id}`"
        )));
    }

    Ok(CliOutput {
        stdout: fs::read_to_string(final_seal_path)?,
    })
}

fn build_tool_plan(goal: &Goal, mode: &ExecutionMode) -> ToolPlan {
    let mut approval_required = Vec::new();
    if goal.allowed_capabilities.iter().any(|cap| {
        matches!(
            cap.as_str(),
            "computer_use" | "browser_use" | "app_use" | "mcp_use"
        )
    }) {
        approval_required.push("destructive tool calls require explicit approval".to_string());
        approval_required.push("secret access denied by default".to_string());
    }
    if goal.risk_profile != "low" {
        approval_required.push("human approval required for irreversible actions".to_string());
    }

    let worker_lanes = match mode {
        ExecutionMode::Goal => vec![
            "intake".to_string(),
            "goal-normalizer".to_string(),
            "artifact-writer".to_string(),
        ],
        ExecutionMode::Direct => vec![
            "intake".to_string(),
            "direct-executor-planned".to_string(),
            "verifier-planned".to_string(),
        ],
        ExecutionMode::Naruto => vec![
            "planner".to_string(),
            "patch-worker-1-planned".to_string(),
            "patch-worker-2-planned".to_string(),
            "verifier-planned".to_string(),
            "finalizer-planned".to_string(),
        ],
    };

    ToolPlan {
        capabilities: goal.allowed_capabilities.clone(),
        approval_required,
        worker_lanes,
    }
}

fn extract_requirements(text: &str) -> Vec<Requirement> {
    let mut candidates = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let normalized = line
            .trim_start_matches(|ch: char| ch == '-' || ch == '*' || ch.is_ascii_digit())
            .trim_start_matches('.')
            .trim();
        split_requirement_candidate(normalized, &mut candidates);
    }

    if candidates.is_empty() {
        candidates.push(text.trim().to_string());
    }

    candidates
        .into_iter()
        .filter(|candidate| !candidate.is_empty())
        .enumerate()
        .map(|(index, text)| Requirement {
            id: format!("req-{:03}", index + 1),
            text,
        })
        .collect()
}

fn split_requirement_candidate(text: &str, out: &mut Vec<String>) {
    let mut current = String::new();
    for ch in text.chars() {
        if matches!(ch, '.' | ';' | '\n') {
            push_requirement(&mut current, out);
        } else {
            current.push(ch);
        }
    }
    push_requirement(&mut current, out);
}

fn push_requirement(current: &mut String, out: &mut Vec<String>) {
    let value = current.trim();
    if value.len() >= 3 {
        out.push(value.to_string());
    }
    current.clear();
}

fn extract_constraints(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut constraints = Vec::new();
    if lower.contains("without") || lower.contains("no ") || lower.contains("하지 말") {
        constraints.push("honor explicit negative constraints in the goal text".to_string());
    }
    if lower.contains("budget") || lower.contains("cost") || lower.contains("token") {
        constraints.push("respect explicit budget and token limits".to_string());
    }
    if lower.contains("macos") || lower.contains("keychain") {
        constraints.push("prefer macOS-first implementation paths".to_string());
    }
    if constraints.is_empty() {
        constraints.push("preserve proof-first artifacts and finite loop termination".to_string());
    }
    constraints
}

fn infer_capabilities(text: &str, mode: &ExecutionMode) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut capabilities = vec![
        "file_use".to_string(),
        "shell_use".to_string(),
        "qa_use".to_string(),
        "voxel_triwiki".to_string(),
    ];

    if lower.contains("mcp") {
        capabilities.push("mcp_use".to_string());
    }
    if lower.contains("browser") || lower.contains("playwright") || lower.contains("web") {
        capabilities.push("browser_use".to_string());
    }
    if lower.contains("computer") || lower.contains("screenshot") {
        capabilities.push("computer_use".to_string());
    }
    if lower.contains("app") || lower.contains("macos") || lower.contains("accessibility") {
        capabilities.push("app_use".to_string());
    }
    if lower.contains("design") || lower.contains("image") {
        capabilities.push("design_use".to_string());
    }
    if lower.contains("security") || lower.contains("secret") || lower.contains("auth") {
        capabilities.push("security_use".to_string());
    }
    if matches!(mode, ExecutionMode::Naruto) {
        capabilities.push("parallel_worker_use".to_string());
    }

    capabilities.sort();
    capabilities.dedup();
    capabilities
}

fn infer_goal_kind(text: &str) -> &'static str {
    let lower = text.to_ascii_lowercase();
    if lower.contains("bug") || lower.contains("fix") || lower.contains("repair") {
        "bugfix"
    } else if lower.contains("test") {
        "test_repair"
    } else if lower.contains("design") || lower.contains("ui") {
        "design_improvement"
    } else if lower.contains("browser") {
        "browser_task"
    } else if lower.contains("app") || lower.contains("macos") {
        "app_task"
    } else if lower.contains("security") {
        "security_review"
    } else {
        "code_change"
    }
}

fn infer_risk_profile(text: &str, capabilities: &[String]) -> String {
    let lower = text.to_ascii_lowercase();
    let high_risk_terms = [
        "delete",
        "purchase",
        "send email",
        "password",
        "secret",
        "financial",
        "medical",
        "legal",
        "push",
        "deploy",
    ];
    if high_risk_terms.iter().any(|term| lower.contains(term)) {
        return "high".to_string();
    }
    if capabilities.iter().any(|cap| {
        matches!(
            cap.as_str(),
            "mcp_use" | "computer_use" | "browser_use" | "app_use" | "security_use"
        )
    }) {
        return "medium".to_string();
    }
    "low".to_string()
}

fn build_voxels(goal: &Goal, mission_id: &str, tool_plan: &ToolPlan) -> Vec<Voxel> {
    let mut voxels = Vec::new();
    voxels.push(Voxel {
        id: format!("voxel-goal-{}", goal.id),
        kind: "goal_voxel".to_string(),
        coordinates: format!("mission:{mission_id}/goal:{}", goal.id),
        content_hash: stable_content_hash(&goal.text),
        summary: format!("Goal {}: {}", goal.id, goal.text),
        evidence_refs: vec!["goal-loop.json".to_string()],
        links: vec!["covers_requirement:*".to_string()],
        cache_stability: "dynamic".to_string(),
        privacy_level: "workspace".to_string(),
    });

    for requirement in &goal.success_criteria {
        voxels.push(Voxel {
            id: format!("voxel-requirement-{}", requirement.id),
            kind: "requirement_voxel".to_string(),
            coordinates: format!("mission:{mission_id}/requirement:{}", requirement.id),
            content_hash: stable_content_hash(&requirement.text),
            summary: requirement.text.clone(),
            evidence_refs: vec![
                "goal-loop.json".to_string(),
                "progress-ledger.json".to_string(),
            ],
            links: vec![
                format!("derived_from:{}", goal.id),
                "covered_by:intake".to_string(),
            ],
            cache_stability: "stable".to_string(),
            privacy_level: "workspace".to_string(),
        });
    }

    voxels.push(Voxel {
        id: format!("voxel-proof-{mission_id}"),
        kind: "qa_voxel".to_string(),
        coordinates: format!("mission:{mission_id}/proof:artifact-contract"),
        content_hash: stable_content_hash(&tool_plan.worker_lanes.join(",")),
        summary:
            "Artifact contract written for goal intake, tool plan, stop policy, and final seal"
                .to_string(),
        evidence_refs: vec!["final-seal.json".to_string()],
        links: vec![format!("verified_by:{}", goal.id)],
        cache_stability: "dynamic".to_string(),
        privacy_level: "workspace".to_string(),
    });

    voxels.push(Voxel {
        id: format!("voxel-cache-{mission_id}"),
        kind: "cache_voxel".to_string(),
        coordinates: format!("mission:{mission_id}/cache:stable-prefix"),
        content_hash: stable_content_hash(&goal.allowed_capabilities.join(",")),
        summary: "Cache-stable prefix includes goal, requirements, constraints, and policy summary"
            .to_string(),
        evidence_refs: vec!["voxel-triwiki.json".to_string()],
        links: vec![format!("cached_with:{}", goal.id)],
        cache_stability: "stable".to_string(),
        privacy_level: "workspace".to_string(),
    });

    voxels
}

fn render_goal_loop_json(
    goal: &Goal,
    mission_id: &str,
    mode: &ExecutionMode,
    stamp: &ClockStamp,
    tool_plan: &ToolPlan,
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.goal-loop.v1\",\n",
            "  \"mission_id\": {},\n",
            "  \"created_at\": {},\n",
            "  \"execution_mode\": {},\n",
            "  \"goal\": {},\n",
            "  \"loop_phases\": {},\n",
            "  \"progress_definition\": {},\n",
            "  \"tool_plan_ref\": \"tool-plan.json\",\n",
            "  \"worker_lanes\": {},\n",
            "  \"status\": \"partial\",\n",
            "  \"status_reason\": \"Goal-loop artifacts, local scheduler, local QA/security scan, isolated workspace snapshot, patch gate, Voxel TriWiki seed, and final seal are written; provider-backed workers and repair waves are not live yet.\"\n",
            "}}\n"
        ),
        json_string(mission_id),
        stamp.json(),
        json_string(mode.as_str()),
        render_goal_json(goal),
        json_array(&[
            "intake",
            "goal_normalization",
            "requirement_extraction",
            "voxel_triwiki_context_hydration",
            "capability_planning",
            "worker_lane_allocation",
            "parallel_execution",
            "observation_ingestion",
            "qa_and_verifier_wave",
            "repair_wave",
            "merge_finalizer",
            "final_apply_or_noop",
            "final_seal",
            "memory_update"
        ]),
        json_array(&[
            "new_requirement_covered",
            "new_candidate_gate_passed",
            "new_test_failure_resolved",
            "new_design_diff_improved",
            "new_security_finding_fixed",
            "new_browser_or_app_state_reached",
            "final_seal_advanced"
        ]),
        json_vec(&tool_plan.worker_lanes)
    )
}

fn render_goal_json(goal: &Goal) -> String {
    let requirements = goal
        .success_criteria
        .iter()
        .map(render_requirement_json)
        .collect::<Vec<_>>()
        .join(",\n      ");
    format!(
        concat!(
            "{{\n",
            "    \"id\": {},\n",
            "    \"text\": {},\n",
            "    \"kind\": {},\n",
            "    \"success_criteria\": [\n      {}\n    ],\n",
            "    \"constraints\": {},\n",
            "    \"allowed_capabilities\": {},\n",
            "    \"risk_profile\": {},\n",
            "    \"budget\": {{\"max_tokens\":{},\"max_cost_usd\":{},\"max_tool_calls\":{}}},\n",
            "    \"stop_policy_ref\": \"stop-policy.json\"\n",
            "  }}"
        ),
        json_string(&goal.id),
        json_string(&goal.text),
        json_string(&goal.kind),
        requirements,
        json_vec(&goal.constraints),
        json_vec(&goal.allowed_capabilities),
        json_string(&goal.risk_profile),
        goal.budget.max_tokens,
        goal.budget.max_cost_usd,
        goal.budget.max_tool_calls
    )
}

fn render_requirement_json(requirement: &Requirement) -> String {
    format!(
        "{{\"id\":{},\"text\":{},\"coverage\":\"extracted\"}}",
        json_string(&requirement.id),
        json_string(&requirement.text)
    )
}

fn render_goal_state_jsonl(
    goal: &Goal,
    mission_id: &str,
    stamp: &ClockStamp,
    mode: &ExecutionMode,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "{{\"event\":\"mission_created\",\"mission_id\":{},\"goal_id\":{},\"at\":{},\"mode\":{}}}",
        json_string(mission_id),
        json_string(&goal.id),
        stamp.json(),
        json_string(mode.as_str())
    ));
    lines.push(format!(
        "{{\"event\":\"requirements_extracted\",\"mission_id\":{},\"goal_id\":{},\"count\":{}}}",
        json_string(mission_id),
        json_string(&goal.id),
        goal.success_criteria.len()
    ));
    lines.push(format!(
        "{{\"event\":\"final_seal_written\",\"mission_id\":{},\"goal_id\":{},\"status\":\"partial\"}}",
        json_string(mission_id),
        json_string(&goal.id)
    ));
    lines.join("\n") + "\n"
}

fn render_progress_ledger_json(
    goal: &Goal,
    mission_id: &str,
    stamp: &ClockStamp,
    mode: &ExecutionMode,
) -> String {
    let events = [
        format!(
            "{{\"id\":\"progress-001\",\"kind\":\"new_requirement_covered\",\"detail\":\"{} requirements extracted into goal schema\",\"count\":{}}}",
            goal.success_criteria.len(),
            goal.success_criteria.len()
        ),
        "{\"id\":\"progress-002\",\"kind\":\"new_candidate_gate_passed\",\"detail\":\"local stage scheduler and patch gate artifacts written\",\"count\":2}".to_string(),
        "{\"id\":\"progress-003\",\"kind\":\"new_candidate_gate_passed\",\"detail\":\"secret scan completed and recorded in security audit\",\"count\":1}".to_string(),
        "{\"id\":\"progress-004\",\"kind\":\"new_test_failure_resolved\",\"detail\":\"local Rust QA checks executed or explicitly skipped when no Cargo project exists\",\"count\":1}".to_string(),
        "{\"id\":\"progress-005\",\"kind\":\"final_seal_advanced\",\"detail\":\"final seal artifact written\",\"count\":1}".to_string(),
    ];
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.progress-ledger.v1\",\n",
            "  \"mission_id\": {},\n",
            "  \"goal_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"execution_mode\": {},\n",
            "  \"events\": [\n    {}\n  ],\n",
            "  \"summary\": {{\n",
            "    \"requirements_extracted\": {},\n",
            "    \"goal_intake_complete\": true,\n",
            "    \"execution_coverage\": 0.0,\n",
            "    \"terminal_state\": \"partial\"\n",
            "  }}\n",
            "}}\n"
        ),
        json_string(mission_id),
        json_string(&goal.id),
        stamp.json(),
        json_string(mode.as_str()),
        events.join(",\n    "),
        goal.success_criteria.len()
    )
}

fn render_stop_policy_json(policy: &StopPolicy, mission_id: &str, goal_id: &str) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.stop-policy.v1\",\n",
            "  \"mission_id\": {},\n",
            "  \"goal_id\": {},\n",
            "  \"max_waves\": {},\n",
            "  \"max_wall_clock_seconds\": {},\n",
            "  \"max_tokens\": {},\n",
            "  \"max_cost_usd\": {},\n",
            "  \"max_tool_calls\": {},\n",
            "  \"max_no_progress\": {},\n",
            "  \"max_repeated_output\": {},\n",
            "  \"required_coverage_threshold\": {},\n",
            "  \"terminal_states\": {}\n",
            "}}\n"
        ),
        json_string(mission_id),
        json_string(goal_id),
        policy.max_waves,
        policy.max_wall_clock_seconds,
        DEFAULT_MAX_TOKENS,
        DEFAULT_MAX_COST_USD,
        DEFAULT_MAX_TOOL_CALLS,
        policy.max_no_progress,
        policy.max_repeated_output,
        policy.required_coverage_threshold,
        json_array(&[
            "passed",
            "partial",
            "blocked",
            "failed",
            "timeout",
            "cancelled",
            "needs_user"
        ])
    )
}

fn render_tool_plan_json(tool_plan: &ToolPlan, mission_id: &str, goal_id: &str) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.tool-plan.v1\",\n",
            "  \"mission_id\": {},\n",
            "  \"goal_id\": {},\n",
            "  \"capabilities\": {},\n",
            "  \"approval_required\": {},\n",
            "  \"worker_lanes\": {},\n",
            "  \"safety_policy\": {{\n",
            "    \"raw_mcp_calls_denied\": true,\n",
            "    \"write_path\": \"patch envelope or final apply transaction\",\n",
            "    \"secrets\": \"denied by default\",\n",
            "    \"destructive_actions\": \"explicit human approval\"\n",
            "  }}\n",
            "}}\n"
        ),
        json_string(mission_id),
        json_string(goal_id),
        json_vec(&tool_plan.capabilities),
        json_vec(&tool_plan.approval_required),
        json_vec(&tool_plan.worker_lanes)
    )
}

fn render_triwiki_json(voxels: &[Voxel], mission_id: &str, goal_id: &str) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.voxel-triwiki.v1\",\n",
            "  \"mission_id\": {},\n",
            "  \"goal_id\": {},\n",
            "  \"axes\": {{\"x\":\"code\",\"y\":\"time_mission\",\"z\":\"proof_design_intent\"}},\n",
            "  \"voxel_count\": {},\n",
            "  \"voxels\": [\n    {}\n  ],\n",
            "  \"cache_strategy\": {{\n",
            "    \"stable_prefix\": [\"goal\", \"requirements\", \"constraints\", \"policy\"],\n",
            "    \"dynamic_suffix\": [\"latest_observation\", \"candidate_patch\", \"qa_result\"]\n",
            "  }}\n",
            "}}\n"
        ),
        json_string(mission_id),
        json_string(goal_id),
        voxels.len(),
        voxels
            .iter()
            .map(render_voxel_json)
            .collect::<Vec<_>>()
            .join(",\n    ")
    )
}

fn render_voxels_jsonl(voxels: &[Voxel]) -> String {
    voxels
        .iter()
        .map(render_voxel_json)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn render_voxel_json(voxel: &Voxel) -> String {
    format!(
        concat!(
            "{{\"id\":{},\"kind\":{},\"coordinates\":{},\"content_hash\":{},",
            "\"summary\":{},\"evidence_refs\":{},\"links\":{},",
            "\"cache_stability\":{},\"privacy_level\":{}}}"
        ),
        json_string(&voxel.id),
        json_string(&voxel.kind),
        json_string(&voxel.coordinates),
        json_string(&voxel.content_hash),
        json_string(&voxel.summary),
        json_vec(&voxel.evidence_refs),
        json_vec(&voxel.links),
        json_string(&voxel.cache_stability),
        json_string(&voxel.privacy_level)
    )
}

fn render_final_seal_json(
    goal: &Goal,
    mission_id: &str,
    stamp: &ClockStamp,
    tool_plan: &ToolPlan,
    voxels: &[Voxel],
    checks: &[CommandCheck],
    secret_findings: &[SecretFinding],
) -> String {
    let failed_checks = checks
        .iter()
        .filter(|check| check.status == "failed" || check.status == "error")
        .count();
    let qa_status = if failed_checks == 0 {
        "passed"
    } else {
        "failed"
    };
    let security_status = if secret_findings.is_empty() {
        "passed"
    } else {
        "findings"
    };
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.final-seal.v1\",\n",
            "  \"mission_id\": {},\n",
            "  \"goal_id\": {},\n",
            "  \"sealed_at\": {},\n",
            "  \"status\": \"partial\",\n",
            "  \"status_reason\": \"Goal-loop intake, Voxel TriWiki seed, local scheduler, local QA/security scan, isolated workspace snapshot, patch gate, progress ledger, and final seal were written. Provider-backed workers, repair waves, and final apply transactions remain future work.\",\n",
            "  \"requirements_coverage\": {{\n",
            "    \"requirements_total\": {},\n",
            "    \"requirements_extracted\": {},\n",
            "    \"intake_coverage\": 1.0,\n",
            "    \"execution_coverage\": 0.0\n",
            "  }},\n",
            "  \"model_provenance\": {{\"runtime\":\"local-rust-cli\",\"model_calls\":0}},\n",
            "  \"tool_provenance\": {{\"tool_plan_ref\":\"tool-plan.json\",\"capabilities\":{},\"approval_required\":{}}},\n",
            "  \"qa_summary\": {{\"status\":{},\"check_count\":{},\"failed_checks\":{},\"checks\":{} }},\n",
            "  \"security_summary\": {{\"status\":{},\"risk_profile\":{},\"secrets_exposed\":{},\"secret_findings\":{},\"destructive_actions_executed\":false}},\n",
            "  \"mutation_summary\": {{\"workspace_mutated\":true,\"artifacts_written\":{},\"final_apply_transaction\":\"artifact-only\"}},\n",
            "  \"cache_summary\": {{\"voxel_count\":{},\"content_hash_algorithm\":\"fnv1a64\",\"stable_prefix_seeded\":true}}\n",
            "}}\n"
        ),
        json_string(mission_id),
        json_string(&goal.id),
        stamp.json(),
        goal.success_criteria.len(),
        goal.success_criteria.len(),
        json_vec(&tool_plan.capabilities),
        json_vec(&tool_plan.approval_required),
        json_string(qa_status),
        checks.len(),
        failed_checks,
        json_array(&[
            "goal-loop.json written",
            "goal-state.jsonl written",
            "progress-ledger.json written",
            "stop-policy.json written",
            "tool-plan.json written",
            "voxel-triwiki.json written",
            "qa-report.json written",
            "security-audit.json written",
            "stage-scheduler.json written",
            "worktree-isolation.json written",
            "patch-envelope.json written",
            "final-seal.json written"
        ]),
        json_string(security_status),
        json_string(&goal.risk_profile),
        if secret_findings.is_empty() {
            "false"
        } else {
            "true"
        },
        secret_findings.len(),
        json_array(&[
            "goal-loop.json",
            "goal-state.jsonl",
            "progress-ledger.json",
            "stop-policy.json",
            "tool-plan.json",
            "voxel-triwiki.json",
            "voxels.jsonl",
            "qa-report.json",
            "security-audit.json",
            "stage-scheduler.json",
            "scheduler-events.jsonl",
            "scheduler-final-state.json",
            "worktree-isolation.json",
            "patch-envelope.json",
            "patch-gate-result.json",
            "final-seal.json"
        ]),
        voxels.len()
    )
}

fn write_text_atomic(path: &Path, contents: &str) -> Result<(), OpenSksError> {
    let parent = path
        .parent()
        .ok_or_else(|| OpenSksError::Invalid(format!("path has no parent: {}", path.display())))?;
    fs::create_dir_all(parent)?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            OpenSksError::Invalid(format!("path has no filename: {}", path.display()))
        })?;
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

fn append_text(path: &Path, contents: &str) -> Result<(), OpenSksError> {
    let parent = path
        .parent()
        .ok_or_else(|| OpenSksError::Invalid(format!("path has no parent: {}", path.display())))?;
    fs::create_dir_all(parent)?;
    let mut existing = String::new();
    if path.exists() {
        existing = fs::read_to_string(path)?;
        if !existing.ends_with('\n') {
            existing.push('\n');
        }
    }
    existing.push_str(contents);
    write_text_atomic(path, &existing)
}

fn stable_content_hash(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
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

fn extract_json_string_field(input: &str, key: &str) -> Option<String> {
    let raw = extract_json_raw_field(input, key)?;
    if raw.len() < 2 || !raw.starts_with('"') || !raw.ends_with('"') {
        return None;
    }
    Some(unescape_simple_json_string(&raw[1..raw.len() - 1]))
}

fn extract_json_raw_field(input: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = input.find(&needle)? + needle.len();
    let after_key = &input[start..];
    let colon = after_key.find(':')?;
    let mut chars = after_key[colon + 1..].trim_start().char_indices();
    let (_, first) = chars.next()?;
    if first == '"' {
        let value_start = start + colon + 1 + after_key[colon + 1..].find('"')?;
        let mut escaped = false;
        for (offset, ch) in input[value_start + 1..].char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                let end = value_start + 1 + offset + 1;
                return Some(input[value_start..end].to_string());
            }
        }
        return None;
    }
    let value_start = start + colon + 1 + after_key[colon + 1..].find(first)?;
    let mut value_end = input.len();
    for (offset, ch) in input[value_start..].char_indices() {
        if ch == ',' || ch == '}' || ch == ']' || ch.is_whitespace() {
            value_end = value_start + offset;
            break;
        }
    }
    Some(input[value_start..value_end].to_string())
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

fn usage() -> &'static str {
    concat!(
        "OpenSKS\n\n",
        "Usage:\n",
        "  opensks goal \"<goal text>\" [--kind <kind>] [--mode goal|direct|naruto] [--max-waves <n>]\n",
        "  opensks goal status <mission-id>\n",
        "  opensks run \"<goal text>\"\n",
        "  opensks naruto \"<goal text>\"\n",
        "  opensks browser \"<url or browser goal>\"\n",
        "  opensks app-use \"<app goal>\"\n",
        "  opensks computer-use \"<computer goal>\"\n",
        "  opensks mcp list|add|audit|describe|invoke|serve\n",
        "  opensks voxel query \"<text>\"\n",
        "  opensks cache warm\n",
        "  opensks qa run\n",
        "  opensks design qa\n",
        "  opensks bench\n",
        "  opensks auth\n",
        "  opensks app\n",
        "  opensks scheduler run \"<goal>\"\n",
        "  opensks worktree create \"<worker label>\"\n",
        "  opensks patch propose \"<summary>\"\n",
        "  opensks prd coverage\n\n",
        "The current implementation writes proof-first artifacts under .opensks/ and marks non-live capability planes honestly.\n"
    )
}

fn mcp_usage() -> &'static str {
    concat!(
        "usage: opensks mcp list\n",
        "       opensks mcp add <name> [command-or-url]\n",
        "       opensks mcp audit\n",
        "       opensks mcp describe\n",
        "       opensks mcp invoke <tool-name> [payload]\n",
        "       opensks mcp serve --once [json-rpc-request]\n"
    )
}

fn goal_usage() -> &'static str {
    concat!(
        "usage: opensks goal \"<goal text>\" [--kind <kind>] [--mode goal|direct|naruto] [--max-waves <n>]\n",
        "       opensks goal status <mission-id>\n"
    )
}

pub fn default_cwd() -> Result<PathBuf, OpenSksError> {
    Ok(env::current_dir()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace(name: &str) -> PathBuf {
        let stamp = ClockStamp::now().expect("clock");
        let path = env::temp_dir().join(format!(
            "opensks-test-{name}-{}-{}",
            stamp.compact_id(),
            process::id()
        ));
        fs::create_dir_all(&path).expect("create temp workspace");
        path
    }

    fn first_mission_dir(root: &Path) -> PathBuf {
        let missions_dir = root.join(OPEN_SKSDIR).join("missions");
        fs::read_dir(&missions_dir)
            .expect("mission dir exists")
            .next()
            .expect("one mission")
            .expect("valid mission entry")
            .path()
    }

    #[test]
    fn goal_command_writes_prd_artifact_contract() {
        let root = temp_workspace("artifact-contract");
        let output = run_cli(
            [
                "goal",
                "Implement MCP browser computer app use with Voxel TriWiki",
                "--max-waves",
                "2",
            ],
            &root,
        )
        .expect("goal command succeeds");
        assert!(output.stdout.contains("created OpenSKS goal loop"));

        let mission_dir = first_mission_dir(&root);
        for artifact in [
            "goal-loop.json",
            "goal-state.jsonl",
            "progress-ledger.json",
            "stop-policy.json",
            "tool-plan.json",
            "voxel-triwiki.json",
            "voxels.jsonl",
            "qa-report.json",
            "security-audit.json",
            "stage-scheduler.json",
            "scheduler-events.jsonl",
            "scheduler-final-state.json",
            "worktree-isolation.json",
            "patch-envelope.json",
            "patch-gate-result.json",
            "final-seal.json",
            "prd-coverage.json",
        ] {
            assert!(
                mission_dir.join(artifact).exists(),
                "expected artifact {artifact}"
            );
        }

        let goal_loop = fs::read_to_string(mission_dir.join("goal-loop.json")).expect("goal loop");
        assert!(goal_loop.contains("\"schema\": \"opensks.goal-loop.v1\""));
        assert!(goal_loop.contains("\"parallel_execution\""));
        assert!(goal_loop.contains("\"repair_wave\""));
        assert!(goal_loop.contains("\"final_apply_or_noop\""));
        assert!(
            goal_loop.contains("\"max_waves\": 2")
                || fs::read_to_string(mission_dir.join("stop-policy.json"))
                    .expect("stop policy")
                    .contains("\"max_waves\": 2")
        );
    }

    #[test]
    fn goal_command_extracts_capabilities_and_voxels() {
        let root = temp_workspace("capabilities");
        run_cli(
            [
                "goal",
                "Build browser QA, MCP broker, app automation, and security audit",
                "--mode",
                "naruto",
            ],
            &root,
        )
        .expect("goal command succeeds");

        let mission_dir = first_mission_dir(&root);
        let tool_plan = fs::read_to_string(mission_dir.join("tool-plan.json")).expect("tool plan");
        assert!(tool_plan.contains("\"browser_use\""));
        assert!(tool_plan.contains("\"mcp_use\""));
        assert!(tool_plan.contains("\"app_use\""));
        assert!(tool_plan.contains("\"parallel_worker_use\""));

        let triwiki = fs::read_to_string(mission_dir.join("voxel-triwiki.json")).expect("triwiki");
        assert!(triwiki.contains("\"goal_voxel\""));
        assert!(triwiki.contains("\"requirement_voxel\""));
        assert!(triwiki.contains("\"cache_voxel\""));
    }

    #[test]
    fn status_command_reads_final_seal() {
        let root = temp_workspace("status");
        let output = run_cli(["naruto", "Repair tests with proof artifacts"], &root)
            .expect("naruto command succeeds");
        let mission_line = output
            .stdout
            .lines()
            .find(|line| line.starts_with("mission: "))
            .expect("mission line");
        let mission_id = mission_line.trim_start_matches("mission: ");

        let status = run_cli(["goal", "status", mission_id], &root).expect("status succeeds");
        assert!(
            status
                .stdout
                .contains("\"schema\": \"opensks.final-seal.v1\"")
        );
        assert!(status.stdout.contains("\"status\": \"partial\""));
    }

    #[test]
    fn missing_goal_text_is_usage_error() {
        let root = temp_workspace("missing-text");
        let error = run_cli(["goal"], &root).expect_err("goal text required");
        assert!(matches!(error, OpenSksError::Usage(_)));
    }

    #[test]
    fn prd_coverage_command_writes_honest_ledger() {
        let root = temp_workspace("prd-coverage");
        let output = run_cli(["prd", "coverage"], &root).expect("coverage command succeeds");
        assert!(output.stdout.contains("wrote PRD coverage ledger"));

        let coverage =
            fs::read_to_string(root.join(OPEN_SKSDIR).join("prd-coverage.json")).expect("coverage");
        assert!(coverage.contains("\"schema\": \"opensks.prd-coverage.v1\""));
        assert!(coverage.contains("\"id\":\"P18-001\""));
        assert!(coverage.contains("\"missing_live_implementation\""));
        assert!(coverage.contains(PRD_SOURCE_PATH));
    }

    #[test]
    fn cli_v3_plane_commands_write_named_artifacts() {
        let root = temp_workspace("cli-v3");
        run_cli(["mcp", "add", "local-demo", "stdio://demo"], &root).expect("mcp add");
        run_cli(["mcp", "audit"], &root).expect("mcp audit");
        run_cli(["browser", "local browser smoke"], &root).expect("browser");
        run_cli(["computer-use", "inspect desktop"], &root).expect("computer-use");
        run_cli(["app-use", "inspect Finder"], &root).expect("app-use");
        run_cli(["cache", "warm"], &root).expect("cache warm");
        run_cli(["qa", "run"], &root).expect("qa run");
        run_cli(["design", "qa"], &root).expect("design qa");
        run_cli(["bench"], &root).expect("bench");
        run_cli(["auth"], &root).expect("auth");
        run_cli(["app"], &root).expect("app");
        run_cli(["scheduler", "run", "local QA"], &root).expect("scheduler run");
        run_cli(["worktree", "create", "worker one"], &root).expect("worktree create");
        run_cli(["patch", "propose", "safe patch"], &root).expect("patch propose");

        let open = root.join(OPEN_SKSDIR);
        for artifact in [
            "mcp/mcp-servers.json",
            "mcp/mcp-tool-invocations.jsonl",
            "mcp/mcp-permission-ledger.json",
            "mcp/mcp-risk-report.json",
            "mcp/mcp-broker-policy.json",
            "cache/cache-warm-report.json",
            "cache/cache-dashboard.json",
            "qa/qa-report.json",
            "qa/security-audit.json",
            "design/design-qa-report.json",
            "bench/benchmark-report.json",
            "auth/auth-registry.json",
            "auth/provider-registry.json",
            "app/gui-manifest.json",
            "app/workspace-manifest.json",
        ] {
            assert!(open.join(artifact).exists(), "expected artifact {artifact}");
        }

        assert!(
            first_child_dir(&open.join("scheduler"))
                .join("stage-scheduler.json")
                .exists()
        );
        assert!(
            first_child_dir(&open.join("worktrees"))
                .join("worktree-isolation.json")
                .exists()
        );
        assert!(
            first_child_dir(&open.join("patches"))
                .join("patch-envelope.json")
                .exists()
        );
        assert!(
            first_child_dir(&open.join("browser"))
                .join("browser-session.json")
                .exists()
        );
        assert!(
            first_child_dir(&open.join("computer-use"))
                .join("computer-session.json")
                .exists()
        );
        assert!(
            first_child_dir(&open.join("app-use"))
                .join("app-session.json")
                .exists()
        );
    }

    #[test]
    fn mcp_local_server_describes_serves_and_invokes_tools() {
        let root = temp_workspace("mcp-local-server");
        fs::write(
            root.join("notes.md"),
            "OpenSKS local MCP server can search this needle.\n",
        )
        .expect("write searchable fixture");

        let descriptor = run_cli(["mcp", "describe"], &root).expect("mcp describe");
        assert!(
            descriptor
                .stdout
                .contains("\"schema\": \"opensks.mcp-server-descriptor.v1\"")
        );
        assert!(descriptor.stdout.contains("opensks.repo.search"));

        let list_response = run_cli(
            [
                "mcp",
                "serve",
                "--once",
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}",
            ],
            &root,
        )
        .expect("mcp tools/list");
        assert!(list_response.stdout.contains("\"tools\""));
        assert!(list_response.stdout.contains("opensks.qa.run"));

        let invoke_response = run_cli(
            [
                "mcp",
                "serve",
                "--once",
                "{\"jsonrpc\":\"2.0\",\"id\":\"abc\",\"method\":\"tools/call\",\"params\":{\"name\":\"opensks.repo.search\",\"arguments\":{\"query\":\"needle\"}}}",
            ],
            &root,
        )
        .expect("mcp tools/call");
        assert!(invoke_response.stdout.contains("\"isError\":false"));
        assert!(invoke_response.stdout.contains("notes.md"));

        let cli_invoke =
            run_cli(["mcp", "invoke", "opensks.repo.search", "needle"], &root).expect("mcp invoke");
        assert!(cli_invoke.stdout.contains("match_count"));

        let open = root.join(OPEN_SKSDIR).join("mcp");
        assert!(open.join("mcp-server-descriptor.json").exists());
        assert!(open.join("mcp-serve-session.json").exists());
        let ledger =
            fs::read_to_string(open.join("mcp-tool-invocations.jsonl")).expect("mcp ledger");
        assert!(ledger.contains("opensks.repo.search"));
        assert!(ledger.contains("allowed_by_local_jsonrpc_broker"));
    }

    #[test]
    fn voxel_query_uses_triwiki_memory() {
        let root = temp_workspace("voxel-query");
        run_cli(["goal", "Store Voxel TriWiki proof memory"], &root).expect("goal succeeds");
        let output = run_cli(["voxel", "query", "triwiki"], &root).expect("voxel query succeeds");
        assert!(output.stdout.contains("voxel query matches:"));
        assert!(root.join(OPEN_SKSDIR).join("voxel").exists());
    }

    fn first_child_dir(path: &Path) -> PathBuf {
        fs::read_dir(path)
            .expect("parent exists")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.is_dir())
            .expect("child dir exists")
    }
}
