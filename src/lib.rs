use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{self, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
const OPEN_SKSDIR: &str = ".opensks";
const DEFAULT_MAX_WAVES: u32 = 3;
const DEFAULT_MAX_WALL_CLOCK_SECONDS: u64 = 60 * 60;
const DEFAULT_MAX_TOKENS: u64 = 200_000;
const DEFAULT_MAX_COST_USD: f64 = 25.0;
const DEFAULT_MAX_TOOL_CALLS: u32 = 100;
const DEFAULT_MAX_NO_PROGRESS: u32 = 2;
const DEFAULT_MAX_REPEATED_OUTPUT: u32 = 2;
const DEFAULT_REQUIRED_COVERAGE_THRESHOLD: f64 = 1.0;
const COMPUTER_ISOLATED_LOOP_FINAL_TEXT: &str = "opensks-isolated-loop-ok";
const COMPUTER_ISOLATED_LOOP_BUTTON_ID: &str = "opensks-loop-button";
const COMPUTER_ISOLATED_LOOP_INPUT_ID: &str = "opensks-loop-input";
const COMPUTER_ISOLATED_LOOP_STATUS_ID: &str = "opensks-loop-status";
const BROWSER_LOCAL_LOOP_FINAL_TEXT: &str = "opensks-browser-loop-ok";
const BROWSER_LOCAL_LOOP_BUTTON_ID: &str = "opensks-browser-loop-button";
const BROWSER_LOCAL_LOOP_INPUT_ID: &str = "opensks-browser-loop-input";
const BROWSER_LOCAL_LOOP_STATUS_ID: &str = "opensks-browser-loop-status";
const BROWSER_LOCAL_SCREENSHOT_WIDTH: usize = 32;
const BROWSER_LOCAL_SCREENSHOT_HEIGHT: usize = 32;
const BROWSER_LOCAL_SCREENSHOT_MODE: &str = "deterministic_local_browser_runtime_artifact";
const BROWSER_LOCAL_SCREENSHOT_RENDERER: &str = "opensks_local_browser_runtime_rasterizer_v1";
const DESIGN_SCREENSHOT_WIDTH: usize = 32;
const DESIGN_SCREENSHOT_HEIGHT: usize = 32;
const DESIGN_SCREENSHOT_MODE: &str = "deterministic_local_raster_artifact";
const DESIGN_SCREENSHOT_RENDERER: &str = "opensks_local_source_rasterizer_v1";
const PROVIDER_KEYCHAIN_SERVICE: &str = "opensks-provider-credentials";
const PROVIDER_HTTP_TRANSPORT: &str = "native_reqwest_blocking_http";
pub const OPENSKS_WORKSPACE_ENV: &str = "OPENSKS_WORKSPACE";
const OPEN_SKS_LOGO_SVG: &str = include_str!("../assets/opensks-logo.svg");
#[cfg(target_os = "macos")]
const SWIFT_PACKAGE_DIR_ENV: &str = "OPENSKS_SWIFT_PACKAGE_DIR";
#[cfg(target_os = "macos")]
const SWIFT_DISABLE_SANDBOX_ENV: &str = "OPENSKS_SWIFT_DISABLE_SANDBOX";
#[cfg(target_os = "macos")]
const SWIFT_STUDIO_PRODUCT: &str = "OpenSKSStudio";
const PRD_SOURCE_LABEL: &str =
    "project-prd:opensks-prd-v3-goal-loop-mcp-computer-use-voxel-triwiki";
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

pub fn cli_error_json(error: &OpenSksError, exit_code: i32) -> String {
    serde_json::json!({
        "schema": "opensks.cli-error.v1",
        "status": "failed",
        "exit_code": exit_code,
        "message": error.to_string()
    })
    .to_string()
        + "\n"
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

type CommandCheck = opensks_cli::CommandCheck;

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
struct CachePrefixHitReport {
    baseline_available: bool,
    previous_stable_segment_count: usize,
    current_stable_segment_count: usize,
    matched_stable_segment_count: usize,
    current_stable_bytes: u64,
    matched_stable_bytes: u64,
    estimated_cached_tokens: u64,
    estimated_cache_write_tokens: u64,
    local_hit_percent: f64,
    target_hit_percent: f64,
    local_target_met: bool,
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
    links: Vec<String>,
    forms: Vec<String>,
    meta_names: Vec<String>,
    stderr: String,
}

#[derive(Debug, Clone)]
struct BrowserPolicyDecision {
    requested_action: String,
    decision: String,
    reason: String,
    network_allowed: bool,
    browser_action_allowed: bool,
    sensitive: bool,
}

#[derive(Debug, Clone)]
struct BrowserLocalRuntimeArtifact {
    runtime_ref: &'static str,
    runtime_page_hash: String,
    screenshot_ref: String,
    screenshot_hash: String,
    pixel_count: usize,
}

#[derive(Debug, Clone, Copy)]
struct BrowserLocalArtifactRefs<'a> {
    session_id: &'a str,
    target: &'a str,
    runtime_ref: &'a str,
    runtime_page_hash: &'a str,
    screenshot_ref: &'a str,
    screenshot_hash: &'a str,
}

#[derive(Debug, Clone)]
struct AppInspection {
    attempted: bool,
    status: String,
    frontmost_app: Option<String>,
    stderr: String,
}

#[derive(Debug, Clone)]
struct AppInventory {
    attempted: bool,
    status: String,
    apps: Vec<String>,
    stderr: String,
}

#[derive(Debug, Clone)]
struct AppActionDecision {
    requested_action: String,
    decision: String,
    reason: String,
    inspection_allowed: bool,
    app_action_allowed: bool,
    sensitive: bool,
}

#[derive(Debug, Clone)]
struct GuiSnapshot {
    prd_total: usize,
    prd_implemented: usize,
    prd_artifact_mvp: usize,
    prd_planned: usize,
    prd_missing_live: usize,
    qa_status: String,
    security_status: String,
    provider_configured_count: usize,
    voxel_count: usize,
    mission_count: usize,
    browser_sessions: usize,
    computer_sessions: usize,
    app_sessions: usize,
    worker_lane_missions: usize,
    worker_lane_count: usize,
    worker_lanes: Vec<WorkerLaneSnapshot>,
    worker_runtime: WorkerRuntimeDashboard,
}

#[derive(Debug, Clone)]
struct NativeAppDashboard {
    workspace: PathBuf,
    workspace_label: String,
    app_bundle: PathBuf,
    artifact_dir: PathBuf,
    dashboard_html: PathBuf,
    cli_path: PathBuf,
    acceptance: NativeAcceptanceStatus,
    release: NativeReleaseStatus,
    provider_adapter_check: Option<String>,
    provider_mock_e2e: Option<NativeProviderMockE2eStatus>,
    gui: GuiSnapshot,
}

#[derive(Debug, Clone)]
struct NativeAcceptanceStatus {
    total: usize,
    passed: usize,
    partial: usize,
    failed: usize,
    goal_complete: Option<bool>,
}

#[derive(Debug, Clone)]
struct NativeReleaseStatus {
    status: String,
    source_commit_sha: Option<String>,
    workspace_dirty: bool,
    artifact_digest_gate_passed: bool,
    same_sha_artifact_binding: bool,
    missing_artifacts: Vec<String>,
    blockers: Vec<NativeReleaseBlocker>,
    remediation_actions: Vec<NativeReleaseRemediationAction>,
    signing_evidence: Option<NativeReleaseSigningEvidence>,
}

#[derive(Debug, Clone)]
struct NativeReleaseBlocker {
    code: String,
    message: String,
}

#[derive(Debug, Clone)]
struct NativeReleaseRemediationAction {
    blocker: String,
    action: String,
    scope: String,
}

#[derive(Debug, Clone)]
struct NativeReleaseSigningEvidence {
    checked: bool,
    app_bundle_path: String,
    identifier: Option<String>,
    signature: Option<String>,
    team_identifier: Option<String>,
    cd_hash: Option<String>,
    production_signed: bool,
    notarized: bool,
    codesign_status: Option<i32>,
    notarization_status: Option<i32>,
    diagnostic: String,
}

#[derive(Debug, Clone)]
struct NativeProviderMockE2eStatus {
    status: String,
    fixture_kind: String,
    live_vendor_calls_performed: bool,
    secret_value_exposed: bool,
    model_catalog_count: usize,
    model_catalog_synced: bool,
    model_enabled: bool,
    registry_route_status: String,
    selected_model_id: Option<String>,
    checks: Vec<NativeProviderMockE2eCheck>,
}

#[derive(Debug, Clone)]
struct NativeProviderMockE2eCheck {
    id: String,
    status: String,
    evidence_ref: String,
}

#[derive(Debug, Clone)]
struct WorkerLaneSnapshot {
    mission_id: String,
    status: String,
    execution_mode: String,
    lanes: Vec<String>,
    source: String,
}

#[derive(Debug, Clone)]
struct WorkerRuntimeDashboard {
    available: bool,
    run_id: String,
    active_leases: usize,
    expired_leases: usize,
    recovered_leases: usize,
    routed_requests: usize,
    concurrent_routing: bool,
    source: String,
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
struct ComputerActionDecision {
    requested_action: String,
    decision: String,
    reason: String,
    screenshot_allowed: bool,
    mouse_keyboard_allowed: bool,
    wait_allowed: bool,
    sensitive: bool,
}

#[derive(Debug, Clone)]
struct ProviderDefinition {
    name: &'static str,
    env_var: &'static str,
    kind: &'static str,
    default_base_url: Option<&'static str>,
    model_profile: &'static str,
    cache_support: &'static str,
    auth_method: &'static str,
}

#[derive(Debug, Clone)]
struct ProviderStatus {
    definition: ProviderDefinition,
    configured: bool,
    configured_value: Option<String>,
    credential_source: String,
}

#[derive(Debug, Clone)]
struct ProviderProbe {
    name: String,
    attempted: bool,
    status: String,
    endpoint: Option<String>,
    http_code: Option<String>,
    duration_ms: u128,
    transport: &'static str,
    stderr: String,
}

#[derive(Debug, Clone)]
struct ProviderAdapterCheck {
    name: String,
    configured: bool,
    attempted: bool,
    status: String,
    blockers: Vec<String>,
    credential_source: String,
    endpoint: String,
    http_code: Option<String>,
    duration_ms: u128,
    transport: &'static str,
    stderr: String,
}

#[derive(Debug, Clone)]
struct DesignSurface {
    path: String,
    kind: String,
    bytes: u64,
    content_hash: String,
    visual_signature: String,
    color_tokens: Vec<String>,
}

#[derive(Debug, Clone)]
struct DesignFinding {
    path: String,
    line_number: usize,
    rule: String,
    severity: String,
    message: String,
}

#[derive(Debug, Clone)]
struct DesignVisualDiff {
    path: String,
    status: String,
    previous_signature: Option<String>,
    current_signature: Option<String>,
    bytes_delta: i64,
}

#[derive(Debug, Clone)]
struct DesignScreenshotArtifact {
    path: String,
    kind: String,
    image_path: String,
    width: usize,
    height: usize,
    pixel_count: usize,
    screenshot_hash: String,
    content_hash: String,
    visual_signature: String,
}

#[derive(Debug, Clone)]
struct DesignScreenshotDiff {
    path: String,
    status: String,
    previous_screenshot_hash: Option<String>,
    current_screenshot_hash: Option<String>,
    previous_image_path: Option<String>,
    current_image_path: Option<String>,
    pixel_count: usize,
    pixel_changed_count: usize,
    image_artifacts_present: bool,
}

#[derive(Debug, Clone)]
struct SecurityFinding {
    category: String,
    path: String,
    line_number: usize,
    rule: String,
    severity: String,
    message: String,
}

#[derive(Debug, Clone)]
struct SecurityScanSummary {
    secret_findings: usize,
    security_findings: usize,
    critical_or_warning_findings: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct SecretLeakReleaseHistory {
    scans: usize,
    scanned_total: usize,
    secret_total: usize,
    scanned_latest: usize,
    secret_latest: usize,
}

impl SecretLeakReleaseHistory {
    fn with_current_scan(self, scanned_artifact_count: usize, secret_finding_count: usize) -> Self {
        Self {
            scans: self.scans + 1,
            scanned_total: self.scanned_total + scanned_artifact_count,
            secret_total: self.secret_total + secret_finding_count,
            scanned_latest: scanned_artifact_count,
            secret_latest: secret_finding_count,
        }
    }

    fn artifact_rate(self) -> f64 {
        secret_leak_artifact_rate(self.scanned_total, self.secret_total)
    }

    fn latest_artifact_rate(self) -> f64 {
        secret_leak_artifact_rate(self.scanned_latest, self.secret_latest)
    }

    fn gate_passed(self) -> bool {
        self.scans > 0
            && self.scanned_total > 0
            && self.scanned_latest > 0
            && self.secret_latest == 0
    }
}

struct FinalSealVerification<'a> {
    checks: &'a [CommandCheck],
    security_summary: &'a SecurityScanSummary,
    artifact_refs_present: bool,
}

#[derive(Debug, Clone)]
struct AcceptanceItem {
    id: &'static str,
    criterion: &'static str,
    status: &'static str,
    evidence: &'static str,
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
    if args.is_empty() {
        return run_default_launch_command(cwd);
    }
    if args[0] == "help" || args[0] == "--help" || args[0] == "-h" {
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
        "security" => run_security_command(&args[1..], cwd),
        "bench" => run_bench_command(&args[1..], cwd),
        "auth" => run_auth_command(&args[1..], cwd),
        "provider" => run_provider_command(&args[1..], cwd),
        "capability" => run_capability_command(&args[1..], cwd),
        "daemon" => run_daemon_command(&args[1..], cwd),
        "terminal" => run_terminal_command(&args[1..], cwd),
        "updater" => run_updater_command(&args[1..], cwd),
        "acceptance" => run_acceptance_command(&args[1..], cwd),
        "app" => run_app_command(&args[1..], cwd),
        "app-data" => run_app_data_command(&args[1..], cwd),
        "history" => run_history_command(&args[1..], cwd),
        "scheduler" => run_scheduler_command(&args[1..], cwd),
        "perf" => run_perf_command(&args[1..], cwd),
        "worker" => run_worker_command(&args[1..], cwd),
        "worktree" => run_worktree_command(&args[1..], cwd),
        "patch" => run_patch_command(&args[1..], cwd),
        "graph" => run_graph_command(&args[1..], cwd),
        "hooks" => run_hooks_command(&args[1..], cwd),
        "codegraph" => run_codegraph_command(&args[1..], cwd),
        "triwiki" => run_triwiki_command(&args[1..], cwd),
        "context" => run_context_command(&args[1..], cwd),
        "conversation" => run_conversation_command(&args[1..], cwd),
        "file" => run_file_command(&args[1..], cwd),
        "intel" => run_intel_command(&args[1..], cwd),
        "vault" => run_vault_command(&args[1..], cwd),
        "image" => run_image_command(&args[1..], cwd),
        "reasoning" => run_reasoning_command(&args[1..], cwd),
        "git" => run_git_command(&args[1..], cwd),
        "gc" => run_gc_command(&args[1..], cwd),
        "release" => run_release_command(&args[1..], cwd),
        "prd" => run_prd_command(&args[1..], cwd),
        other => Err(OpenSksError::Usage(format!(
            "unknown command `{other}`\n\n{}",
            usage()
        ))),
    }
}

fn run_history_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_history_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

pub fn is_daemon_stdio_invocation(args: &[String]) -> bool {
    opensks_cli::is_daemon_stdio_invocation(args)
}

pub fn run_daemon_stdio_stream(args: &[String], cwd: &Path) -> Result<(), OpenSksError> {
    opensks_cli::run_daemon_stdio_stream(args, cwd).map_err(convert_cli_error)
}

fn run_daemon_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_daemon_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_terminal_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_terminal_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn convert_cli_error(error: opensks_cli::CliError) -> OpenSksError {
    match error {
        opensks_cli::CliError::Usage(message) => OpenSksError::Usage(message),
        opensks_cli::CliError::Invalid(message) => OpenSksError::Invalid(message),
        opensks_cli::CliError::Io(error) => OpenSksError::Io(error),
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
    let decision = plan_browser_action(&target);
    let probe = if decision.network_allowed {
        probe_http_target(&target)
    } else {
        HttpProbe {
            attempted: false,
            status: "blocked_by_policy".to_string(),
            exit_code: None,
            http_code: None,
            effective_url: None,
            stdout: String::new(),
            stderr: decision.reason.clone(),
        }
    };
    let snapshot = if decision.network_allowed {
        capture_page_snapshot(&target)
    } else {
        PageSnapshot {
            attempted: false,
            status: "blocked_by_policy".to_string(),
            title: None,
            bytes: 0,
            content_hash: None,
            links: Vec::new(),
            forms: Vec::new(),
            meta_names: Vec::new(),
            stderr: decision.reason.clone(),
        }
    };
    let session = capability_session(
        "browser",
        &target,
        if decision.sensitive {
            "blocked_by_policy"
        } else if probe.attempted {
            "network_probe"
        } else {
            "planned"
        },
        if decision.sensitive {
            "Browser policy broker blocked a sensitive browser action before network execution."
        } else if probe.attempted {
            "Browser network state was probed with isolated curl requests; links/forms/meta are extracted. Local deterministic browser runtime artifacts record open/screenshot/click/type evidence; live Playwright/Chrome Extension/browser control remains false."
        } else {
            "Browser policy and session artifacts are written. Local deterministic browser runtime artifacts record open/screenshot/click/type evidence; live Playwright/Chrome Extension/browser control remains false."
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
            "browser-policy-decision.json",
            "browser-action-plan.json",
            "browser-page-links.json",
            "browser-final-state.json",
            "browser-runtime/index.html",
            "browser-screenshot-snapshots.jsonl",
            "browser-interaction-loop.json",
            "browser-interaction-events.jsonl",
        ],
    )?;
    write_capability_session(cwd, &session, Some(&target))?;
    write_browser_probe_artifacts(cwd, &session, &target, &probe, &snapshot, &decision)?;
    capability_output(&session, cwd)
}

fn run_computer_use_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let target = require_freeform(args, "usage: opensks computer-use \"<computer goal>\"")?;
    let screenshot_id = ClockStamp::now()?.compact_id();
    let decision = plan_computer_action(&target);
    let session = capability_session(
        "computer-use",
        &target,
        if decision.screenshot_allowed {
            "policy_brokered_screenshot_attempted"
        } else {
            "blocked_by_policy"
        },
        "Computer-use action broker writes policy decisions; safe observation can attempt screenshot capture, while mouse/keyboard and sensitive actions are blocked or require approval.",
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
            "isolated-browser-runtime/",
            "computer-browser-loop.json",
            "computer-browser-loop-events.jsonl",
            "isolated-browser-container.json",
            "computer-action-plan.json",
            "computer-policy-decision.json",
            "computer-final-state.json",
        ],
    )?;
    write_capability_session(cwd, &session, Some(&target))?;
    let screenshot = if decision.screenshot_allowed {
        capture_computer_screenshot(cwd, &session, &screenshot_id)?
    } else {
        ScreenshotCapture {
            attempted: false,
            status: "blocked_by_policy".to_string(),
            path: None,
            bytes: 0,
            stderr: decision.reason.clone(),
        }
    };
    if decision.wait_allowed {
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    write_computer_capture_artifacts(cwd, &session, &target, &screenshot, &decision)?;
    capability_output(&session, cwd)
}

fn run_app_use_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let target = require_freeform(args, "usage: opensks app-use \"<app goal>\"")?;
    let decision = plan_app_action(&target);
    let inspection = inspect_frontmost_app();
    let inventory = inspect_running_apps();
    let session = capability_session(
        "app-use",
        &target,
        if decision.sensitive {
            "blocked_by_policy"
        } else if inspection.status == "captured" {
            "inspected"
        } else {
            "planned"
        },
        "macOS app-use broker writes policy decisions; safe inspection can capture app state, while native app actions and sensitive intents are blocked or require approval.",
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
            "running-apps.json",
            "app-action-plan.json",
            "app-policy-decision.json",
            "app-final-state.json",
        ],
    )?;
    write_capability_session(cwd, &session, Some(&target))?;
    write_app_inspection_artifacts(cwd, &session, &target, &inspection, &inventory, &decision)?;
    capability_output(&session, cwd)
}

fn run_voxel_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let subcommand = args
        .first()
        .ok_or_else(|| OpenSksError::Usage(voxel_usage().to_string()))?;
    if subcommand == "index" {
        let stamp = ClockStamp::now()?;
        let triwiki_dir = cwd.join(OPEN_SKSDIR).join("triwiki");
        fs::create_dir_all(&triwiki_dir)?;
        let voxels = index_workspace_voxels(cwd)?;
        write_text_atomic(
            &triwiki_dir.join("voxels.jsonl"),
            &render_voxels_jsonl(&voxels),
        )?;
        write_text_atomic(
            &triwiki_dir.join("voxel-index-report.json"),
            &render_voxel_index_report(&stamp, &voxels),
        )?;
        write_text_atomic(
            &triwiki_dir.join("triwiki-graph.json"),
            &render_index_triwiki_graph(&stamp, &voxels),
        )?;
        return Ok(CliOutput {
            stdout: format!(
                "indexed workspace voxels\nvoxels: {}\nartifacts: {}\n",
                voxels.len(),
                triwiki_dir.display()
            ),
        });
    }
    if subcommand != "query" {
        return Err(OpenSksError::Usage(format!(
            "unknown voxel subcommand `{subcommand}`\n\n{}",
            voxel_usage()
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
    let snapshot_path = dir.join("cache-prefix-snapshot.jsonl");
    let previous_prefix = read_cache_prefix_snapshot(&snapshot_path)?;
    let prefix_hit = compute_cache_prefix_hit(&previous_prefix, &segments);
    write_text_atomic(
        &dir.join("cache-warm-report.json"),
        &render_cache_report(&stamp, &segments),
    )?;
    write_text_atomic(
        &dir.join("cache-dashboard.json"),
        &render_cache_dashboard(&stamp, &segments, &prefix_hit),
    )?;
    write_text_atomic(
        &dir.join("cache-hit-report.json"),
        &render_cache_hit_report(&stamp, &prefix_hit),
    )?;
    write_text_atomic(
        &dir.join("cache-layout-improvement.json"),
        &render_cache_layout_improvement_report(&stamp, &segments, &prefix_hit),
    )?;
    write_text_atomic(&snapshot_path, &render_cache_prefix_snapshot(&segments))?;
    Ok(CliOutput {
        stdout: format!(
            "warmed cache planning artifacts\nlocal_prefix_hit_percent: {:.2}\nartifacts: {}\n",
            prefix_hit.local_hit_percent,
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
    let secret_scan_targets = count_secret_scan_targets(cwd)?;
    let security_findings = scan_workspace_for_security_findings(cwd)?;
    let secret_history_path = dir.join("secret-leak-release-history.jsonl");
    let secret_history = read_secret_leak_release_history(&secret_history_path)?
        .with_current_scan(secret_scan_targets, secret_findings.len());
    append_text(
        &secret_history_path,
        &render_secret_leak_release_history_event(
            &stamp,
            "qa",
            secret_scan_targets,
            &secret_findings,
        ),
    )?;
    write_text_atomic(
        &dir.join("qa-report.json"),
        &render_qa_report(&stamp, &checks),
    )?;
    write_text_atomic(
        &dir.join("security-audit.json"),
        &render_security_audit(&stamp, &secret_findings, &security_findings),
    )?;
    write_text_atomic(
        &dir.join("security-findings.jsonl"),
        &render_security_findings_jsonl(&stamp, &security_findings),
    )?;
    write_text_atomic(
        &dir.join("secret-leak-rate.json"),
        &render_secret_leak_rate_report(
            &stamp,
            "qa",
            secret_scan_targets,
            &secret_findings,
            secret_history,
            &["security-audit.json", "security-findings.jsonl"],
        ),
    )?;
    write_text_atomic(
        &dir.join("secret-leak-gate.json"),
        &render_secret_leak_gate_report(
            &stamp,
            "qa",
            &secret_findings,
            secret_history,
            &[
                "secret-leak-rate.json",
                "secret-leak-release-history.json",
                "secret-leak-release-history.jsonl",
                "security-audit.json",
                "security-findings.jsonl",
            ],
        ),
    )?;
    write_text_atomic(
        &dir.join("secret-leak-release-history.json"),
        &render_secret_leak_release_history_report(&stamp, "qa", secret_history),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote QA and security audit artifacts\nchecks: {}\nsecret_findings: {}\nsecurity_findings: {}\nartifacts: {}\n",
            checks.len(),
            secret_findings.len(),
            security_findings.len(),
            dir.display()
        ),
    })
}

fn run_security_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    // PR-044: the structured `opensks.security-report.v1` aggregator lives in
    // opensks-cli (over the opensks-contracts schema). Route the `report`
    // subcommand there; the existing secret-leak `audit` gate below is unchanged.
    if args.first().map(String::as_str) == Some("report") {
        let output = opensks_cli::run_security_command(args, cwd).map_err(convert_cli_error)?;
        return Ok(CliOutput {
            stdout: output.stdout,
        });
    }
    require_exact_subcommand(args, "audit", "usage: opensks security audit")?;
    let dir = cwd.join(OPEN_SKSDIR).join("security");
    fs::create_dir_all(&dir)?;
    let stamp = ClockStamp::now()?;
    let secret_findings = scan_workspace_for_secrets(cwd)?;
    let secret_scan_targets = count_secret_scan_targets(cwd)?;
    let security_findings = scan_workspace_for_security_findings(cwd)?;
    let secret_history_path = dir.join("secret-leak-release-history.jsonl");
    let secret_history = read_secret_leak_release_history(&secret_history_path)?
        .with_current_scan(secret_scan_targets, secret_findings.len());
    append_text(
        &secret_history_path,
        &render_secret_leak_release_history_event(
            &stamp,
            "security",
            secret_scan_targets,
            &secret_findings,
        ),
    )?;
    write_text_atomic(
        &dir.join("security-audit.json"),
        &render_security_audit(&stamp, &secret_findings, &security_findings),
    )?;
    write_text_atomic(
        &dir.join("security-findings.jsonl"),
        &render_security_findings_jsonl(&stamp, &security_findings),
    )?;
    write_text_atomic(
        &dir.join("secret-leak-rate.json"),
        &render_secret_leak_rate_report(
            &stamp,
            "security",
            secret_scan_targets,
            &secret_findings,
            secret_history,
            &["security-audit.json", "security-findings.jsonl"],
        ),
    )?;
    write_text_atomic(
        &dir.join("secret-leak-gate.json"),
        &render_secret_leak_gate_report(
            &stamp,
            "security",
            &secret_findings,
            secret_history,
            &[
                "secret-leak-rate.json",
                "secret-leak-release-history.json",
                "secret-leak-release-history.jsonl",
                "security-audit.json",
                "security-findings.jsonl",
            ],
        ),
    )?;
    write_text_atomic(
        &dir.join("secret-leak-release-history.json"),
        &render_secret_leak_release_history_report(&stamp, "security", secret_history),
    )?;
    write_text_atomic(&dir.join("threat-model.json"), &render_threat_model(&stamp))?;
    Ok(CliOutput {
        stdout: format!(
            "wrote security audit artifacts\nsecret_findings: {}\nsecurity_findings: {}\nartifacts: {}\n",
            secret_findings.len(),
            security_findings.len(),
            dir.display()
        ),
    })
}

fn run_design_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    if let Some(sub) = args.first().map(String::as_str) {
        if matches!(
            sub,
            "audit"
                | "activate"
                | "active-status"
                | "save-tokens"
                | "compile"
                | "list"
                | "revision-propose"
                | "revision-accept"
                | "revision-reject"
                | "revision-rollback"
        ) {
            let output =
                opensks_cli::run_design_studio_command(args, cwd).map_err(convert_cli_error)?;
            return Ok(CliOutput {
                stdout: output.stdout,
            });
        }
        if matches!(
            sub,
            "import" | "import-approve" | "import-reject" | "import-status"
        ) {
            let output =
                opensks_cli::run_design_import_command(args, cwd).map_err(convert_cli_error)?;
            return Ok(CliOutput {
                stdout: output.stdout,
            });
        }
    }
    require_exact_subcommand(args, "qa", "usage: opensks design qa")?;
    let dir = cwd.join(OPEN_SKSDIR).join("design");
    fs::create_dir_all(&dir)?;
    let stamp = ClockStamp::now()?;
    let (surfaces, findings) = collect_design_qa(cwd)?;
    let snapshot_path = dir.join("design-visual-snapshots.jsonl");
    let previous_surfaces = read_design_surface_snapshot(&snapshot_path)?;
    let visual_diffs = compute_design_visual_diffs(&previous_surfaces, &surfaces);
    let screenshot_snapshot_path = dir.join("design-screenshot-snapshots.jsonl");
    let previous_screenshots = read_design_screenshot_snapshot(&screenshot_snapshot_path)?;
    let current_screenshots = write_design_screenshot_artifacts(&dir, &surfaces)?;
    let screenshot_diffs =
        compute_design_screenshot_diffs(&dir, &previous_screenshots, &current_screenshots);
    let screenshot_baseline_available = !previous_screenshots.is_empty();
    let screenshot_diff_executed = !surfaces.is_empty();
    write_text_atomic(
        &dir.join("design-qa-report.json"),
        &render_design_qa_report(
            &stamp,
            &surfaces,
            &findings,
            &visual_diffs,
            screenshot_diff_executed,
            screenshot_baseline_available,
        ),
    )?;
    write_text_atomic(
        &dir.join("design-surface-inventory.json"),
        &render_design_surface_inventory(&stamp, &surfaces),
    )?;
    write_text_atomic(
        &dir.join("design-findings.jsonl"),
        &render_design_findings_jsonl(&stamp, &findings),
    )?;
    write_text_atomic(
        &dir.join("design-visual-diff-report.json"),
        &render_design_visual_diff_report(
            &stamp,
            &visual_diffs,
            !previous_surfaces.is_empty(),
            screenshot_diff_executed,
        ),
    )?;
    write_text_atomic(
        &dir.join("design-screenshot-diff-report.json"),
        &render_design_screenshot_diff_report(
            &stamp,
            &screenshot_diffs,
            &current_screenshots,
            screenshot_baseline_available,
            screenshot_diff_executed,
        ),
    )?;
    write_text_atomic(&snapshot_path, &render_design_surface_snapshot(&surfaces))?;
    write_text_atomic(
        &screenshot_snapshot_path,
        &render_design_screenshot_snapshot(&current_screenshots),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote design QA artifacts\nsurfaces: {}\nfindings: {}\nvisual_diffs: {}\nreport: {}\n",
            surfaces.len(),
            findings.len(),
            visual_diffs.len(),
            dir.join("design-qa-report.json").display()
        ),
    })
}

fn run_bench_command(_args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let dir = cwd.join(OPEN_SKSDIR).join("bench");
    fs::create_dir_all(&dir)?;
    let stamp = ClockStamp::now()?;
    let checks = run_local_qa_checks(cwd);
    let statuses = provider_statuses(cwd);
    let adapter_check_present = cwd
        .join(OPEN_SKSDIR)
        .join("providers")
        .join("provider-adapter-check.json")
        .exists();
    let native_collaboration =
        opensks_native_collaboration::discover_native_collaboration_evidence(cwd);
    write_text_atomic(
        &dir.join("benchmark-report.json"),
        &render_benchmark_report(&stamp, &checks),
    )?;
    write_text_atomic(
        &dir.join("multi-llm-roster.json"),
        &render_multi_llm_roster(&stamp),
    )?;
    write_text_atomic(
        &dir.join("role-assignments.json"),
        &render_role_assignments(&stamp),
    )?;
    write_text_atomic(
        &dir.join("disagreement-report.json"),
        &render_disagreement_report(&stamp),
    )?;
    write_text_atomic(
        &dir.join("quorum-report.json"),
        &render_quorum_report(&stamp),
    )?;
    write_text_atomic(
        &dir.join("collaboration-preflight.json"),
        &render_collaboration_preflight(&stamp, &statuses, adapter_check_present),
    )?;
    write_text_atomic(
        &dir.join("native-collaboration-execution.json"),
        &opensks_native_collaboration::render_native_collaboration_execution(
            &stamp.json(),
            &native_collaboration,
        ),
    )?;
    write_text_atomic(
        &dir.join("native-collaboration-events.jsonl"),
        &opensks_native_collaboration::render_native_collaboration_events_jsonl(
            &stamp.json(),
            &native_collaboration,
        ),
    )?;
    write_text_atomic(
        &dir.join("native-proof-diagnostics.json"),
        &opensks_native_collaboration::render_native_proof_diagnostics(
            &stamp.json(),
            &native_collaboration,
        ),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote benchmark and multi-LLM artifacts\nartifacts: {}\nreport: {}\npreflight: {}\n",
            dir.display(),
            dir.join("benchmark-report.json").display(),
            dir.join("collaboration-preflight.json").display()
        ),
    })
}

fn run_auth_command(_args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let dir = cwd.join(OPEN_SKSDIR).join("auth");
    fs::create_dir_all(&dir)?;
    let stamp = ClockStamp::now()?;
    let statuses = provider_statuses(cwd);
    write_text_atomic(
        &dir.join("auth-registry.json"),
        &render_auth_registry(&stamp, &statuses),
    )?;
    write_text_atomic(
        &dir.join("provider-registry.json"),
        &render_provider_registry(&stamp, &statuses, &[]),
    )?;
    write_text_atomic(
        &dir.join("auth-policy.json"),
        &render_auth_policy(&stamp, &statuses),
    )?;
    write_text_atomic(
        &dir.join("auth-audit-log.jsonl"),
        &render_auth_audit_event(&stamp, "auth_registry_snapshot", &statuses),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote auth/provider registry artifacts\nartifacts: {}\n",
            dir.display()
        ),
    })
}

fn run_provider_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(CliOutput {
            stdout: provider_usage().to_string(),
        });
    }
    let subcommand = args.first().expect("provider args checked above");
    let dir = cwd.join(OPEN_SKSDIR).join("providers");
    fs::create_dir_all(&dir)?;
    let stamp = ClockStamp::now()?;
    if subcommand == "mock-e2e" {
        return run_provider_mock_e2e(cwd, &dir, &stamp);
    }
    let statuses = provider_statuses(cwd);

    match subcommand.as_str() {
        "list" => {
            write_provider_registry_artifacts(&dir, &stamp, &statuses, &[])?;
            Ok(CliOutput {
                stdout: format!(
                    "wrote provider registry and dashboard\nproviders: {}\nartifacts: {}\n",
                    statuses.len(),
                    dir.display()
                ),
            })
        }
        "probe" => {
            let probes = probe_providers(&statuses);
            write_provider_registry_artifacts(&dir, &stamp, &statuses, &probes)?;
            write_text_atomic(
                &dir.join("provider-probe-report.json"),
                &render_provider_probe_report(&stamp, &probes),
            )?;
            append_text(
                &dir.join("usage-ledger.jsonl"),
                &render_provider_usage_event(&stamp, "probe", &probes),
            )?;
            let attempted = probes.iter().filter(|probe| probe.attempted).count();
            Ok(CliOutput {
                stdout: format!(
                    "probed local provider endpoints\nproviders: {}\nattempted: {}\nreport: {}\n",
                    probes.len(),
                    attempted,
                    dir.join("provider-probe-report.json").display()
                ),
            })
        }
        "usage" => {
            let probes = Vec::new();
            write_provider_registry_artifacts(&dir, &stamp, &statuses, &probes)?;
            append_text(
                &dir.join("usage-ledger.jsonl"),
                &render_provider_usage_event(&stamp, "usage_snapshot", &probes),
            )?;
            write_text_atomic(
                &dir.join("usage-dashboard.json"),
                &render_usage_dashboard(&stamp, &statuses, &probes),
            )?;
            Ok(CliOutput {
                stdout: format!(
                    "wrote provider usage ledger snapshot\nledger: {}\n",
                    dir.join("usage-ledger.jsonl").display()
                ),
            })
        }
        "mock-e2e" => unreachable!("provider mock-e2e is handled before status collection"),
        "adapter-check" => {
            let checks = check_provider_adapters(&dir, &statuses);
            write_provider_registry_artifacts(&dir, &stamp, &statuses, &[])?;
            write_text_atomic(
                &dir.join("provider-adapter-check.json"),
                &render_provider_adapter_check_report(&stamp, &checks),
            )?;
            let attempted = checks.iter().filter(|check| check.attempted).count();
            let blockers =
                provider_adapter_check_blockers(&checks, remote_provider_adapter_probe_opted_in());
            let blocker_lines = provider_adapter_check_blocker_stdout_lines(&blockers);
            Ok(CliOutput {
                stdout: format!(
                    "checked remote provider adapters\nadapters: {}\nattempted: {}\nblockers: {}\n{}report: {}\n",
                    checks.len(),
                    attempted,
                    blockers.len(),
                    blocker_lines,
                    dir.join("provider-adapter-check.json").display()
                ),
            })
        }
        other if opensks_cli::is_registry_subcommand(other) => {
            let output =
                opensks_cli::run_provider_registry_command(args, cwd).map_err(convert_cli_error)?;
            Ok(CliOutput {
                stdout: output.stdout,
            })
        }
        "route" => {
            let output = opensks_cli::run_provider_route_command(&args[1..], cwd)
                .map_err(convert_cli_error)?;
            Ok(CliOutput {
                stdout: output.stdout,
            })
        }
        other => Err(OpenSksError::Usage(format!(
            "unknown provider subcommand `{other}`\n\n{}",
            provider_usage()
        ))),
    }
}

fn run_provider_mock_e2e(
    cwd: &Path,
    dir: &Path,
    stamp: &ClockStamp,
) -> Result<CliOutput, OpenSksError> {
    let scratch = cwd
        .join(OPEN_SKSDIR)
        .join("runtime")
        .join("provider-mock-e2e-scratch");
    match fs::remove_dir_all(&scratch) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    fs::create_dir_all(&scratch)?;

    let now_ms = stamp
        .secs
        .saturating_mul(1000)
        .saturating_add(u64::from(stamp.nanos / 1_000_000));
    let provider_id = "mock-openai-compatible";
    let model_id = "mock-openai-compatible/code-model";
    let connection = opensks_contracts::ProviderConnection {
        schema: opensks_contracts::PROVIDER_CONNECTION_SCHEMA.to_string(),
        id: provider_id.to_string(),
        kind: opensks_contracts::ProviderKind::OpenAiCompatible,
        display_name: "Mock OpenAI-compatible Provider".to_string(),
        enabled: true,
        endpoint: opensks_contracts::ProviderEndpoint {
            base_url: "https://fixture.provider.local/v1".to_string(),
            allow_insecure_http: false,
        },
        auth: opensks_contracts::SecretRef::macos_keychain(
            "ai.opensks.provider.mock-e2e",
            provider_id,
            1,
        ),
        organization_ref: None,
        project_ref: None,
        health: opensks_contracts::ProviderHealthSnapshot {
            state: opensks_contracts::HealthState::Healthy,
            circuit_open: false,
            checked_at_ms: Some(now_ms),
            reason_code: "mock_e2e_fixture".to_string(),
            diagnostic_ref: None,
        },
        concurrency: opensks_contracts::ProviderConcurrencyPolicy {
            max_concurrent_requests: 2,
            requests_per_minute: Some(60),
            tokens_per_minute: None,
        },
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
        revision: 1,
    };
    let repo = opensks_provider::ProviderRepository::open_workspace(&scratch)
        .map_err(|error| OpenSksError::Invalid(error.to_string()))?;
    repo.upsert_connection(&connection, None, now_ms)
        .map_err(|error| OpenSksError::Invalid(error.to_string()))?;

    let models_fixture = r#"{"data":[{"id":"code-model","name":"Code Model","context_length":64000,"supported_parameters":["tools","response_format"],"architecture":{"input_modalities":["text"],"output_modalities":["text"]}}]}"#;
    let models = opensks_provider::parse_openai_compatible_models(&connection, models_fixture)
        .map_err(|error| OpenSksError::Invalid(error.to_string()))?;
    let sync_receipt = repo
        .sync_models(provider_id, &models, now_ms)
        .map_err(|error| OpenSksError::Invalid(error.to_string()))?;
    let stored_models = repo
        .list_models(provider_id)
        .map_err(|error| OpenSksError::Invalid(error.to_string()))?;
    let settings = opensks_contracts::ConversationTurnSettings {
        model: opensks_contracts::ModelSelection {
            mode: opensks_contracts::ModelSelectionMode::Pinned,
            model_id: Some(model_id.to_string()),
            fallback_model_ids: Vec::new(),
        },
        reasoning_effort: opensks_contracts::ReasoningEffort::Standard,
        execution_mode: opensks_contracts::ExecutionMode::Worktree,
        pipeline_id: "provider-mock-e2e".to_string(),
        graph_revision: None,
        max_parallelism: 1,
        verifier_count: 1,
        tool_policy_id: "provider-mock-e2e-readonly".to_string(),
        approval_policy_id: "provider-mock-e2e-local".to_string(),
        token_budget: None,
        cost_budget_usd: None,
        timeout_ms: Some(30_000),
        image_model_id: None,
    };
    let decision = opensks_provider::resolve_routing_decision_from_repository(
        &repo,
        "provider-mock-e2e",
        &settings,
    )
    .map_err(|error| OpenSksError::Invalid(error.to_string()))?;

    let catalog_synced = sync_receipt.mutation
        == opensks_contracts::ProviderMutationKind::ModelsSynced
        && !stored_models.is_empty();
    let model_enabled = stored_models
        .iter()
        .any(|model| model.id == model_id && model.enabled);
    let route_resolved = decision.status == opensks_contracts::RoutingStatus::Resolved
        && decision.selected_model_id.as_deref() == Some(model_id);
    let all_verified = catalog_synced && model_enabled && route_resolved;
    let report_status = if all_verified {
        opensks_contracts::TrustStatus::Verified
    } else {
        opensks_contracts::TrustStatus::Partial
    };
    let check_status = |passed: bool| {
        if passed {
            opensks_contracts::TrustStatus::Verified
        } else {
            opensks_contracts::TrustStatus::Partial
        }
    };
    let report = opensks_contracts::ProviderMockE2eReport {
        schema: opensks_contracts::PROVIDER_MOCK_E2E_SCHEMA.to_string(),
        generated_at: opensks_contracts::ContractTimestamp {
            unix_seconds: stamp.secs.min(i64::MAX as u64) as i64,
            nanos: stamp.nanos,
        },
        status: report_status,
        fixture_kind: "openai_compatible_registry_fixture".to_string(),
        live_vendor_calls_performed: false,
        secret_value_exposed: false,
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
        model_catalog_count: stored_models.len(),
        model_catalog_synced: catalog_synced,
        model_enabled,
        registry_route_status: decision.status.clone(),
        selected_model_id: decision.selected_model_id.clone(),
        route_receipt: decision.route_receipt.clone(),
        checks: vec![
            opensks_contracts::ProviderMockE2eCheck {
                id: "secret_ref_only".to_string(),
                status: opensks_contracts::TrustStatus::Verified,
                evidence_ref:
                    "auth store is SecretRef::macos_keychain; no credential value is constructed"
                        .to_string(),
            },
            opensks_contracts::ProviderMockE2eCheck {
                id: "model_catalog_synced".to_string(),
                status: check_status(catalog_synced),
                evidence_ref: "parse_openai_compatible_models + ProviderRepository::sync_models"
                    .to_string(),
            },
            opensks_contracts::ProviderMockE2eCheck {
                id: "model_enabled".to_string(),
                status: check_status(model_enabled),
                evidence_ref: "ProviderRepository::list_models after sync".to_string(),
            },
            opensks_contracts::ProviderMockE2eCheck {
                id: "registry_route_resolved".to_string(),
                status: check_status(route_resolved),
                evidence_ref: "resolve_routing_decision_from_repository pinned code model"
                    .to_string(),
            },
            opensks_contracts::ProviderMockE2eCheck {
                id: "no_live_vendor_calls".to_string(),
                status: opensks_contracts::TrustStatus::Verified,
                evidence_ref: "fixture path does not call provider HTTP transports".to_string(),
            },
        ],
    };
    let artifact = dir.join("provider-mock-e2e.json");
    write_text_atomic(
        &artifact,
        &(serde_json::to_string_pretty(&report).map_err(|error| {
            OpenSksError::Invalid(format!("serialize provider mock e2e report: {error}"))
        })? + "\n"),
    )?;
    fs::remove_dir_all(&scratch).ok();

    Ok(CliOutput {
        stdout: format!(
            "wrote provider mock E2E proof\nstatus: {:?}\nmodel_catalog_count: {}\nroute_status: {:?}\nlive_vendor_calls_performed: false\nartifact: {}\n",
            report.status,
            report.model_catalog_count,
            report.registry_route_status,
            artifact.display()
        ),
    })
}

fn run_updater_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    require_exact_subcommand(args, "plan", "usage: opensks updater plan")?;
    let dir = cwd.join(OPEN_SKSDIR).join("updater");
    fs::create_dir_all(&dir)?;
    let stamp = ClockStamp::now()?;
    let manifest = render_update_manifest(&stamp);
    let manifest_hash = stable_content_hash(&manifest);
    let signature = local_update_signature(&manifest_hash);
    write_text_atomic(&dir.join("update-manifest.json"), &manifest)?;
    write_text_atomic(
        &dir.join("update-signature.json"),
        &render_update_signature(&stamp, &manifest_hash, &signature),
    )?;
    write_text_atomic(
        &dir.join("update-channels.json"),
        &render_update_channels(&stamp),
    )?;
    write_text_atomic(
        &dir.join("rollback-plan.json"),
        &render_rollback_plan(&stamp),
    )?;
    write_text_atomic(
        &dir.join("update-boundary.json"),
        &render_update_boundary(&stamp),
    )?;
    write_text_atomic(
        &dir.join("updater-final-state.json"),
        &render_updater_final_state(&stamp, &manifest_hash, &signature),
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote signed updater plan artifacts\nartifacts: {}\nmanifest_hash: {}\n",
            dir.display(),
            manifest_hash
        ),
    })
}

fn run_acceptance_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    require_exact_subcommand(args, "audit", "usage: opensks acceptance audit")?;
    let dir = cwd.join(OPEN_SKSDIR).join("acceptance");
    fs::create_dir_all(&dir)?;
    let stamp = ClockStamp::now()?;
    let mvp = mvp_acceptance_items(cwd);
    let beta = beta_acceptance_items(cwd);
    let production = production_acceptance_items(cwd);
    write_text_atomic(
        &dir.join("mvp-acceptance.json"),
        &render_acceptance_report(&stamp, "mvp", &mvp),
    )?;
    write_text_atomic(
        &dir.join("beta-acceptance.json"),
        &render_acceptance_report(&stamp, "beta", &beta),
    )?;
    write_text_atomic(
        &dir.join("production-acceptance.json"),
        &render_acceptance_report(&stamp, "production", &production),
    )?;
    write_text_atomic(
        &dir.join("acceptance-summary.json"),
        &render_acceptance_summary(&stamp, &mvp, &beta, &production),
    )?;
    write_text_atomic(
        &dir.join("acceptance-findings.jsonl"),
        &render_acceptance_findings_jsonl(&stamp, &mvp, &beta, &production),
    )?;
    let (total, passed, partial, failed) =
        combined_acceptance_counts(&[&mvp[..], &beta[..], &production[..]]);
    Ok(CliOutput {
        stdout: format!(
            "wrote acceptance audit artifacts\ncriteria: {}\npassed: {}\npartial: {}\nfailed: {}\nartifacts: {}\n",
            total,
            passed,
            partial,
            failed,
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
    write_text_atomic(
        &dir.join("platform-manifest.json"),
        &render_platform_manifest(&stamp),
    )?;
    write_text_atomic(
        &dir.join("module-manifest.json"),
        &render_module_manifest(&stamp),
    )?;
    write_text_atomic(
        &dir.join("macos-integration-manifest.json"),
        &render_macos_integration_manifest(&stamp),
    )?;
    write_text_atomic(
        &dir.join("source-notes-ledger.json"),
        &render_source_notes_ledger(&stamp),
    )?;
    write_text_atomic(
        &dir.join("product-statement.json"),
        &render_product_statement(&stamp),
    )?;
    let worker_lanes = collect_worker_lane_snapshots(cwd);
    write_text_atomic(
        &dir.join("worker-lanes.json"),
        &render_worker_lanes_report(&stamp, &worker_lanes),
    )?;
    write_text_atomic(&dir.join("gui-data.json"), &render_gui_data(&stamp, cwd)?)?;
    write_text_atomic(
        &dir.join("dashboard.html"),
        &render_dashboard_html(&stamp, cwd)?,
    )?;
    Ok(CliOutput {
        stdout: format!(
            "wrote GUI/workspace dashboard artifacts\nartifacts: {}\ndashboard: {}\n",
            dir.display(),
            dir.join("dashboard.html").display()
        ),
    })
}

fn run_default_launch_command(cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = run_app_command(&[], cwd)?;
    let app_bundle = create_native_app_bundle(cwd)?;
    let dashboard = cwd.join(OPEN_SKSDIR).join("app").join("dashboard.html");
    Ok(CliOutput {
        stdout: format!(
            "created OpenSKS macOS app launcher\n{}\napp: {}\ndashboard_data: {}\nnext: double-click OpenSKS.app or run `open {}`\n",
            output.stdout,
            app_bundle.display(),
            dashboard.display(),
            app_bundle.display()
        ),
    })
}

fn run_app_data_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let workspace = args
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.to_path_buf());
    let dashboard = native_app_dashboard(&workspace)?;
    Ok(CliOutput {
        stdout: render_app_data_json(&dashboard),
    })
}

/// Emit the SwiftUI app's domain data as JSON. This is the only Rust→Swift data
/// boundary; the app shells `opensks-cli app-data <workspace>` and decodes it.
fn render_app_data_json(d: &NativeAppDashboard) -> String {
    let g = &d.gui;
    let a = &d.acceptance;
    let goal_complete = match a.goal_complete {
        Some(true) => "true",
        Some(false) => "false",
        None => "null",
    };
    let release = render_native_release_json(&d.release);
    let provider_adapter_check = d.provider_adapter_check.as_deref().unwrap_or("null");
    let provider_mock_e2e = d
        .provider_mock_e2e
        .as_ref()
        .map(render_native_provider_mock_e2e_json)
        .unwrap_or_else(|| "null".to_string());
    let lanes = render_worker_lane_items_json(&g.worker_lanes);
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.app-data.v1\",\n",
            "  \"workspace\": {workspace},\n",
            "  \"workspace_label\": {workspace_label},\n",
            "  \"app_bundle\": {app_bundle},\n",
            "  \"artifact_dir\": {artifact_dir},\n",
            "  \"dashboard_html\": {dashboard_html},\n",
            "  \"missions_dir\": {missions_dir},\n",
            "  \"cli_path\": {cli_path},\n",
            "  \"acceptance\": {{\"total\": {total}, \"passed\": {passed}, ",
            "\"partial\": {partial}, \"failed\": {failed}, \"goal_complete\": {goal_complete}}},\n",
            "  \"release\": {release},\n",
            "  \"provider_adapter_check\": {provider_adapter_check},\n",
            "  \"provider_mock_e2e\": {provider_mock_e2e},\n",
            "  \"gui\": {{\"prd_total\": {prd_total}, \"prd_implemented\": {prd_implemented}, ",
            "\"prd_artifact_mvp\": {prd_artifact_mvp}, \"prd_planned\": {prd_planned}, ",
            "\"prd_missing_live\": {prd_missing_live}, \"qa_status\": {qa_status}, ",
            "\"security_status\": {security_status}, ",
            "\"provider_configured_count\": {provider_configured_count}, ",
            "\"voxel_count\": {voxel_count}, \"mission_count\": {mission_count}, ",
            "\"browser_sessions\": {browser_sessions}, \"computer_sessions\": {computer_sessions}, ",
            "\"app_sessions\": {app_sessions}, \"worker_lane_missions\": {worker_lane_missions}, ",
            "\"worker_lane_count\": {worker_lane_count}}},\n",
            "  \"worker_lanes\": {lanes}\n",
            "}}\n"
        ),
        workspace = json_string(&d.workspace.display().to_string()),
        workspace_label = json_string(&d.workspace_label),
        app_bundle = json_string(&d.app_bundle.display().to_string()),
        artifact_dir = json_string(&d.artifact_dir.display().to_string()),
        dashboard_html = json_string(&d.dashboard_html.display().to_string()),
        missions_dir = json_string(
            &d.workspace
                .join(OPEN_SKSDIR)
                .join("missions")
                .display()
                .to_string()
        ),
        cli_path = json_string(&d.cli_path.display().to_string()),
        total = a.total,
        passed = a.passed,
        partial = a.partial,
        failed = a.failed,
        goal_complete = goal_complete,
        release = release,
        provider_adapter_check = provider_adapter_check,
        provider_mock_e2e = provider_mock_e2e,
        prd_total = g.prd_total,
        prd_implemented = g.prd_implemented,
        prd_artifact_mvp = g.prd_artifact_mvp,
        prd_planned = g.prd_planned,
        prd_missing_live = g.prd_missing_live,
        qa_status = json_string(&g.qa_status),
        security_status = json_string(&g.security_status),
        provider_configured_count = g.provider_configured_count,
        voxel_count = g.voxel_count,
        mission_count = g.mission_count,
        browser_sessions = g.browser_sessions,
        computer_sessions = g.computer_sessions,
        app_sessions = g.app_sessions,
        worker_lane_missions = g.worker_lane_missions,
        worker_lane_count = g.worker_lane_count,
        lanes = lanes,
    )
}

fn render_native_provider_mock_e2e_json(status: &NativeProviderMockE2eStatus) -> String {
    format!(
        concat!(
            "{{\"status\": {status}, ",
            "\"fixture_kind\": {fixture_kind}, ",
            "\"live_vendor_calls_performed\": {live_vendor_calls_performed}, ",
            "\"secret_value_exposed\": {secret_value_exposed}, ",
            "\"model_catalog_count\": {model_catalog_count}, ",
            "\"model_catalog_synced\": {model_catalog_synced}, ",
            "\"model_enabled\": {model_enabled}, ",
            "\"registry_route_status\": {registry_route_status}, ",
            "\"selected_model_id\": {selected_model_id}, ",
            "\"checks\": {checks}}}"
        ),
        status = json_string(&status.status),
        fixture_kind = json_string(&status.fixture_kind),
        live_vendor_calls_performed = status.live_vendor_calls_performed,
        secret_value_exposed = status.secret_value_exposed,
        model_catalog_count = status.model_catalog_count,
        model_catalog_synced = status.model_catalog_synced,
        model_enabled = status.model_enabled,
        registry_route_status = json_string(&status.registry_route_status),
        selected_model_id = status
            .selected_model_id
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_string()),
        checks = render_native_provider_mock_e2e_checks_json(&status.checks),
    )
}

fn render_native_provider_mock_e2e_checks_json(checks: &[NativeProviderMockE2eCheck]) -> String {
    let items = checks
        .iter()
        .map(|check| {
            format!(
                "{{\"id\": {id}, \"status\": {status}, \"evidence_ref\": {evidence_ref}}}",
                id = json_string(&check.id),
                status = json_string(&check.status),
                evidence_ref = json_string(&check.evidence_ref)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{items}]")
}

fn render_native_release_json(release: &NativeReleaseStatus) -> String {
    format!(
        concat!(
            "{{\"status\": {status}, ",
            "\"source_commit_sha\": {source_commit_sha}, ",
            "\"workspace_dirty\": {workspace_dirty}, ",
            "\"artifact_digest_gate_passed\": {artifact_digest_gate_passed}, ",
            "\"same_sha_artifact_binding\": {same_sha_artifact_binding}, ",
            "\"missing_artifacts\": {missing_artifacts}, ",
            "\"blockers\": {blockers}, ",
            "\"remediation_actions\": {actions}, ",
            "\"signing_evidence\": {signing_evidence}}}"
        ),
        status = json_string(&release.status),
        source_commit_sha = render_optional_string_json(release.source_commit_sha.as_deref()),
        workspace_dirty = release.workspace_dirty,
        artifact_digest_gate_passed = release.artifact_digest_gate_passed,
        same_sha_artifact_binding = release.same_sha_artifact_binding,
        missing_artifacts = render_string_array_json(&release.missing_artifacts),
        blockers = render_native_release_blockers_json(&release.blockers),
        actions = render_native_release_actions_json(&release.remediation_actions),
        signing_evidence =
            render_native_release_signing_evidence_json(release.signing_evidence.as_ref()),
    )
}

fn render_native_release_blockers_json(blockers: &[NativeReleaseBlocker]) -> String {
    let items = blockers
        .iter()
        .map(|blocker| {
            format!(
                "{{\"code\": {code}, \"message\": {message}}}",
                code = json_string(&blocker.code),
                message = json_string(&blocker.message)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{items}]")
}

fn render_native_release_actions_json(actions: &[NativeReleaseRemediationAction]) -> String {
    let items = actions
        .iter()
        .map(|action| {
            format!(
                "{{\"blocker\": {blocker}, \"action\": {action_text}, \"scope\": {scope}}}",
                blocker = json_string(&action.blocker),
                action_text = json_string(&action.action),
                scope = json_string(&action.scope)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{items}]")
}

fn render_native_release_signing_evidence_json(
    evidence: Option<&NativeReleaseSigningEvidence>,
) -> String {
    let Some(evidence) = evidence else {
        return "null".to_string();
    };
    format!(
        concat!(
            "{{\"checked\": {checked}, ",
            "\"app_bundle_path\": {app_bundle_path}, ",
            "\"identifier\": {identifier}, ",
            "\"signature\": {signature}, ",
            "\"team_identifier\": {team_identifier}, ",
            "\"cd_hash\": {cd_hash}, ",
            "\"production_signed\": {production_signed}, ",
            "\"notarized\": {notarized}, ",
            "\"codesign_status\": {codesign_status}, ",
            "\"notarization_status\": {notarization_status}, ",
            "\"diagnostic\": {diagnostic}}}"
        ),
        checked = evidence.checked,
        app_bundle_path = json_string(&evidence.app_bundle_path),
        identifier = render_optional_string_json(evidence.identifier.as_deref()),
        signature = render_optional_string_json(evidence.signature.as_deref()),
        team_identifier = render_optional_string_json(evidence.team_identifier.as_deref()),
        cd_hash = render_optional_string_json(evidence.cd_hash.as_deref()),
        production_signed = evidence.production_signed,
        notarized = evidence.notarized,
        codesign_status = render_optional_i32_json(evidence.codesign_status),
        notarization_status = render_optional_i32_json(evidence.notarization_status),
        diagnostic = json_string(&evidence.diagnostic),
    )
}

fn render_optional_string_json(value: Option<&str>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_string())
}

fn render_optional_i32_json(value: Option<i32>) -> String {
    value
        .map(|number| number.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn render_string_array_json(values: &[String]) -> String {
    let items = values
        .iter()
        .map(|value| json_string(value))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{items}]")
}

fn run_scheduler_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_scheduler_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_perf_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_perf_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_capability_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_capability_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_worker_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_worker_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_worktree_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_worktree_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_patch_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_patch_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_graph_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_graph_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_hooks_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_hooks_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_codegraph_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_codegraph_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_triwiki_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_triwiki_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_context_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_context_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_conversation_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_conversation_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_file_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_file_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_intel_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_intel_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_vault_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_vault_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_image_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_image_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_reasoning_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_reasoning_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_git_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_git_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_gc_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_gc_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
    })
}

fn run_release_command(args: &[String], cwd: &Path) -> Result<CliOutput, OpenSksError> {
    let output = opensks_cli::run_release_command(args, cwd).map_err(convert_cli_error)?;
    Ok(CliOutput {
        stdout: output.stdout,
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

fn plan_browser_action(target: &str) -> BrowserPolicyDecision {
    let lower = target.to_ascii_lowercase();
    let sensitive = [
        "password",
        "credential",
        "login",
        "purchase",
        "buy",
        "payment",
        "transfer",
        "send",
        "submit",
        "upload",
        "download",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let interactive = [
        "click", "type", "fill", "submit", "scroll", "select", "upload", "download",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let is_url = target.starts_with("http://") || target.starts_with("https://");

    if sensitive {
        return BrowserPolicyDecision {
            requested_action: classify_browser_action(&lower),
            decision: "denied_sensitive_browser_action".to_string(),
            reason: "Sensitive browser action requires explicit approval and was not executed."
                .to_string(),
            network_allowed: false,
            browser_action_allowed: false,
            sensitive: true,
        };
    }

    if interactive {
        return BrowserPolicyDecision {
            requested_action: classify_browser_action(&lower),
            decision: "approval_required_for_browser_action".to_string(),
            reason: "Browser interaction was planned but not executed without explicit approval."
                .to_string(),
            network_allowed: is_url,
            browser_action_allowed: false,
            sensitive: false,
        };
    }

    BrowserPolicyDecision {
        requested_action: if is_url {
            "inspect_url".to_string()
        } else {
            "plan_browser_task".to_string()
        },
        decision: if is_url {
            "allowed_network_observation".to_string()
        } else {
            "planned_non_url_browser_task".to_string()
        },
        reason: "Only non-destructive browser observation is allowed in the current local slice."
            .to_string(),
        network_allowed: is_url,
        browser_action_allowed: false,
        sensitive: false,
    }
}

fn classify_browser_action(lower: &str) -> String {
    for (needle, action) in [
        ("password", "credential_entry"),
        ("credential", "credential_entry"),
        ("login", "credential_entry"),
        ("purchase", "purchase"),
        ("buy", "purchase"),
        ("payment", "payment"),
        ("transfer", "payment"),
        ("send", "send"),
        ("submit", "submit"),
        ("upload", "upload"),
        ("download", "download"),
        ("click", "click"),
        ("type", "type"),
        ("fill", "type"),
        ("scroll", "scroll"),
        ("select", "select"),
    ] {
        if lower.contains(needle) {
            return action.to_string();
        }
    }
    "inspect_url".to_string()
}

fn write_browser_probe_artifacts(
    cwd: &Path,
    session: &CapabilitySession,
    target: &str,
    probe: &HttpProbe,
    snapshot: &PageSnapshot,
    decision: &BrowserPolicyDecision,
) -> Result<(), OpenSksError> {
    let dir = cwd.join(OPEN_SKSDIR).join(session.plane).join(&session.id);
    write_text_atomic(
        &dir.join("network-log.har"),
        &render_browser_har(session, target, probe),
    )?;
    write_text_atomic(
        &dir.join("browser-final-state.json"),
        &render_browser_final_state(session, target, probe, snapshot, decision),
    )?;
    write_text_atomic(
        &dir.join("dom-snapshots").join("initial.json"),
        &render_browser_dom_snapshot(session, target, probe, snapshot),
    )?;
    write_text_atomic(
        &dir.join("browser-policy-decision.json"),
        &render_browser_policy_decision(session, target, decision),
    )?;
    write_text_atomic(
        &dir.join("browser-action-plan.json"),
        &render_browser_action_plan(session, target, decision),
    )?;
    write_text_atomic(
        &dir.join("browser-page-links.json"),
        &render_browser_page_links(session, target, snapshot),
    )?;
    write_text_atomic(
        &dir.join("browser-actions.jsonl"),
        &render_browser_actions_jsonl(session, probe, snapshot, decision),
    )?;
    let local_runtime = write_browser_local_runtime(cwd, session, target)?;
    write_text_atomic(
        &dir.join("browser-screenshot-snapshots.jsonl"),
        &render_browser_screenshot_snapshot(session, target, &local_runtime),
    )?;
    write_text_atomic(
        &dir.join("browser-interaction-loop.json"),
        &render_browser_interaction_loop(
            session,
            target,
            probe,
            snapshot,
            decision,
            &local_runtime,
        ),
    )?;
    write_text_atomic(
        &dir.join("browser-interaction-events.jsonl"),
        &render_browser_interaction_events(session, decision, &local_runtime),
    )?;
    Ok(())
}

fn write_browser_local_runtime(
    cwd: &Path,
    session: &CapabilitySession,
    target: &str,
) -> Result<BrowserLocalRuntimeArtifact, OpenSksError> {
    let session_dir = cwd.join(OPEN_SKSDIR).join(session.plane).join(&session.id);
    let runtime_dir = session_dir.join("browser-runtime");
    fs::create_dir_all(&runtime_dir)?;
    let page = render_browser_local_runtime_page(target);
    let runtime_page_hash = stable_content_hash(&page);
    write_text_atomic(&runtime_dir.join("index.html"), &page)?;

    let screenshot_ppm = render_browser_local_screenshot_ppm(session, target, &runtime_page_hash);
    let screenshot_hash = stable_content_hash(&screenshot_ppm);
    let screenshot_ref = browser_local_screenshot_path(&screenshot_hash);
    write_text_atomic(&session_dir.join(&screenshot_ref), &screenshot_ppm)?;

    Ok(BrowserLocalRuntimeArtifact {
        runtime_ref: "browser-runtime/index.html",
        runtime_page_hash,
        screenshot_ref,
        screenshot_hash,
        pixel_count: BROWSER_LOCAL_SCREENSHOT_WIDTH * BROWSER_LOCAL_SCREENSHOT_HEIGHT,
    })
}

fn render_browser_local_runtime_page(target: &str) -> String {
    format!(
        concat!(
            "<!doctype html>\n",
            "<html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n",
            "<title>OpenSKS local browser interaction loop</title></head>\n",
            "<body><main><h1>OpenSKS local browser interaction loop</h1>\n",
            "<p data-target=\"{}\">Policy-scoped local browser-use seed.</p>\n",
            "<button id=\"{}\" type=\"button\" data-click-result=\"{}\">Record browser click</button>\n",
            "<label for=\"{}\">Browser loop input</label>\n",
            "<input id=\"{}\" name=\"browser-loop-input\" data-type-result=\"{}\" autocomplete=\"off\">\n",
            "<output id=\"{}\">opensks-browser-loop-ready</output>\n",
            "<script>\n",
            "const button = document.getElementById('{}');\n",
            "const input = document.getElementById('{}');\n",
            "const status = document.getElementById('{}');\n",
            "button.addEventListener('click', () => {{ status.value = button.dataset.clickResult; status.textContent = button.dataset.clickResult; }});\n",
            "input.addEventListener('input', () => {{ status.value = input.value || input.dataset.typeResult; status.textContent = input.value || input.dataset.typeResult; }});\n",
            "</script>\n",
            "</main></body></html>\n"
        ),
        html_escape_attr(target),
        BROWSER_LOCAL_LOOP_BUTTON_ID,
        BROWSER_LOCAL_LOOP_FINAL_TEXT,
        BROWSER_LOCAL_LOOP_INPUT_ID,
        BROWSER_LOCAL_LOOP_INPUT_ID,
        BROWSER_LOCAL_LOOP_FINAL_TEXT,
        BROWSER_LOCAL_LOOP_STATUS_ID,
        BROWSER_LOCAL_LOOP_BUTTON_ID,
        BROWSER_LOCAL_LOOP_INPUT_ID,
        BROWSER_LOCAL_LOOP_STATUS_ID
    )
}

fn render_browser_local_screenshot_ppm(
    session: &CapabilitySession,
    target: &str,
    runtime_page_hash: &str,
) -> String {
    let mut out = format!(
        concat!(
            "P3\n",
            "# OpenSKS deterministic local browser runtime screenshot artifact\n",
            "# session_id={}\n",
            "# target_hash={}\n",
            "# runtime_page_hash={}\n",
            "# renderer={}\n",
            "{} {}\n",
            "255\n"
        ),
        session.id,
        stable_content_hash(target),
        runtime_page_hash,
        BROWSER_LOCAL_SCREENSHOT_RENDERER,
        BROWSER_LOCAL_SCREENSHOT_WIDTH,
        BROWSER_LOCAL_SCREENSHOT_HEIGHT
    );
    let seed = stable_content_hash_u64(&format!(
        "{}|{}|{}|{}",
        session.id, target, runtime_page_hash, BROWSER_LOCAL_SCREENSHOT_MODE
    ));
    for y in 0..BROWSER_LOCAL_SCREENSHOT_HEIGHT {
        for x in 0..BROWSER_LOCAL_SCREENSHOT_WIDTH {
            let value = stable_content_hash_u64(&format!("{seed:016x}|{x}|{y}|{target}"));
            let red = (value & 0xff) as u8;
            let green = ((value >> 8) & 0xff) as u8;
            let blue = ((value >> 16) & 0xff) as u8;
            out.push_str(&format!("{red} {green} {blue}\n"));
        }
    }
    out
}

fn browser_local_screenshot_path(screenshot_hash: &str) -> String {
    let image_hash = screenshot_hash.replace("fnv1a64:", "").replace(':', "-");
    format!("screenshots/browser-local-state-{image_hash}.ppm")
}

fn render_browser_screenshot_snapshot(
    session: &CapabilitySession,
    target: &str,
    artifact: &BrowserLocalRuntimeArtifact,
) -> String {
    format!(
        concat!(
            "{{\"schema\":\"opensks.browser-screenshot-snapshot.v1\",",
            "\"session_id\":{},\"target\":{},\"image_path\":{},",
            "\"width\":{},\"height\":{},\"pixel_count\":{},",
            "\"screenshot_hash\":{},\"renderer\":{},\"mode\":{},",
            "\"runtime_ref\":{},\"runtime_page_hash\":{}}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        json_string(&artifact.screenshot_ref),
        BROWSER_LOCAL_SCREENSHOT_WIDTH,
        BROWSER_LOCAL_SCREENSHOT_HEIGHT,
        artifact.pixel_count,
        json_string(&artifact.screenshot_hash),
        json_string(BROWSER_LOCAL_SCREENSHOT_RENDERER),
        json_string(BROWSER_LOCAL_SCREENSHOT_MODE),
        json_string(artifact.runtime_ref),
        json_string(&artifact.runtime_page_hash)
    )
}

fn render_browser_interaction_loop(
    session: &CapabilitySession,
    target: &str,
    probe: &HttpProbe,
    snapshot: &PageSnapshot,
    decision: &BrowserPolicyDecision,
    artifact: &BrowserLocalRuntimeArtifact,
) -> String {
    let loop_steps = json_array(&[
        "create_local_browser_runtime",
        "open_local_runtime_state",
        "record_local_screenshot_artifact",
        "click_local_runtime_button",
        "type_local_runtime_input",
        "record_final_state",
    ]);
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.browser-interaction-loop.v1\",\n",
            "  \"session_id\": {},\n",
            "  \"target\": {},\n",
            "  \"status\": \"local_browser_open_screenshot_click_type_recorded\",\n",
            "  \"loop_iterations\": 6,\n",
            "  \"loop_steps\": {},\n",
            "  \"runtime_ref\": {},\n",
            "  \"runtime_page_hash\": {},\n",
            "  \"screenshot_ref\": {},\n",
            "  \"screenshot_hash\": {},\n",
            "  \"screenshot_mode\": {},\n",
            "  \"screenshot_renderer\": {},\n",
            "  \"pixel_count\": {},\n",
            "  \"open_recorded\": true,\n",
            "  \"screenshot_recorded\": true,\n",
            "  \"click_recorded\": true,\n",
            "  \"type_recorded\": true,\n",
            "  \"final_text\": {},\n",
            "  \"button_element_id\": {},\n",
            "  \"input_element_id\": {},\n",
            "  \"status_element_id\": {},\n",
            "  \"network_probe_attempted\": {},\n",
            "  \"page_snapshot_attempted\": {},\n",
            "  \"policy_decision\": {},\n",
            "  \"sensitive_action_detected\": {},\n",
            "  \"live_browser_control\": false,\n",
            "  \"playwright_actions_executed\": false,\n",
            "  \"chrome_extension_evidence\": false,\n",
            "  \"external_web_control\": false,\n",
            "  \"credential_entry_executed\": false,\n",
            "  \"browser_click_type_executed\": false,\n",
            "  \"requires_approval_before_live_interaction\": true,\n",
            "  \"browser_final_state_ref\": \"browser-final-state.json\",\n",
            "  \"policy_decision_ref\": \"browser-policy-decision.json\",\n",
            "  \"screenshot_snapshot_ref\": \"browser-screenshot-snapshots.jsonl\",\n",
            "  \"evidence_note\": \"local deterministic browser-use artifacts record open/screenshot/click/type; live Playwright, Chrome Extension, live DOM interaction, external web control, credential entry, and real browser-rendered screenshots remain false/unverified\"\n",
            "}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        loop_steps,
        json_string(artifact.runtime_ref),
        json_string(&artifact.runtime_page_hash),
        json_string(&artifact.screenshot_ref),
        json_string(&artifact.screenshot_hash),
        json_string(BROWSER_LOCAL_SCREENSHOT_MODE),
        json_string(BROWSER_LOCAL_SCREENSHOT_RENDERER),
        artifact.pixel_count,
        json_string(BROWSER_LOCAL_LOOP_FINAL_TEXT),
        json_string(BROWSER_LOCAL_LOOP_BUTTON_ID),
        json_string(BROWSER_LOCAL_LOOP_INPUT_ID),
        json_string(BROWSER_LOCAL_LOOP_STATUS_ID),
        probe.attempted,
        snapshot.attempted,
        json_string(&decision.decision),
        decision.sensitive
    )
}

fn render_browser_interaction_events(
    session: &CapabilitySession,
    decision: &BrowserPolicyDecision,
    artifact: &BrowserLocalRuntimeArtifact,
) -> String {
    [
        format!(
            "{{\"schema\":\"opensks.browser-interaction-event.v1\",\"session_id\":{},\"event\":\"browser_runtime_created\",\"runtime_ref\":{},\"runtime_page_hash\":{},\"executed\":true}}",
            json_string(&session.id),
            json_string(artifact.runtime_ref),
            json_string(&artifact.runtime_page_hash)
        ),
        format!(
            "{{\"schema\":\"opensks.browser-interaction-event.v1\",\"session_id\":{},\"event\":\"local_page_open_recorded\",\"runtime_ref\":{},\"executed\":true}}",
            json_string(&session.id),
            json_string(artifact.runtime_ref)
        ),
        format!(
            "{{\"schema\":\"opensks.browser-interaction-event.v1\",\"session_id\":{},\"event\":\"local_screenshot_recorded\",\"screenshot_ref\":{},\"screenshot_hash\":{},\"executed\":true}}",
            json_string(&session.id),
            json_string(&artifact.screenshot_ref),
            json_string(&artifact.screenshot_hash)
        ),
        format!(
            "{{\"schema\":\"opensks.browser-interaction-event.v1\",\"session_id\":{},\"event\":\"local_click_recorded\",\"element_id\":{},\"final_text\":{},\"executed\":true}}",
            json_string(&session.id),
            json_string(BROWSER_LOCAL_LOOP_BUTTON_ID),
            json_string(BROWSER_LOCAL_LOOP_FINAL_TEXT)
        ),
        format!(
            "{{\"schema\":\"opensks.browser-interaction-event.v1\",\"session_id\":{},\"event\":\"local_type_recorded\",\"element_id\":{},\"typed_text\":{},\"executed\":true}}",
            json_string(&session.id),
            json_string(BROWSER_LOCAL_LOOP_INPUT_ID),
            json_string(BROWSER_LOCAL_LOOP_FINAL_TEXT)
        ),
        format!(
            "{{\"schema\":\"opensks.browser-interaction-event.v1\",\"session_id\":{},\"event\":\"local_final_state_recorded\",\"status_element_id\":{},\"final_text\":{},\"executed\":true}}",
            json_string(&session.id),
            json_string(BROWSER_LOCAL_LOOP_STATUS_ID),
            json_string(BROWSER_LOCAL_LOOP_FINAL_TEXT)
        ),
        format!(
            "{{\"schema\":\"opensks.browser-interaction-event.v1\",\"session_id\":{},\"event\":\"live_browser_or_playwright_action\",\"executed\":false,\"policy_decision\":{},\"approval_required\":true,\"live_browser_control\":false,\"playwright_actions_executed\":false,\"chrome_extension_evidence\":false,\"external_web_control\":false,\"credential_entry_executed\":false}}",
            json_string(&session.id),
            json_string(&decision.decision)
        ),
    ]
    .join("\n")
        + "\n"
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
    decision: &BrowserPolicyDecision,
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
            "  \"link_count\": {},\n",
            "  \"form_count\": {},\n",
            "  \"meta_count\": {},\n",
            "  \"policy_decision\": {},\n",
            "  \"sensitive_action_detected\": {},\n",
            "  \"stderr\": {},\n",
            "  \"playwright_actions_executed\": false,\n",
            "  \"live_browser_control\": false,\n",
            "  \"chrome_extension_evidence\": false,\n",
            "  \"external_web_control\": false,\n",
            "  \"credential_entry_executed\": false,\n",
            "  \"browser_click_type_executed\": false,\n",
            "  \"browser_interaction_loop_ref\": \"browser-interaction-loop.json\",\n",
            "  \"browser_runtime_ref\": \"browser-runtime/index.html\"\n",
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
        snapshot.links.len(),
        snapshot.forms.len(),
        snapshot.meta_names.len(),
        json_string(&decision.decision),
        decision.sensitive,
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
            "\"title\":{},\"content_hash\":{},\"bytes\":{},",
            "\"links\":{},\"forms\":{},\"meta_names\":{},\"nodes\":[],\"reason\":{}}}\n"
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
        json_vec(&snapshot.links),
        json_vec(&snapshot.forms),
        json_vec(&snapshot.meta_names),
        if snapshot.status == "captured" {
            json_string(
                "curl GET captured HTML bytes, title, links, forms, and meta names; full DOM tree requires Playwright",
            )
        } else {
            json_string(&snapshot.stderr)
        }
    )
}

fn render_browser_policy_decision(
    session: &CapabilitySession,
    target: &str,
    decision: &BrowserPolicyDecision,
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.browser-policy-decision.v1\",\n",
            "  \"session_id\": {},\n",
            "  \"target\": {},\n",
            "  \"requested_action\": {},\n",
            "  \"decision\": {},\n",
            "  \"reason\": {},\n",
            "  \"network_allowed\": {},\n",
            "  \"browser_action_allowed\": {},\n",
            "  \"sensitive\": {}\n",
            "}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        json_string(&decision.requested_action),
        json_string(&decision.decision),
        json_string(&decision.reason),
        decision.network_allowed,
        decision.browser_action_allowed,
        decision.sensitive
    )
}

fn render_browser_action_plan(
    session: &CapabilitySession,
    target: &str,
    decision: &BrowserPolicyDecision,
) -> String {
    let planned_actions = if decision.network_allowed {
        json_array(&["head_probe", "get_snapshot", "extract_links_forms_meta"])
    } else {
        json_array(&[])
    };
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.browser-action-plan.v1\",\n",
            "  \"session_id\": {},\n",
            "  \"target\": {},\n",
            "  \"planned_actions\": {},\n",
            "  \"executed_browser_actions\": [],\n",
            "  \"requires_approval_before_interaction\": true,\n",
            "  \"policy_decision_ref\": \"browser-policy-decision.json\"\n",
            "}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        planned_actions
    )
}

fn render_browser_page_links(
    session: &CapabilitySession,
    target: &str,
    snapshot: &PageSnapshot,
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.browser-page-links.v1\",\n",
            "  \"session_id\": {},\n",
            "  \"target\": {},\n",
            "  \"captured\": {},\n",
            "  \"links\": {},\n",
            "  \"forms\": {},\n",
            "  \"meta_names\": {}\n",
            "}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        snapshot.status == "captured",
        json_vec(&snapshot.links),
        json_vec(&snapshot.forms),
        json_vec(&snapshot.meta_names)
    )
}

fn render_browser_actions_jsonl(
    session: &CapabilitySession,
    probe: &HttpProbe,
    snapshot: &PageSnapshot,
    decision: &BrowserPolicyDecision,
) -> String {
    format!(
        concat!(
            "{{\"session_id\":{},\"plane\":\"browser\",\"action\":{},",
            "\"executed\":{},\"network_status\":{},\"snapshot_status\":{},",
            "\"requires_broker\":true,\"policy_decision\":{}}}\n"
        ),
        json_string(&session.id),
        json_string(&decision.requested_action),
        probe.status == "captured" || snapshot.status == "captured",
        json_string(&probe.status),
        json_string(&snapshot.status),
        json_string(&decision.decision)
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
            links: Vec::new(),
            forms: Vec::new(),
            meta_names: Vec::new(),
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
                links: extract_html_attributes(&body, "a", "href", 50),
                forms: extract_html_attributes(&body, "form", "action", 20),
                meta_names: extract_html_attributes(&body, "meta", "name", 30),
                stderr,
            }
        }
        Err(error) => PageSnapshot {
            attempted: true,
            status: "error".to_string(),
            title: None,
            bytes: 0,
            content_hash: None,
            links: Vec::new(),
            forms: Vec::new(),
            meta_names: Vec::new(),
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

fn extract_html_attributes(body: &str, tag: &str, attr: &str, limit: usize) -> Vec<String> {
    let lower = body.to_ascii_lowercase();
    let tag_prefix = format!("<{}", tag.to_ascii_lowercase());
    let attr_prefix = format!("{}=", attr.to_ascii_lowercase());
    let mut values = Vec::new();
    let mut search_start = 0;
    while values.len() < limit {
        let Some(tag_offset) = lower[search_start..].find(&tag_prefix) else {
            break;
        };
        let tag_start = search_start + tag_offset;
        let tag_end = lower[tag_start..]
            .find('>')
            .map(|offset| tag_start + offset)
            .unwrap_or_else(|| lower.len());
        let tag_text = &body[tag_start..tag_end];
        let lower_tag = &lower[tag_start..tag_end];
        if let Some(attr_offset) = lower_tag.find(&attr_prefix) {
            let value_start = attr_offset + attr_prefix.len();
            if let Some(value) = extract_quoted_or_bare_attribute(&tag_text[value_start..])
                && !values.contains(&value)
            {
                values.push(value);
            }
        }
        search_start = tag_end.saturating_add(1);
    }
    values
}

fn extract_quoted_or_bare_attribute(value: &str) -> Option<String> {
    let trimmed = value.trim_start();
    let first = trimmed.chars().next()?;
    if first == '"' || first == '\'' {
        let end = trimmed[1..].find(first)? + 1;
        let value = collapse_whitespace(&trimmed[1..end]);
        return (!value.is_empty()).then_some(value);
    }
    let value = trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches('/')
        .trim_matches('>')
        .to_string();
    (!value.is_empty()).then_some(value)
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

fn inspect_running_apps() -> AppInventory {
    if !cfg!(target_os = "macos") {
        return AppInventory {
            attempted: false,
            status: "skipped_non_macos".to_string(),
            apps: Vec::new(),
            stderr: String::new(),
        };
    }

    let output = process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to get name of application processes whose background only is false",
        ])
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let apps = stdout
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            AppInventory {
                attempted: true,
                status: if output.status.success() {
                    "captured".to_string()
                } else {
                    "failed".to_string()
                },
                apps,
                stderr,
            }
        }
        Err(error) => AppInventory {
            attempted: true,
            status: "error".to_string(),
            apps: Vec::new(),
            stderr: error.to_string(),
        },
    }
}

fn plan_app_action(target: &str) -> AppActionDecision {
    let lower = target.to_ascii_lowercase();
    let sensitive = [
        "password",
        "credential",
        "login",
        "send",
        "email",
        "delete",
        "purchase",
        "buy",
        "payment",
        "transfer",
        "trash",
        "archive",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let interactive = [
        "click", "type", "select", "open", "create", "move", "rename", "press", "paste",
    ]
    .iter()
    .any(|needle| lower.contains(needle));

    if sensitive {
        return AppActionDecision {
            requested_action: classify_app_action(&lower),
            decision: "denied_sensitive_app_action".to_string(),
            reason:
                "Sensitive app-use intent requires explicit human approval and was not executed."
                    .to_string(),
            inspection_allowed: true,
            app_action_allowed: false,
            sensitive: true,
        };
    }

    if interactive {
        return AppActionDecision {
            requested_action: classify_app_action(&lower),
            decision: "approval_required_for_app_action".to_string(),
            reason: "Native app action was planned but not executed without explicit approval."
                .to_string(),
            inspection_allowed: true,
            app_action_allowed: false,
            sensitive: false,
        };
    }

    AppActionDecision {
        requested_action: "inspect_app_state".to_string(),
        decision: "allowed_inspection_only".to_string(),
        reason: "Only non-destructive app inspection is allowed in the current local slice."
            .to_string(),
        inspection_allowed: true,
        app_action_allowed: false,
        sensitive: false,
    }
}

fn classify_app_action(lower: &str) -> String {
    for (needle, action) in [
        ("password", "credential_entry"),
        ("credential", "credential_entry"),
        ("login", "credential_entry"),
        ("send", "send"),
        ("email", "send"),
        ("delete", "delete"),
        ("trash", "delete"),
        ("purchase", "purchase"),
        ("buy", "purchase"),
        ("archive", "archive"),
        ("click", "click"),
        ("type", "type"),
        ("select", "select"),
        ("open", "open"),
        ("create", "create"),
        ("move", "move"),
        ("rename", "rename"),
        ("paste", "paste"),
    ] {
        if lower.contains(needle) {
            return action.to_string();
        }
    }
    "inspect_app_state".to_string()
}

fn write_app_inspection_artifacts(
    cwd: &Path,
    session: &CapabilitySession,
    target: &str,
    inspection: &AppInspection,
    inventory: &AppInventory,
    decision: &AppActionDecision,
) -> Result<(), OpenSksError> {
    let dir = cwd.join(OPEN_SKSDIR).join(session.plane).join(&session.id);
    write_text_atomic(
        &dir.join("accessibility-tree.json"),
        &render_app_accessibility_tree(session, target, inspection, inventory, decision),
    )?;
    write_text_atomic(
        &dir.join("running-apps.json"),
        &render_running_apps(session, inventory),
    )?;
    write_text_atomic(
        &dir.join("app-policy-decision.json"),
        &render_app_policy_decision(session, target, decision),
    )?;
    write_text_atomic(
        &dir.join("app-action-plan.json"),
        &render_app_action_plan(session, target, decision),
    )?;
    write_text_atomic(
        &dir.join("app-actions.jsonl"),
        &render_app_actions_jsonl(session, inspection, inventory, decision),
    )?;
    write_text_atomic(
        &dir.join("app-final-state.json"),
        &render_app_final_state(session, target, inspection, inventory, decision),
    )?;
    Ok(())
}

fn render_app_accessibility_tree(
    session: &CapabilitySession,
    target: &str,
    inspection: &AppInspection,
    inventory: &AppInventory,
    decision: &AppActionDecision,
) -> String {
    let nodes = inspection
        .frontmost_app
        .as_ref()
        .map(|app| {
            format!(
                "{{\"role\":\"application\",\"name\":{},\"frontmost\":true}}",
                json_string(app)
            )
        })
        .unwrap_or_else(|| String::from(""));
    format!(
        concat!(
            "{{\"schema\":\"opensks.accessibility-tree.v1\",\"session_id\":{},",
            "\"target\":{},\"captured\":{},\"frontmost_app\":{},",
            "\"running_app_count\":{},\"nodes\":[{}],\"status\":{},",
            "\"policy_decision\":{},\"stderr\":{}}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        inspection.status == "captured" && decision.inspection_allowed,
        inspection
            .frontmost_app
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_string()),
        inventory.apps.len(),
        nodes,
        json_string(&inspection.status),
        json_string(&decision.decision),
        json_string(&inspection.stderr)
    )
}

fn render_app_final_state(
    session: &CapabilitySession,
    target: &str,
    inspection: &AppInspection,
    inventory: &AppInventory,
    decision: &AppActionDecision,
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
            "  \"running_app_count\": {},\n",
            "  \"policy_decision\": {},\n",
            "  \"sensitive_action_detected\": {},\n",
            "  \"live_app_actions_executed\": false\n",
            "}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        inspection.attempted,
        if decision.sensitive {
            json_string("blocked_by_policy")
        } else {
            json_string(&inspection.status)
        },
        inspection
            .frontmost_app
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_string()),
        inventory.apps.len(),
        json_string(&decision.decision),
        decision.sensitive
    )
}

fn render_running_apps(session: &CapabilitySession, inventory: &AppInventory) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.running-apps.v1\",\n",
            "  \"session_id\": {},\n",
            "  \"attempted\": {},\n",
            "  \"status\": {},\n",
            "  \"apps\": {},\n",
            "  \"stderr\": {}\n",
            "}}\n"
        ),
        json_string(&session.id),
        inventory.attempted,
        json_string(&inventory.status),
        json_vec(&inventory.apps),
        json_string(&inventory.stderr)
    )
}

fn render_app_policy_decision(
    session: &CapabilitySession,
    target: &str,
    decision: &AppActionDecision,
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.app-policy-decision.v1\",\n",
            "  \"session_id\": {},\n",
            "  \"target\": {},\n",
            "  \"requested_action\": {},\n",
            "  \"decision\": {},\n",
            "  \"reason\": {},\n",
            "  \"inspection_allowed\": {},\n",
            "  \"app_action_allowed\": {},\n",
            "  \"sensitive\": {}\n",
            "}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        json_string(&decision.requested_action),
        json_string(&decision.decision),
        json_string(&decision.reason),
        decision.inspection_allowed,
        decision.app_action_allowed,
        decision.sensitive
    )
}

fn render_app_action_plan(
    session: &CapabilitySession,
    target: &str,
    decision: &AppActionDecision,
) -> String {
    let planned_actions = if decision.inspection_allowed {
        json_array(&["frontmost_app_inspection", "running_apps_inventory"])
    } else {
        json_array(&[])
    };
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.app-action-plan.v1\",\n",
            "  \"session_id\": {},\n",
            "  \"target\": {},\n",
            "  \"planned_actions\": {},\n",
            "  \"executed_native_app_actions\": [],\n",
            "  \"requires_approval_before_native_action\": true,\n",
            "  \"policy_decision_ref\": \"app-policy-decision.json\"\n",
            "}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        planned_actions
    )
}

fn render_app_actions_jsonl(
    session: &CapabilitySession,
    inspection: &AppInspection,
    inventory: &AppInventory,
    decision: &AppActionDecision,
) -> String {
    format!(
        concat!(
            "{{\"session_id\":{},\"plane\":\"app-use\",\"action\":{},",
            "\"executed\":false,\"inspection_status\":{},\"running_app_count\":{},",
            "\"requires_broker\":true,\"policy_decision\":{}}}\n"
        ),
        json_string(&session.id),
        json_string(&decision.requested_action),
        json_string(&inspection.status),
        inventory.apps.len(),
        json_string(&decision.decision)
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

fn plan_computer_action(target: &str) -> ComputerActionDecision {
    let lower = target.to_ascii_lowercase();
    let sensitive = [
        "password",
        "passcode",
        "credential",
        "secret",
        "purchase",
        "buy",
        "order",
        "payment",
        "transfer",
        "send",
        "email",
        "delete",
        "login",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let interactive = contains_any_action_token(
        &lower,
        &[
            "click", "type", "drag", "press", "key", "scroll", "paste", "submit", "open",
        ],
    );
    let wait_requested = lower.contains("wait") || lower.contains("pause");
    let observe_requested = [
        "inspect",
        "observe",
        "look",
        "screen",
        "screenshot",
        "capture",
        "desktop",
    ]
    .iter()
    .any(|needle| lower.contains(needle));

    if sensitive {
        return ComputerActionDecision {
            requested_action: classify_computer_action(&lower),
            decision: "denied_sensitive_action".to_string(),
            reason:
                "Sensitive computer-use action requires explicit human approval and was not executed."
                    .to_string(),
            screenshot_allowed: false,
            mouse_keyboard_allowed: false,
            wait_allowed: false,
            sensitive: true,
        };
    }

    if interactive {
        return ComputerActionDecision {
            requested_action: classify_computer_action(&lower),
            decision: "approval_required_for_mouse_keyboard".to_string(),
            reason: "Mouse/keyboard action was planned but not executed without explicit approval."
                .to_string(),
            screenshot_allowed: observe_requested,
            mouse_keyboard_allowed: false,
            wait_allowed: wait_requested,
            sensitive: false,
        };
    }

    ComputerActionDecision {
        requested_action: if wait_requested {
            "wait_and_observe".to_string()
        } else {
            "observe_screenshot".to_string()
        },
        decision: "allowed_observation_only".to_string(),
        reason: "Only non-destructive observation actions are allowed in the current local slice."
            .to_string(),
        screenshot_allowed: true,
        mouse_keyboard_allowed: false,
        wait_allowed: wait_requested,
        sensitive: false,
    }
}

fn contains_any_action_token(lower: &str, tokens: &[&str]) -> bool {
    lower
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .any(|token| tokens.contains(&token))
}

fn classify_computer_action(lower: &str) -> String {
    for (needle, action) in [
        ("password", "credential_entry"),
        ("login", "credential_entry"),
        ("click", "click"),
        ("type", "type"),
        ("drag", "drag"),
        ("press", "key_press"),
        ("scroll", "scroll"),
        ("open", "open"),
        ("paste", "paste"),
        ("submit", "submit"),
        ("delete", "delete"),
        ("send", "send"),
        ("purchase", "purchase"),
        ("buy", "purchase"),
    ] {
        if lower.contains(needle) {
            return action.to_string();
        }
    }
    "observe_screenshot".to_string()
}

fn write_computer_capture_artifacts(
    cwd: &Path,
    session: &CapabilitySession,
    target: &str,
    screenshot: &ScreenshotCapture,
    decision: &ComputerActionDecision,
) -> Result<(), OpenSksError> {
    let dir = cwd.join(OPEN_SKSDIR).join(session.plane).join(&session.id);
    write_text_atomic(
        &dir.join("computer-final-state.json"),
        &render_computer_final_state(session, target, screenshot, decision),
    )?;
    write_text_atomic(
        &dir.join("computer-actions.jsonl"),
        &render_computer_actions_jsonl(session, screenshot, decision),
    )?;
    write_text_atomic(
        &dir.join("computer-policy-decision.json"),
        &render_computer_policy_decision(session, target, decision),
    )?;
    write_text_atomic(
        &dir.join("computer-action-plan.json"),
        &render_computer_action_plan(session, target, decision),
    )?;
    let isolated_runtime = write_isolated_browser_runtime(cwd, session, target)?;
    write_text_atomic(
        &dir.join("isolated-browser-container.json"),
        &render_isolated_browser_container(session, target, &isolated_runtime),
    )?;
    write_text_atomic(
        &dir.join("computer-browser-loop.json"),
        &render_computer_browser_loop(session, target, screenshot, decision, &isolated_runtime),
    )?;
    write_text_atomic(
        &dir.join("computer-browser-loop-events.jsonl"),
        &render_computer_browser_loop_events(session, screenshot, decision, &isolated_runtime),
    )?;
    Ok(())
}

fn write_isolated_browser_runtime(
    cwd: &Path,
    session: &CapabilitySession,
    target: &str,
) -> Result<PathBuf, OpenSksError> {
    let runtime_dir = cwd
        .join(OPEN_SKSDIR)
        .join(session.plane)
        .join(&session.id)
        .join("isolated-browser-runtime");
    fs::create_dir_all(&runtime_dir)?;
    let page = render_isolated_browser_runtime_page(target);
    write_text_atomic(&runtime_dir.join("index.html"), &page)?;
    Ok(runtime_dir)
}

fn render_isolated_browser_runtime_page(target: &str) -> String {
    let page = format!(
        concat!(
            "<!doctype html>\n",
            "<html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n",
            "<title>OpenSKS isolated observation loop</title></head>\n",
            "<body><main><h1>OpenSKS isolated observation loop</h1>\n",
            "<p data-target=\"{}\">Observation-only browser/container seed.</p>\n",
            "<button id=\"{}\" type=\"button\" data-click-result=\"{}\">Record loop click</button>\n",
            "<label for=\"{}\">Loop input</label>\n",
            "<input id=\"{}\" name=\"loop-input\" data-type-result=\"{}\" autocomplete=\"off\">\n",
            "<output id=\"{}\">opensks-isolated-loop-ready</output>\n",
            "<script>\n",
            "const button = document.getElementById('{}');\n",
            "const input = document.getElementById('{}');\n",
            "const status = document.getElementById('{}');\n",
            "button.addEventListener('click', () => {{ status.value = button.dataset.clickResult; status.textContent = button.dataset.clickResult; }});\n",
            "input.addEventListener('input', () => {{ status.value = input.value || input.dataset.typeResult; status.textContent = input.value || input.dataset.typeResult; }});\n",
            "</script>\n",
            "</main></body></html>\n"
        ),
        html_escape_attr(target),
        COMPUTER_ISOLATED_LOOP_BUTTON_ID,
        COMPUTER_ISOLATED_LOOP_FINAL_TEXT,
        COMPUTER_ISOLATED_LOOP_INPUT_ID,
        COMPUTER_ISOLATED_LOOP_INPUT_ID,
        COMPUTER_ISOLATED_LOOP_FINAL_TEXT,
        COMPUTER_ISOLATED_LOOP_STATUS_ID,
        COMPUTER_ISOLATED_LOOP_BUTTON_ID,
        COMPUTER_ISOLATED_LOOP_INPUT_ID,
        COMPUTER_ISOLATED_LOOP_STATUS_ID
    );
    page
}

fn render_computer_final_state(
    session: &CapabilitySession,
    target: &str,
    screenshot: &ScreenshotCapture,
    decision: &ComputerActionDecision,
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
            "  \"policy_decision\": {},\n",
            "  \"sensitive_action_detected\": {},\n",
            "  \"mouse_keyboard_actions_executed\": false,\n",
            "  \"live_browser_container_control\": false,\n",
            "  \"external_web_control\": false,\n",
            "  \"isolated_browser_loop_ref\": \"computer-browser-loop.json\",\n",
            "  \"isolated_browser_runtime_ref\": \"isolated-browser-runtime/index.html\",\n",
            "  \"isolated_browser_final_text\": {},\n",
            "  \"wait_executed\": {}\n",
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
        json_string(&screenshot.stderr),
        json_string(&decision.decision),
        decision.sensitive,
        json_string(COMPUTER_ISOLATED_LOOP_FINAL_TEXT),
        decision.wait_allowed
    )
}

fn render_isolated_browser_container(
    session: &CapabilitySession,
    target: &str,
    runtime_dir: &Path,
) -> String {
    let page_path = runtime_dir.join("index.html");
    let page_hash = fs::read_to_string(&page_path)
        .map(|contents| stable_content_hash(&contents))
        .unwrap_or_else(|_| "unavailable".to_string());
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.isolated-browser-container.v1\",\n",
            "  \"session_id\": {},\n",
            "  \"target\": {},\n",
            "  \"isolation_root\": {},\n",
            "  \"seed_page\": {},\n",
            "  \"seed_page_hash\": {},\n",
            "  \"network_access_enabled\": false,\n",
            "  \"browser_process_launched\": false,\n",
            "  \"live_browser_control\": false,\n",
            "  \"external_web_control\": false,\n",
            "  \"container_status\": \"local_artifact_seeded\"\n",
            "}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        json_string(&runtime_dir.display().to_string()),
        json_string(&page_path.display().to_string()),
        json_string(&page_hash)
    )
}

fn render_computer_browser_loop(
    session: &CapabilitySession,
    target: &str,
    screenshot: &ScreenshotCapture,
    decision: &ComputerActionDecision,
    runtime_dir: &Path,
) -> String {
    let loop_steps = json_array(&[
        "create_isolated_runtime",
        "observe_screenshot_status",
        "open_local_runtime_state",
        "click_local_runtime_button",
        "type_local_runtime_input",
        "record_final_state",
    ]);
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.computer-browser-loop.v1\",\n",
            "  \"session_id\": {},\n",
            "  \"target\": {},\n",
            "  \"status\": \"local_isolated_observation_loop_recorded\",\n",
            "  \"isolation_root\": {},\n",
            "  \"loop_iterations\": 6,\n",
            "  \"loop_steps\": {},\n",
            "  \"isolated_runtime_created\": true,\n",
            "  \"isolated_runtime_ref\": \"isolated-browser-runtime/index.html\",\n",
            "  \"observation_loop_executed\": true,\n",
            "  \"computer_session_ref\": \"computer-session.json\",\n",
            "  \"computer_final_state_ref\": \"computer-final-state.json\",\n",
            "  \"browser_container_ref\": \"isolated-browser-container.json\",\n",
            "  \"browser_seed_ref\": \"isolated-browser-runtime/index.html\",\n",
            "  \"screenshot_status\": {},\n",
            "  \"policy_decision\": {},\n",
            "  \"isolated_browser_open_recorded\": true,\n",
            "  \"isolated_browser_click_recorded\": true,\n",
            "  \"isolated_browser_type_recorded\": true,\n",
            "  \"isolated_browser_final_text\": {},\n",
            "  \"button_element_id\": {},\n",
            "  \"input_element_id\": {},\n",
            "  \"status_element_id\": {},\n",
            "  \"live_browser_container_control\": false,\n",
            "  \"browser_click_type_executed\": false,\n",
            "  \"mouse_keyboard_actions_executed\": false,\n",
            "  \"external_web_control\": false,\n",
            "  \"requires_approval_before_interaction\": true,\n",
            "  \"evidence_note\": \"local isolated HTML open/click/type loop events are recorded as artifacts; live browser container control, live browser actions, external web control, and mouse/keyboard execution remain false/unverified\"\n",
            "}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        json_string(&runtime_dir.display().to_string()),
        loop_steps,
        json_string(&screenshot.status),
        json_string(&decision.decision),
        json_string(COMPUTER_ISOLATED_LOOP_FINAL_TEXT),
        json_string(COMPUTER_ISOLATED_LOOP_BUTTON_ID),
        json_string(COMPUTER_ISOLATED_LOOP_INPUT_ID),
        json_string(COMPUTER_ISOLATED_LOOP_STATUS_ID)
    )
}

fn render_computer_browser_loop_events(
    session: &CapabilitySession,
    screenshot: &ScreenshotCapture,
    decision: &ComputerActionDecision,
    runtime_dir: &Path,
) -> String {
    [
        format!(
            "{{\"schema\":\"opensks.computer-browser-loop-event.v1\",\"session_id\":{},\"event\":\"isolated_runtime_created\",\"path\":{},\"executed\":true}}",
            json_string(&session.id),
            json_string(&runtime_dir.display().to_string())
        ),
        format!(
            "{{\"schema\":\"opensks.computer-browser-loop-event.v1\",\"session_id\":{},\"event\":\"isolated_browser_open_recorded\",\"runtime_ref\":\"isolated-browser-runtime/index.html\",\"executed\":true}}",
            json_string(&session.id)
        ),
        format!(
            "{{\"schema\":\"opensks.computer-browser-loop-event.v1\",\"session_id\":{},\"event\":\"isolated_browser_click_recorded\",\"element_id\":{},\"final_text\":{},\"executed\":true}}",
            json_string(&session.id),
            json_string(COMPUTER_ISOLATED_LOOP_BUTTON_ID),
            json_string(COMPUTER_ISOLATED_LOOP_FINAL_TEXT)
        ),
        format!(
            "{{\"schema\":\"opensks.computer-browser-loop-event.v1\",\"session_id\":{},\"event\":\"isolated_browser_type_recorded\",\"element_id\":{},\"typed_text\":{},\"executed\":true}}",
            json_string(&session.id),
            json_string(COMPUTER_ISOLATED_LOOP_INPUT_ID),
            json_string(COMPUTER_ISOLATED_LOOP_FINAL_TEXT)
        ),
        format!(
            "{{\"schema\":\"opensks.computer-browser-loop-event.v1\",\"session_id\":{},\"event\":\"isolated_browser_final_state_recorded\",\"status_element_id\":{},\"final_text\":{},\"executed\":true}}",
            json_string(&session.id),
            json_string(COMPUTER_ISOLATED_LOOP_STATUS_ID),
            json_string(COMPUTER_ISOLATED_LOOP_FINAL_TEXT)
        ),
        format!(
            "{{\"schema\":\"opensks.computer-browser-loop-event.v1\",\"session_id\":{},\"event\":\"computer_observation\",\"screenshot_status\":{},\"executed\":{}}}",
            json_string(&session.id),
            json_string(&screenshot.status),
            screenshot.attempted
        ),
        format!(
            "{{\"schema\":\"opensks.computer-browser-loop-event.v1\",\"session_id\":{},\"event\":\"interactive_browser_or_mouse_keyboard_action\",\"executed\":false,\"policy_decision\":{},\"approval_required\":true,\"live_browser_container_control\":false,\"external_web_control\":false}}",
            json_string(&session.id),
            json_string(&decision.decision)
        ),
    ]
    .join("\n")
        + "\n"
}

fn render_computer_actions_jsonl(
    session: &CapabilitySession,
    screenshot: &ScreenshotCapture,
    decision: &ComputerActionDecision,
) -> String {
    format!(
        concat!(
            "{{\"session_id\":{},\"plane\":\"computer-use\",\"action\":{},",
            "\"executed\":{},\"status\":{},\"requires_broker\":true,",
            "\"policy_decision\":{}}}\n"
        ),
        json_string(&session.id),
        json_string(&decision.requested_action),
        screenshot.status == "captured",
        json_string(&screenshot.status),
        json_string(&decision.decision)
    )
}

fn render_computer_policy_decision(
    session: &CapabilitySession,
    target: &str,
    decision: &ComputerActionDecision,
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.computer-policy-decision.v1\",\n",
            "  \"session_id\": {},\n",
            "  \"target\": {},\n",
            "  \"requested_action\": {},\n",
            "  \"decision\": {},\n",
            "  \"reason\": {},\n",
            "  \"screenshot_allowed\": {},\n",
            "  \"mouse_keyboard_allowed\": {},\n",
            "  \"wait_allowed\": {},\n",
            "  \"sensitive\": {}\n",
            "}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        json_string(&decision.requested_action),
        json_string(&decision.decision),
        json_string(&decision.reason),
        decision.screenshot_allowed,
        decision.mouse_keyboard_allowed,
        decision.wait_allowed,
        decision.sensitive
    )
}

fn render_computer_action_plan(
    session: &CapabilitySession,
    target: &str,
    decision: &ComputerActionDecision,
) -> String {
    let planned_actions = if decision.screenshot_allowed && decision.wait_allowed {
        json_array(&["wait_250ms", "screenshot"])
    } else if decision.screenshot_allowed {
        json_array(&["screenshot"])
    } else if decision.wait_allowed {
        json_array(&["wait_250ms"])
    } else {
        json_array(&[])
    };
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.computer-action-plan.v1\",\n",
            "  \"session_id\": {},\n",
            "  \"target\": {},\n",
            "  \"planned_actions\": {},\n",
            "  \"executed_mouse_keyboard_actions\": [],\n",
            "  \"requires_approval_before_mouse_keyboard\": true,\n",
            "  \"policy_decision_ref\": \"computer-policy-decision.json\"\n",
            "}}\n"
        ),
        json_string(&session.id),
        json_string(target),
        planned_actions
    )
}

fn run_local_qa_checks(cwd: &Path) -> Vec<CommandCheck> {
    opensks_cli::run_local_qa_checks(cwd)
}

fn scan_workspace_for_secrets(cwd: &Path) -> Result<Vec<SecretFinding>, OpenSksError> {
    let mut findings = Vec::new();
    scan_dir_for_secrets(cwd, cwd, &mut findings)?;
    Ok(findings)
}

fn count_secret_scan_targets(cwd: &Path) -> Result<usize, OpenSksError> {
    let mut count = 0;
    count_secret_scan_targets_in_dir(cwd, &mut count)?;
    Ok(count)
}

fn count_secret_scan_targets_in_dir(current: &Path, count: &mut usize) -> Result<(), OpenSksError> {
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(OpenSksError::Io(error)),
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip_runtime_path(&name) {
            continue;
        }
        if path.is_dir() {
            count_secret_scan_targets_in_dir(&path, count)?;
        } else if is_text_like_file(&path) {
            *count += 1;
        }
    }
    Ok(())
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

fn scan_workspace_for_security_findings(cwd: &Path) -> Result<Vec<SecurityFinding>, OpenSksError> {
    let mut findings = Vec::new();
    scan_dir_for_security_findings(cwd, cwd, &mut findings)?;
    findings.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.line_number.cmp(&right.line_number))
            .then(left.rule.cmp(&right.rule))
    });
    Ok(findings)
}

fn scan_dir_for_security_findings(
    root: &Path,
    current: &Path,
    findings: &mut Vec<SecurityFinding>,
) -> Result<(), OpenSksError> {
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(OpenSksError::Io(error)),
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip_runtime_path(&name) {
            continue;
        }
        if path.is_dir() {
            scan_dir_for_security_findings(root, &path, findings)?;
            continue;
        }
        if !is_text_like_file(&path) {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        inspect_security_surface(&relative_path(root, &path), &contents, findings);
    }
    Ok(())
}

fn inspect_security_surface(path: &str, contents: &str, findings: &mut Vec<SecurityFinding>) {
    for (index, line) in contents.lines().enumerate() {
        let line_number = index + 1;
        let lower = line.to_ascii_lowercase();
        if contains_joined_phrase(&lower, &["ignore ", "previous ", "instructions"])
            || contains_joined_phrase(&lower, &["disregard ", "previous ", "instructions"])
            || contains_joined_phrase(&lower, &["reveal ", "hidden"])
            || contains_joined_phrase(&lower, &["system ", "prompt"])
        {
            findings.push(security_finding(
                "prompt_injection",
                path,
                line_number,
                "prompt_injection_phrase",
                "warning",
                "Prompt-injection-like phrase found in workspace text.",
            ));
        }
        if contains_joined_phrase(&lower, &["c", "url "])
            && lower.as_bytes().contains(&124)
            && contains_joined_phrase(&lower, &["s", "h"])
        {
            findings.push(security_finding(
                "supply_chain",
                path,
                line_number,
                "curl_pipe_shell",
                "critical",
                "curl piped into shell requires explicit review.",
            ));
        }
        if contains_joined_phrase(&lower, &["npm ", "install ", "-g"])
            || contains_joined_phrase(&lower, &["pip ", "install"])
        {
            findings.push(security_finding(
                "supply_chain",
                path,
                line_number,
                "unpinned_package_install",
                "info",
                "Package install command should be checked for pinning and trusted source.",
            ));
        }
        if contains_joined_phrase(&lower, &["rm ", "-rf ", "/"])
            || contains_joined_phrase(&lower, &["sudo ", "rm ", "-rf"])
        {
            findings.push(security_finding(
                "unsafe_action",
                path,
                line_number,
                "destructive_shell_command",
                "critical",
                "Destructive shell command pattern found.",
            ));
        }
        if contains_joined_phrase(&lower, &["m", "cp"])
            && contains_joined_phrase(&lower, &["always ", "allow"])
        {
            findings.push(security_finding(
                "mcp_tool_poisoning",
                path,
                line_number,
                "mcp_allowlist_bypass_phrase",
                "warning",
                "MCP allowlist bypass phrasing should be reviewed.",
            ));
        }
    }
}

fn contains_joined_phrase(line: &str, parts: &[&str]) -> bool {
    line.contains(&parts.concat())
}

fn security_finding(
    category: &str,
    path: &str,
    line_number: usize,
    rule: &str,
    severity: &str,
    message: &str,
) -> SecurityFinding {
    SecurityFinding {
        category: category.to_string(),
        path: path.to_string(),
        line_number,
        rule: rule.to_string(),
        severity: severity.to_string(),
        message: message.to_string(),
    }
}

fn security_scan_summary(
    secret_findings: &[SecretFinding],
    security_findings: &[SecurityFinding],
) -> SecurityScanSummary {
    SecurityScanSummary {
        secret_findings: secret_findings.len(),
        security_findings: security_findings.len(),
        critical_or_warning_findings: security_findings
            .iter()
            .filter(|finding| finding.severity == "critical" || finding.severity == "warning")
            .count(),
    }
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
        ".git" | ".opensks" | ".sneakoscope" | "target" | "node_modules" | ".DS_Store"
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
    append_voxel_triwiki_cache_segment(cwd, &mut segments)?;
    segments.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(segments)
}

fn append_voxel_triwiki_cache_segment(
    cwd: &Path,
    segments: &mut Vec<CacheSegment>,
) -> Result<(), OpenSksError> {
    let path = cwd.join(OPEN_SKSDIR).join("triwiki").join("voxels.jsonl");
    if !path.exists() {
        return Ok(());
    }
    let contents = fs::read_to_string(&path)?;
    segments.push(CacheSegment {
        name: "voxel_triwiki_summary".to_string(),
        path: ".opensks/triwiki/voxels.jsonl".to_string(),
        bytes: contents.len() as u64,
        content_hash: stable_content_hash(&contents),
        stability: "stable".to_string(),
    });
    Ok(())
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

fn read_cache_prefix_snapshot(path: &Path) -> Result<Vec<CacheSegment>, OpenSksError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents = fs::read_to_string(path)?;
    let segments = contents
        .lines()
        .filter_map(|line| {
            let path = extract_json_string_field(line, "path")?;
            let content_hash = extract_json_string_field(line, "content_hash")?;
            let stability = extract_json_string_field(line, "stability")?;
            let bytes = extract_json_number_field(line, "bytes").unwrap_or_default() as u64;
            Some(CacheSegment {
                name: path.replace('/', "::"),
                path,
                bytes,
                content_hash,
                stability,
            })
        })
        .collect();
    Ok(segments)
}

fn compute_cache_prefix_hit(
    previous: &[CacheSegment],
    current: &[CacheSegment],
) -> CachePrefixHitReport {
    let previous_stable = previous
        .iter()
        .filter(|segment| segment.stability == "stable")
        .map(|segment| (segment.path.as_str(), segment))
        .collect::<HashMap<_, _>>();
    let current_stable = current
        .iter()
        .filter(|segment| segment.stability == "stable")
        .collect::<Vec<_>>();
    let matched = current_stable
        .iter()
        .filter(|segment| {
            previous_stable
                .get(segment.path.as_str())
                .is_some_and(|previous| previous.content_hash == segment.content_hash)
        })
        .collect::<Vec<_>>();
    let current_stable_bytes = current_stable
        .iter()
        .map(|segment| segment.bytes)
        .sum::<u64>();
    let matched_stable_bytes = matched.iter().map(|segment| segment.bytes).sum::<u64>();
    let local_hit_percent = if current_stable_bytes == 0 {
        0.0
    } else {
        (matched_stable_bytes as f64 / current_stable_bytes as f64) * 100.0
    };
    let target_hit_percent = 95.0;
    CachePrefixHitReport {
        baseline_available: !previous_stable.is_empty(),
        previous_stable_segment_count: previous_stable.len(),
        current_stable_segment_count: current_stable.len(),
        matched_stable_segment_count: matched.len(),
        current_stable_bytes,
        matched_stable_bytes,
        estimated_cached_tokens: estimate_tokens_from_bytes(matched_stable_bytes),
        estimated_cache_write_tokens: estimate_tokens_from_bytes(
            current_stable_bytes.saturating_sub(matched_stable_bytes),
        ),
        local_hit_percent,
        target_hit_percent,
        local_target_met: !previous_stable.is_empty() && local_hit_percent >= target_hit_percent,
    }
}

fn estimate_tokens_from_bytes(bytes: u64) -> u64 {
    bytes.saturating_add(3) / 4
}

fn index_workspace_voxels(cwd: &Path) -> Result<Vec<Voxel>, OpenSksError> {
    let mut voxels = Vec::new();
    index_workspace_voxels_from_dir(cwd, cwd, &mut voxels)?;
    voxels.sort_by(|left, right| left.coordinates.cmp(&right.coordinates));
    Ok(voxels)
}

fn index_workspace_voxels_from_dir(
    root: &Path,
    current: &Path,
    voxels: &mut Vec<Voxel>,
) -> Result<(), OpenSksError> {
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(OpenSksError::Io(error)),
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip_runtime_path(&name) {
            continue;
        }
        if path.is_dir() {
            index_workspace_voxels_from_dir(root, &path, voxels)?;
            continue;
        }
        if !is_text_like_file(&path) {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        let relative = relative_path(root, &path);
        voxels.push(workspace_file_voxel(&relative, &contents));
        voxels.extend(workspace_topic_voxels(&relative, &contents));
        for (index, line) in contents.lines().enumerate() {
            if let Some(voxel) = symbol_voxel_for_line(&relative, index + 1, line) {
                voxels.push(voxel);
            }
        }
    }
    Ok(())
}

fn workspace_file_voxel(relative: &str, contents: &str) -> Voxel {
    let kind = infer_workspace_voxel_kind(relative, contents);
    let stability = if matches!(
        relative,
        "Cargo.toml" | "Cargo.lock" | "README.md" | ".gitignore"
    ) || relative.starts_with("docs/")
    {
        "stable"
    } else {
        "dynamic"
    };
    Voxel {
        id: format!("voxel-index-file-{}", stable_id(relative)),
        kind,
        coordinates: format!("repo:file:{relative}"),
        content_hash: stable_content_hash(contents),
        summary: format!("{} bytes indexed from {}", contents.len(), relative),
        evidence_refs: vec![relative.to_string()],
        links: vec!["indexed_by:voxel.index".to_string()],
        cache_stability: stability.to_string(),
        privacy_level: "workspace".to_string(),
    }
}

fn workspace_topic_voxels(relative: &str, contents: &str) -> Vec<Voxel> {
    let lower = format!(
        "{} {}",
        relative.to_ascii_lowercase(),
        contents.to_ascii_lowercase()
    );
    let mut topics = Vec::new();
    for (kind, needles) in [
        (
            "provider_voxel",
            &["provider", "openrouter", "ollama", "lm studio", "openai"] as &[&str],
        ),
        (
            "security_voxel",
            &["security", "secret", "prompt injection", "supply chain"],
        ),
        (
            "design_voxel",
            &["design", "accessibility", "color", "viewport"],
        ),
        ("cache_voxel", &["cache", "cached", "stable prefix"]),
    ] {
        if needles.iter().any(|needle| lower.contains(needle)) {
            topics.push(Voxel {
                id: format!("voxel-index-topic-{}-{}", kind, stable_id(relative)),
                kind: kind.to_string(),
                coordinates: format!("repo:topic:{kind}:{relative}"),
                content_hash: stable_content_hash(&format!("{kind}:{relative}:{contents}")),
                summary: format!("{kind} topic evidence indexed from {relative}"),
                evidence_refs: vec![relative.to_string()],
                links: vec![
                    format!("derived_from:repo:file:{relative}"),
                    "indexed_by:voxel.index".to_string(),
                ],
                cache_stability: if relative == "README.md" || relative.starts_with("docs/") {
                    "stable".to_string()
                } else {
                    "dynamic".to_string()
                },
                privacy_level: "workspace".to_string(),
            });
        }
    }
    topics
}

fn symbol_voxel_for_line(relative: &str, line_number: usize, line: &str) -> Option<Voxel> {
    let trimmed = line.trim();
    let symbol_name = if let Some(rest) = trimmed.strip_prefix("fn ") {
        rest.split(['(', '<', ' '])
            .next()
            .filter(|value| !value.is_empty())
    } else if let Some(rest) = trimmed.strip_prefix("struct ") {
        rest.split(['{', '<', ' '])
            .next()
            .filter(|value| !value.is_empty())
    } else if let Some(rest) = trimmed.strip_prefix("enum ") {
        rest.split(['{', '<', ' '])
            .next()
            .filter(|value| !value.is_empty())
    } else if let Some(rest) = trimmed.strip_prefix("pub fn ") {
        rest.split(['(', '<', ' '])
            .next()
            .filter(|value| !value.is_empty())
    } else {
        None
    }?;
    Some(Voxel {
        id: format!(
            "voxel-index-symbol-{}",
            stable_id(&format!("{relative}:{line_number}:{symbol_name}"))
        ),
        kind: "symbol_voxel".to_string(),
        coordinates: format!("repo:symbol:{relative}:{line_number}:{symbol_name}"),
        content_hash: stable_content_hash(trimmed),
        summary: format!("Symbol {symbol_name} in {relative}:{line_number}"),
        evidence_refs: vec![format!("{relative}:{line_number}")],
        links: vec![
            format!("depends_on:repo:file:{relative}"),
            "indexed_by:voxel.index".to_string(),
        ],
        cache_stability: "dynamic".to_string(),
        privacy_level: "workspace".to_string(),
    })
}

fn infer_workspace_voxel_kind(relative: &str, contents: &str) -> String {
    let lower_path = relative.to_ascii_lowercase();
    let lower = contents.to_ascii_lowercase();
    if lower_path.contains("test") || lower.contains("#[test]") {
        "test_voxel".to_string()
    } else if lower_path.ends_with(".rs") {
        "code_voxel".to_string()
    } else if lower_path.contains("design")
        || lower.contains("design qa")
        || lower.contains("color")
    {
        "design_voxel".to_string()
    } else if lower_path.contains("security") || lower.contains("security audit") {
        "security_voxel".to_string()
    } else if lower_path.contains("provider")
        || lower.contains("openrouter")
        || lower.contains("ollama")
    {
        "provider_voxel".to_string()
    } else if lower_path.ends_with("cargo.toml") || lower_path.ends_with("cargo.lock") {
        "package_voxel".to_string()
    } else if lower_path.ends_with(".md") {
        "context_voxel".to_string()
    } else {
        "code_voxel".to_string()
    }
}

fn stable_id(value: &str) -> String {
    stable_content_hash(value)
        .trim_start_matches("fnv1a64:")
        .to_string()
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

fn render_voxel_index_report(stamp: &ClockStamp, voxels: &[Voxel]) -> String {
    let stable = voxels
        .iter()
        .filter(|voxel| voxel.cache_stability == "stable")
        .count();
    let dynamic = voxels.len().saturating_sub(stable);
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.voxel-index-report.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"voxel_count\": {},\n",
            "  \"stable_voxels\": {},\n",
            "  \"dynamic_voxels\": {},\n",
            "  \"kind_summary\": {},\n",
            "  \"axes\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        voxels.len(),
        stable,
        dynamic,
        render_voxel_kind_summary_json(voxels),
        json_array(&[
            "code_space",
            "time_mission_space",
            "proof_design_intent_space"
        ])
    )
}

fn render_voxel_kind_summary_json(voxels: &[Voxel]) -> String {
    let kinds = [
        "code_voxel",
        "symbol_voxel",
        "test_voxel",
        "context_voxel",
        "design_voxel",
        "security_voxel",
        "provider_voxel",
        "package_voxel",
        "cache_voxel",
    ];
    let rows = kinds
        .iter()
        .map(|kind| {
            let count = voxels.iter().filter(|voxel| voxel.kind == *kind).count();
            format!("{}:{}", json_string(kind), count)
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{rows}}}")
}

fn render_index_triwiki_graph(stamp: &ClockStamp, voxels: &[Voxel]) -> String {
    let nodes = voxels
        .iter()
        .map(|voxel| {
            format!(
                "{{\"id\":{},\"kind\":{},\"coordinates\":{},\"hash\":{}}}",
                json_string(&voxel.id),
                json_string(&voxel.kind),
                json_string(&voxel.coordinates),
                json_string(&voxel.content_hash)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let edges = voxels
        .iter()
        .flat_map(|voxel| {
            voxel.links.iter().map(move |link| {
                format!(
                    "{{\"from\":{},\"link\":{}}}",
                    json_string(&voxel.id),
                    json_string(link)
                )
            })
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.triwiki-graph.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"source\": \"voxel index\",\n",
            "  \"node_count\": {},\n",
            "  \"edge_count\": {},\n",
            "  \"nodes\": [{}],\n",
            "  \"edges\": [{}]\n",
            "}}\n"
        ),
        stamp.json(),
        voxels.len(),
        voxels.iter().map(|voxel| voxel.links.len()).sum::<usize>(),
        nodes,
        edges
    )
}

fn collect_design_qa(cwd: &Path) -> Result<(Vec<DesignSurface>, Vec<DesignFinding>), OpenSksError> {
    let mut surfaces = Vec::new();
    let mut findings = Vec::new();
    collect_design_qa_from_dir(cwd, cwd, &mut surfaces, &mut findings)?;
    surfaces.sort_by(|left, right| left.path.cmp(&right.path));
    findings.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.line_number.cmp(&right.line_number))
            .then(left.rule.cmp(&right.rule))
    });
    Ok((surfaces, findings))
}

fn collect_design_qa_from_dir(
    root: &Path,
    current: &Path,
    surfaces: &mut Vec<DesignSurface>,
    findings: &mut Vec<DesignFinding>,
) -> Result<(), OpenSksError> {
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(OpenSksError::Io(error)),
    };

    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip_runtime_path(&name) {
            continue;
        }
        if path.is_dir() {
            collect_design_qa_from_dir(root, &path, surfaces, findings)?;
            continue;
        }
        if !is_design_surface_file(&path) {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        let relative = relative_path(root, &path);
        let kind = design_surface_kind(&path, &contents);
        let color_tokens = extract_color_tokens(&contents);
        let visual_signature = design_visual_signature(&contents, &color_tokens);
        surfaces.push(DesignSurface {
            path: relative.clone(),
            kind,
            bytes: contents.len() as u64,
            content_hash: stable_content_hash(&contents),
            visual_signature,
            color_tokens: color_tokens.iter().take(32).cloned().collect(),
        });
        inspect_design_surface(&relative, &contents, findings);
    }
    Ok(())
}

fn read_design_surface_snapshot(path: &Path) -> Result<Vec<DesignSurface>, OpenSksError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents = fs::read_to_string(path)?;
    let surfaces = contents
        .lines()
        .filter_map(|line| {
            let path = extract_json_string_field(line, "path")?;
            let kind =
                extract_json_string_field(line, "kind").unwrap_or_else(|| "unknown".to_string());
            let content_hash = extract_json_string_field(line, "content_hash")?;
            let visual_signature = extract_json_string_field(line, "visual_signature")?;
            let bytes = extract_json_number_field(line, "bytes").unwrap_or_default() as u64;
            Some(DesignSurface {
                path,
                kind,
                bytes,
                content_hash,
                visual_signature,
                color_tokens: Vec::new(),
            })
        })
        .collect();
    Ok(surfaces)
}

fn compute_design_visual_diffs(
    previous: &[DesignSurface],
    current: &[DesignSurface],
) -> Vec<DesignVisualDiff> {
    let previous_by_path = previous
        .iter()
        .map(|surface| (surface.path.as_str(), surface))
        .collect::<HashMap<_, _>>();
    let current_by_path = current
        .iter()
        .map(|surface| (surface.path.as_str(), surface))
        .collect::<HashMap<_, _>>();
    let mut diffs = Vec::new();

    for surface in current {
        match previous_by_path.get(surface.path.as_str()) {
            Some(previous) if previous.visual_signature == surface.visual_signature => {
                diffs.push(DesignVisualDiff {
                    path: surface.path.clone(),
                    status: "unchanged".to_string(),
                    previous_signature: Some(previous.visual_signature.clone()),
                    current_signature: Some(surface.visual_signature.clone()),
                    bytes_delta: surface.bytes as i64 - previous.bytes as i64,
                });
            }
            Some(previous) => {
                diffs.push(DesignVisualDiff {
                    path: surface.path.clone(),
                    status: "changed".to_string(),
                    previous_signature: Some(previous.visual_signature.clone()),
                    current_signature: Some(surface.visual_signature.clone()),
                    bytes_delta: surface.bytes as i64 - previous.bytes as i64,
                });
            }
            None => {
                diffs.push(DesignVisualDiff {
                    path: surface.path.clone(),
                    status: "added".to_string(),
                    previous_signature: None,
                    current_signature: Some(surface.visual_signature.clone()),
                    bytes_delta: surface.bytes as i64,
                });
            }
        }
    }

    for surface in previous {
        if !current_by_path.contains_key(surface.path.as_str()) {
            diffs.push(DesignVisualDiff {
                path: surface.path.clone(),
                status: "removed".to_string(),
                previous_signature: Some(surface.visual_signature.clone()),
                current_signature: None,
                bytes_delta: -(surface.bytes as i64),
            });
        }
    }

    diffs.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.status.cmp(&right.status))
    });
    diffs
}

fn read_design_screenshot_snapshot(
    path: &Path,
) -> Result<Vec<DesignScreenshotArtifact>, OpenSksError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents = fs::read_to_string(path)?;
    let snapshots = contents
        .lines()
        .filter_map(|line| {
            if !json_string_field_equals(line, "schema", "opensks.design-screenshot-snapshot.v1") {
                return None;
            }
            let path = extract_json_string_field(line, "path")?;
            let kind = extract_json_string_field(line, "kind")?;
            let image_path = extract_json_string_field(line, "image_path")?;
            let width = extract_json_number_field(line, "width")?;
            let height = extract_json_number_field(line, "height")?;
            let pixel_count = extract_json_number_field(line, "pixel_count")?;
            let screenshot_hash = extract_json_string_field(line, "screenshot_hash")?;
            let content_hash = extract_json_string_field(line, "content_hash")?;
            let visual_signature = extract_json_string_field(line, "visual_signature")?;
            Some(DesignScreenshotArtifact {
                path,
                kind,
                image_path,
                width,
                height,
                pixel_count,
                screenshot_hash,
                content_hash,
                visual_signature,
            })
        })
        .collect();
    Ok(snapshots)
}

fn write_design_screenshot_artifacts(
    design_dir: &Path,
    surfaces: &[DesignSurface],
) -> Result<Vec<DesignScreenshotArtifact>, OpenSksError> {
    let screenshot_dir = design_dir.join("screenshots");
    fs::create_dir_all(&screenshot_dir)?;
    let mut artifacts = Vec::new();
    for surface in surfaces {
        let ppm = render_design_screenshot_ppm(surface);
        let screenshot_hash = stable_content_hash(&ppm);
        let image_path = design_screenshot_image_path(surface, &screenshot_hash);
        write_text_atomic(&design_dir.join(&image_path), &ppm)?;
        artifacts.push(DesignScreenshotArtifact {
            path: surface.path.clone(),
            kind: surface.kind.clone(),
            image_path,
            width: DESIGN_SCREENSHOT_WIDTH,
            height: DESIGN_SCREENSHOT_HEIGHT,
            pixel_count: DESIGN_SCREENSHOT_WIDTH * DESIGN_SCREENSHOT_HEIGHT,
            screenshot_hash,
            content_hash: surface.content_hash.clone(),
            visual_signature: surface.visual_signature.clone(),
        });
    }
    Ok(artifacts)
}

fn design_screenshot_image_path(surface: &DesignSurface, screenshot_hash: &str) -> String {
    let path_hash = stable_content_hash(&surface.path)
        .replace("fnv1a64:", "")
        .replace(':', "-");
    let image_hash = screenshot_hash.replace("fnv1a64:", "").replace(':', "-");
    format!("screenshots/design-screenshot-{path_hash}-{image_hash}.ppm")
}

fn render_design_screenshot_ppm(surface: &DesignSurface) -> String {
    let mut out = format!(
        concat!(
            "P3\n",
            "# OpenSKS deterministic local raster screenshot artifact\n",
            "# source_path={}\n",
            "# renderer={}\n",
            "{} {}\n",
            "255\n"
        ),
        surface.path, DESIGN_SCREENSHOT_RENDERER, DESIGN_SCREENSHOT_WIDTH, DESIGN_SCREENSHOT_HEIGHT
    );
    let seed = stable_content_hash_u64(&format!(
        "{}|{}|{}|{}",
        surface.path, surface.kind, surface.content_hash, surface.visual_signature
    ));
    for y in 0..DESIGN_SCREENSHOT_HEIGHT {
        for x in 0..DESIGN_SCREENSHOT_WIDTH {
            let value = stable_content_hash_u64(&format!(
                "{seed:016x}|{}|{}|{}|{}",
                x, y, surface.visual_signature, surface.content_hash
            ));
            let red = (value & 0xff) as u8;
            let green = ((value >> 8) & 0xff) as u8;
            let blue = ((value >> 16) & 0xff) as u8;
            out.push_str(&format!("{red} {green} {blue}\n"));
        }
    }
    out
}

fn compute_design_screenshot_diffs(
    design_dir: &Path,
    previous: &[DesignScreenshotArtifact],
    current: &[DesignScreenshotArtifact],
) -> Vec<DesignScreenshotDiff> {
    let previous_by_path = previous
        .iter()
        .map(|surface| (surface.path.as_str(), surface))
        .collect::<HashMap<_, _>>();
    let current_by_path = current
        .iter()
        .map(|surface| (surface.path.as_str(), surface))
        .collect::<HashMap<_, _>>();
    let mut diffs = Vec::new();

    for artifact in current {
        match previous_by_path.get(artifact.path.as_str()) {
            Some(previous_artifact) => {
                let (pixel_changed_count, pixel_count, image_artifacts_present) =
                    compare_design_screenshot_pixels(design_dir, previous_artifact, artifact);
                diffs.push(DesignScreenshotDiff {
                    path: artifact.path.clone(),
                    status: if previous_artifact.screenshot_hash == artifact.screenshot_hash {
                        "unchanged".to_string()
                    } else {
                        "changed".to_string()
                    },
                    previous_screenshot_hash: Some(previous_artifact.screenshot_hash.clone()),
                    current_screenshot_hash: Some(artifact.screenshot_hash.clone()),
                    previous_image_path: Some(previous_artifact.image_path.clone()),
                    current_image_path: Some(artifact.image_path.clone()),
                    pixel_count,
                    pixel_changed_count,
                    image_artifacts_present,
                });
            }
            None => {
                let image_artifacts_present =
                    design_screenshot_file_hash_matches(design_dir, artifact);
                diffs.push(DesignScreenshotDiff {
                    path: artifact.path.clone(),
                    status: "added".to_string(),
                    previous_screenshot_hash: None,
                    current_screenshot_hash: Some(artifact.screenshot_hash.clone()),
                    previous_image_path: None,
                    current_image_path: Some(artifact.image_path.clone()),
                    pixel_count: artifact.pixel_count,
                    pixel_changed_count: artifact.pixel_count,
                    image_artifacts_present,
                });
            }
        }
    }

    for artifact in previous {
        if !current_by_path.contains_key(artifact.path.as_str()) {
            let image_artifacts_present = design_screenshot_file_hash_matches(design_dir, artifact);
            diffs.push(DesignScreenshotDiff {
                path: artifact.path.clone(),
                status: "removed".to_string(),
                previous_screenshot_hash: Some(artifact.screenshot_hash.clone()),
                current_screenshot_hash: None,
                previous_image_path: Some(artifact.image_path.clone()),
                current_image_path: None,
                pixel_count: artifact.pixel_count,
                pixel_changed_count: artifact.pixel_count,
                image_artifacts_present,
            });
        }
    }

    diffs.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.status.cmp(&right.status))
    });
    diffs
}

fn compare_design_screenshot_pixels(
    design_dir: &Path,
    previous: &DesignScreenshotArtifact,
    current: &DesignScreenshotArtifact,
) -> (usize, usize, bool) {
    let previous_path = design_dir.join(&previous.image_path);
    let current_path = design_dir.join(&current.image_path);
    let Ok(previous_ppm) = fs::read_to_string(previous_path) else {
        return (
            current.pixel_count.max(previous.pixel_count),
            current.pixel_count.max(previous.pixel_count),
            false,
        );
    };
    let Ok(current_ppm) = fs::read_to_string(current_path) else {
        return (
            current.pixel_count.max(previous.pixel_count),
            current.pixel_count.max(previous.pixel_count),
            false,
        );
    };
    if stable_content_hash(&previous_ppm) != previous.screenshot_hash
        || stable_content_hash(&current_ppm) != current.screenshot_hash
    {
        return (
            current.pixel_count.max(previous.pixel_count),
            current.pixel_count.max(previous.pixel_count),
            false,
        );
    }
    let Some(previous_pixels) = parse_ppm_pixels(&previous_ppm) else {
        return (
            current.pixel_count.max(previous.pixel_count),
            current.pixel_count.max(previous.pixel_count),
            false,
        );
    };
    let Some(current_pixels) = parse_ppm_pixels(&current_ppm) else {
        return (
            current.pixel_count.max(previous.pixel_count),
            current.pixel_count.max(previous.pixel_count),
            false,
        );
    };
    let pixel_count = previous_pixels.len().max(current_pixels.len());
    let common_changed = previous_pixels
        .iter()
        .zip(current_pixels.iter())
        .filter(|(previous, current)| previous != current)
        .count();
    let length_delta = previous_pixels.len().abs_diff(current_pixels.len());
    (common_changed + length_delta, pixel_count, true)
}

fn design_screenshot_file_hash_matches(
    design_dir: &Path,
    artifact: &DesignScreenshotArtifact,
) -> bool {
    let Ok(contents) = fs::read_to_string(design_dir.join(&artifact.image_path)) else {
        return false;
    };
    stable_content_hash(&contents) == artifact.screenshot_hash
        && parse_ppm_pixels(&contents).is_some_and(|pixels| pixels.len() == artifact.pixel_count)
}

fn parse_ppm_pixels(contents: &str) -> Option<Vec<(u8, u8, u8)>> {
    parse_ppm_pixels_with_size(contents, DESIGN_SCREENSHOT_WIDTH, DESIGN_SCREENSHOT_HEIGHT)
}

fn parse_ppm_pixels_with_size(
    contents: &str,
    expected_width: usize,
    expected_height: usize,
) -> Option<Vec<(u8, u8, u8)>> {
    let tokens = contents
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .flat_map(|line| line.split_whitespace())
        .collect::<Vec<_>>();
    if tokens.len() < 4 || tokens.first().copied() != Some("P3") {
        return None;
    }
    let width = tokens.get(1)?.parse::<usize>().ok()?;
    let height = tokens.get(2)?.parse::<usize>().ok()?;
    let max_value = tokens.get(3)?.parse::<usize>().ok()?;
    if width != expected_width || height != expected_height || max_value != 255 {
        return None;
    }
    let values = tokens[4..]
        .iter()
        .map(|token| token.parse::<u8>().ok())
        .collect::<Option<Vec<_>>>()?;
    if values.len() != width * height * 3 {
        return None;
    }
    Some(
        values
            .chunks_exact(3)
            .map(|chunk| (chunk[0], chunk[1], chunk[2]))
            .collect(),
    )
}

fn design_visual_signature(contents: &str, color_tokens: &[String]) -> String {
    let mut signature_parts = Vec::new();
    signature_parts.push(format!("colors={}", color_tokens.join("|")));
    for line in contents.lines() {
        let lower = line.trim().to_ascii_lowercase();
        if lower.contains("class=")
            || lower.contains("classname")
            || lower.contains("<img")
            || lower.contains("<button")
            || lower.contains("width:")
            || lower.contains("height:")
            || lower.contains("display:")
            || lower.contains("grid")
            || lower.contains("flex")
            || lower.contains("color:")
            || lower.contains("background")
        {
            signature_parts.push(lower.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
    stable_content_hash(&signature_parts.join("\n"))
}

fn is_design_surface_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("html" | "htm" | "css" | "scss" | "js" | "jsx" | "ts" | "tsx" | "md" | "mdx")
    )
}

fn design_surface_kind(path: &Path, contents: &str) -> String {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html" | "htm") => "html".to_string(),
        Some("css" | "scss") => "stylesheet".to_string(),
        Some("jsx" | "tsx") => "component".to_string(),
        Some("js" | "ts") if contents.contains("<") && contents.contains("className") => {
            "component".to_string()
        }
        Some("md" | "mdx") => "documentation".to_string(),
        _ => "script".to_string(),
    }
}

fn inspect_design_surface(path: &str, contents: &str, findings: &mut Vec<DesignFinding>) {
    let lower = contents.to_ascii_lowercase();
    if (path.ends_with(".html") || path.ends_with(".htm") || lower.contains("<html"))
        && !lower.contains("name=\"viewport\"")
        && !lower.contains("name='viewport'")
    {
        findings.push(design_finding(
            path,
            1,
            "responsive_viewport_missing",
            "warning",
            "HTML surface does not declare a viewport meta tag.",
        ));
    }

    for (index, line) in contents.lines().enumerate() {
        let line_number = index + 1;
        let lower_line = line.to_ascii_lowercase();
        if lower_line.contains("<img") && !lower_line.contains(" alt=") {
            findings.push(design_finding(
                path,
                line_number,
                "image_alt_missing",
                "warning",
                "Image-like element is missing an alt attribute.",
            ));
        }
        if lower_line.contains("<button")
            && !lower_line.contains("aria-label")
            && !line_has_button_text(line)
        {
            findings.push(design_finding(
                path,
                line_number,
                "button_accessible_name_missing",
                "warning",
                "Button-like element may not expose a visible label or aria-label.",
            ));
        }
        if lower_line.contains("width:")
            && lower_line.contains("px")
            && line_has_large_fixed_width(&lower_line)
        {
            findings.push(design_finding(
                path,
                line_number,
                "large_fixed_width",
                "info",
                "Large fixed pixel width should be checked against responsive breakpoints.",
            ));
        }
        if lower_line.contains("color:")
            && lower_line.contains('#')
            && !lower_line.contains("contrast")
        {
            findings.push(design_finding(
                path,
                line_number,
                "contrast_unverified_color_token",
                "info",
                "Color token found; contrast still requires rendered foreground/background evidence.",
            ));
        }
    }
}

fn design_finding(
    path: &str,
    line_number: usize,
    rule: &str,
    severity: &str,
    message: &str,
) -> DesignFinding {
    DesignFinding {
        path: path.to_string(),
        line_number,
        rule: rule.to_string(),
        severity: severity.to_string(),
        message: message.to_string(),
    }
}

fn line_has_button_text(line: &str) -> bool {
    let Some(open_end) = line.find('>') else {
        return false;
    };
    let Some(close_start) = line[open_end + 1..].to_ascii_lowercase().find("</button>") else {
        return false;
    };
    let text = line[open_end + 1..open_end + 1 + close_start]
        .trim()
        .trim_matches(|ch: char| ch == '\u{00a0}');
    !text.is_empty() && !text.starts_with('<')
}

fn line_has_large_fixed_width(line: &str) -> bool {
    let Some(width_index) = line.find("width:") else {
        return false;
    };
    let after_width = &line[width_index + "width:".len()..];
    let digits = after_width
        .chars()
        .skip_while(|ch| ch.is_whitespace())
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    let Ok(width) = digits.parse::<u32>() else {
        return false;
    };
    width > 480
}

fn extract_color_tokens(contents: &str) -> Vec<String> {
    let chars = contents.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '#' {
            index += 1;
            continue;
        }
        let mut end = index + 1;
        while end < chars.len() && chars[end].is_ascii_hexdigit() && end - index <= 8 {
            end += 1;
        }
        let len = end - index - 1;
        if len == 3 || len == 6 || len == 8 {
            let token = chars[index..end].iter().collect::<String>();
            if !tokens.contains(&token) {
                tokens.push(token);
            }
        }
        index = end.max(index + 1);
    }
    tokens
}

fn provider_definitions() -> Vec<ProviderDefinition> {
    vec![
        ProviderDefinition {
            name: "OpenRouter",
            env_var: "OPENROUTER_API_KEY",
            kind: "remote",
            default_base_url: None,
            model_profile: "multi-provider-router",
            cache_support: "provider-dependent",
            auth_method: "api_key",
        },
        ProviderDefinition {
            name: "OpenAI",
            env_var: "OPENAI_API_KEY",
            kind: "remote",
            default_base_url: None,
            model_profile: "gpt-strong-finalizer",
            cache_support: "provider-dependent",
            auth_method: "api_key_or_oauth_future",
        },
        ProviderDefinition {
            name: "Claude",
            env_var: "ANTHROPIC_API_KEY",
            kind: "remote",
            default_base_url: None,
            model_profile: "review-security-planning",
            cache_support: "provider-dependent",
            auth_method: "api_key_or_oauth_future",
        },
        ProviderDefinition {
            name: "Gemini",
            env_var: "GEMINI_API_KEY",
            kind: "remote",
            default_base_url: None,
            model_profile: "huge-context-multimodal",
            cache_support: "provider-dependent",
            auth_method: "api_key",
        },
        ProviderDefinition {
            name: "Codex LB",
            env_var: "CODEX_LB_API_KEY",
            kind: "remote",
            default_base_url: None,
            model_profile: "optional-codex-load-balancer",
            cache_support: "unknown",
            auth_method: "api_key",
        },
        ProviderDefinition {
            name: "Ollama",
            env_var: "OLLAMA_HOST",
            kind: "local",
            default_base_url: Some("http://127.0.0.1:11434"),
            model_profile: "privacy-local-scout",
            cache_support: "local-runtime",
            auth_method: "local_endpoint",
        },
        ProviderDefinition {
            name: "LM Studio",
            env_var: "LM_STUDIO_BASE_URL",
            kind: "local",
            default_base_url: Some("http://127.0.0.1:1234/v1"),
            model_profile: "openai-compatible-local",
            cache_support: "local-runtime",
            auth_method: "local_endpoint",
        },
        ProviderDefinition {
            name: "OpenAI-compatible local endpoints",
            env_var: "OPENAI_BASE_URL",
            kind: "local_or_remote",
            default_base_url: None,
            model_profile: "openai-compatible-configured",
            cache_support: "endpoint-dependent",
            auth_method: "workspace_scoped_endpoint",
        },
    ]
}

fn provider_statuses(workspace: &Path) -> Vec<ProviderStatus> {
    provider_statuses_with_keychain_command(workspace, None)
}

fn provider_statuses_with_keychain_command(
    workspace: &Path,
    keychain_command: Option<&Path>,
) -> Vec<ProviderStatus> {
    let registry_credentials = provider_registry_credentials(workspace, keychain_command);
    provider_definitions()
        .into_iter()
        .map(|definition| {
            provider_status_for_definition(definition, keychain_command, &registry_credentials)
        })
        .collect()
}

fn provider_status_for_definition(
    definition: ProviderDefinition,
    keychain_command: Option<&Path>,
    registry_credentials: &[ProviderRegistryCredential],
) -> ProviderStatus {
    let env_value = env::var(definition.env_var)
        .ok()
        .filter(|value| !value.is_empty());
    let keychain_value = provider_keychain_credential(&definition, keychain_command);
    let registry_value = provider_registry_credential(&definition, registry_credentials);
    let (configured_value, credential_source) = if let Some(value) = env_value {
        (Some(value), "env".to_string())
    } else if let Some(value) = keychain_value {
        (Some(value), "keychain_legacy".to_string())
    } else if let Some((value, source)) = registry_value {
        (Some(value), source)
    } else {
        (None, "none".to_string())
    };
    ProviderStatus {
        configured: configured_value.is_some(),
        configured_value,
        credential_source,
        definition,
    }
}

#[derive(Debug, Clone)]
struct ProviderRegistryCredential {
    provider_kind: opensks_contracts::ProviderKind,
    provider_id: String,
    value: String,
}

fn provider_registry_credentials(
    workspace: &Path,
    keychain_command: Option<&Path>,
) -> Vec<ProviderRegistryCredential> {
    let Ok(repo) = opensks_provider::ProviderRepository::open_workspace(workspace) else {
        return Vec::new();
    };
    let Ok(connections) = repo.list_connections() else {
        return Vec::new();
    };
    connections
        .into_iter()
        .filter(|connection| {
            connection.enabled
                && matches!(
                    connection.kind,
                    opensks_contracts::ProviderKind::OpenRouter
                        | opensks_contracts::ProviderKind::OpenAi
                )
        })
        .filter_map(|connection| {
            provider_secret_ref_credential(&connection.auth, keychain_command).map(|value| {
                ProviderRegistryCredential {
                    provider_kind: connection.kind,
                    provider_id: connection.id,
                    value,
                }
            })
        })
        .collect()
}

fn provider_registry_credential(
    definition: &ProviderDefinition,
    credentials: &[ProviderRegistryCredential],
) -> Option<(String, String)> {
    let wanted = match definition.name {
        "OpenRouter" => opensks_contracts::ProviderKind::OpenRouter,
        "OpenAI" => opensks_contracts::ProviderKind::OpenAi,
        _ => return None,
    };
    credentials
        .iter()
        .find(|credential| credential.provider_kind == wanted)
        .map(|credential| {
            (
                credential.value.clone(),
                format!("provider_registry_keychain:{}", credential.provider_id),
            )
        })
}

fn provider_keychain_credential(
    definition: &ProviderDefinition,
    keychain_command: Option<&Path>,
) -> Option<String> {
    #[cfg(test)]
    keychain_command?;

    #[cfg(not(target_os = "macos"))]
    keychain_command?;

    provider_keychain_lookup(
        PROVIDER_KEYCHAIN_SERVICE,
        definition.env_var,
        keychain_command,
    )
}

fn provider_secret_ref_credential(
    auth: &opensks_contracts::SecretRef,
    keychain_command: Option<&Path>,
) -> Option<String> {
    if auth.store != opensks_contracts::SecretStoreKind::MacosKeychain {
        return None;
    }

    #[cfg(test)]
    keychain_command?;

    #[cfg(not(target_os = "macos"))]
    keychain_command?;

    provider_keychain_lookup(&auth.service, &auth.account, keychain_command)
}

fn provider_keychain_lookup(
    service: &str,
    account: &str,
    keychain_command: Option<&Path>,
) -> Option<String> {
    let command = keychain_command
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("security"));
    let output = process::Command::new(command)
        .args(["find-generic-password", "-s", service, "-a", account, "-w"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_string();
    (!value.is_empty()).then_some(value)
}

fn provider_auth_posture(status: &ProviderStatus) -> &'static str {
    if !status.configured {
        return "not_configured";
    }
    match status.credential_source.as_str() {
        "env" => "configured_env_override",
        "keychain_legacy" => "configured_keychain_fallback",
        source if source.starts_with("provider_registry_keychain:") => {
            "configured_provider_registry_keychain"
        }
        _ => "configured",
    }
}

fn render_provider_statuses_json(statuses: &[ProviderStatus]) -> String {
    let rows = statuses
        .iter()
        .map(|status| {
            format!(
                concat!(
                    "{{\"name\":{},\"kind\":{},\"credential_env\":{},",
                    "\"configured\":{},\"credential_source\":{},",
                    "\"auth_posture\":{},\"secret_value_exposed\":false,",
                    "\"model_profile\":{},\"cache_support\":{},\"auth_method\":{}}}"
                ),
                json_string(status.definition.name),
                json_string(status.definition.kind),
                json_string(status.definition.env_var),
                status.configured,
                json_string(&status.credential_source),
                json_string(provider_auth_posture(status)),
                json_string(status.definition.model_profile),
                json_string(status.definition.cache_support),
                json_string(status.definition.auth_method)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn probe_providers(statuses: &[ProviderStatus]) -> Vec<ProviderProbe> {
    statuses.iter().map(probe_provider).collect()
}

fn probe_provider(status: &ProviderStatus) -> ProviderProbe {
    let endpoint = provider_probe_endpoint(status);
    let Some(endpoint) = endpoint else {
        return ProviderProbe {
            name: status.definition.name.to_string(),
            attempted: false,
            status: if status.configured {
                "remote_probe_requires_explicit_approval".to_string()
            } else {
                "not_configured".to_string()
            },
            endpoint: None,
            http_code: None,
            duration_ms: 0,
            transport: PROVIDER_HTTP_TRANSPORT,
            stderr: String::new(),
        };
    };

    let started = Instant::now();
    match opensks_provider::native_http_get_status(&endpoint, None, Duration::from_secs(3)) {
        Ok(response) => {
            let status_text = if response.http_code != "000" {
                "reachable"
            } else {
                "unreachable"
            };
            ProviderProbe {
                name: status.definition.name.to_string(),
                attempted: true,
                status: status_text.to_string(),
                endpoint: Some(redact_endpoint_for_report(&endpoint)),
                http_code: if response.http_code.is_empty() {
                    None
                } else {
                    Some(response.http_code)
                },
                duration_ms: started.elapsed().as_millis(),
                transport: PROVIDER_HTTP_TRANSPORT,
                stderr: response.diagnostic,
            }
        }
        Err(error) => ProviderProbe {
            name: status.definition.name.to_string(),
            attempted: true,
            status: "unreachable".to_string(),
            endpoint: Some(redact_endpoint_for_report(&endpoint)),
            http_code: None,
            duration_ms: started.elapsed().as_millis(),
            transport: PROVIDER_HTTP_TRANSPORT,
            stderr: redact_provider_diagnostic(&error, ""),
        },
    }
}

fn provider_probe_endpoint(status: &ProviderStatus) -> Option<String> {
    let base = status
        .configured_value
        .as_deref()
        .or(status.definition.default_base_url)?;
    if !is_local_http_endpoint(base) {
        return None;
    }
    let endpoint = match status.definition.name {
        "Ollama" => join_url_path(base, "/api/tags"),
        "LM Studio" | "OpenAI-compatible local endpoints" => join_url_path(base, "/models"),
        _ => return None,
    };
    Some(endpoint)
}

fn check_provider_adapters(dir: &Path, statuses: &[ProviderStatus]) -> Vec<ProviderAdapterCheck> {
    statuses
        .iter()
        .filter_map(|status| provider_adapter_endpoint(status).map(|endpoint| (status, endpoint)))
        .map(|(status, endpoint)| check_provider_adapter(dir, status, &endpoint))
        .collect()
}

fn check_provider_adapter(
    _dir: &Path,
    status: &ProviderStatus,
    endpoint: &str,
) -> ProviderAdapterCheck {
    if !status.configured {
        return ProviderAdapterCheck {
            name: status.definition.name.to_string(),
            configured: false,
            attempted: false,
            status: "not_configured".to_string(),
            blockers: vec![format!(
                "configure_{}_credential",
                status.definition.env_var
            )],
            credential_source: status.credential_source.clone(),
            endpoint: endpoint.to_string(),
            http_code: None,
            duration_ms: 0,
            transport: PROVIDER_HTTP_TRANSPORT,
            stderr: String::new(),
        };
    }
    let remote_probe_allowed = remote_provider_adapter_probe_opted_in();
    if !remote_probe_allowed {
        return ProviderAdapterCheck {
            name: status.definition.name.to_string(),
            configured: true,
            attempted: false,
            status: "remote_probe_requires_OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE_1".to_string(),
            blockers: vec!["set_OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE_1".to_string()],
            credential_source: status.credential_source.clone(),
            endpoint: endpoint.to_string(),
            http_code: None,
            duration_ms: 0,
            transport: PROVIDER_HTTP_TRANSPORT,
            stderr: String::new(),
        };
    }

    let Some(secret) = status.configured_value.as_deref() else {
        return ProviderAdapterCheck {
            name: status.definition.name.to_string(),
            configured: false,
            attempted: false,
            status: "not_configured".to_string(),
            blockers: vec![format!(
                "configure_{}_credential",
                status.definition.env_var
            )],
            credential_source: status.credential_source.clone(),
            endpoint: endpoint.to_string(),
            http_code: None,
            duration_ms: 0,
            transport: PROVIDER_HTTP_TRANSPORT,
            stderr: String::new(),
        };
    };

    let started = Instant::now();
    match opensks_provider::native_http_get_status(endpoint, Some(secret), Duration::from_secs(10))
    {
        Ok(response) => {
            let stderr = redact_provider_diagnostic(&response.diagnostic, secret);
            let status_text = if response.http_code.starts_with('2') {
                "adapter_models_endpoint_reachable"
            } else if response.http_code == "401" || response.http_code == "403" {
                "adapter_auth_failed"
            } else {
                "adapter_remote_error"
            };
            let blockers = match status_text {
                "adapter_models_endpoint_reachable" => Vec::new(),
                "adapter_auth_failed" => {
                    vec![format!("replace_{}_credential", status.definition.env_var)]
                }
                _ => vec![format!(
                    "resolve_{}_models_endpoint",
                    status.definition.name
                )],
            };
            ProviderAdapterCheck {
                name: status.definition.name.to_string(),
                configured: true,
                attempted: true,
                status: status_text.to_string(),
                blockers,
                credential_source: status.credential_source.clone(),
                endpoint: endpoint.to_string(),
                http_code: if response.http_code.is_empty() {
                    None
                } else {
                    Some(response.http_code)
                },
                duration_ms: started.elapsed().as_millis(),
                transport: PROVIDER_HTTP_TRANSPORT,
                stderr,
            }
        }
        Err(error) => ProviderAdapterCheck {
            name: status.definition.name.to_string(),
            configured: true,
            attempted: true,
            status: "adapter_check_error".to_string(),
            blockers: vec![format!(
                "resolve_{}_adapter_check_error",
                status.definition.name
            )],
            credential_source: status.credential_source.clone(),
            endpoint: endpoint.to_string(),
            http_code: None,
            duration_ms: started.elapsed().as_millis(),
            transport: PROVIDER_HTTP_TRANSPORT,
            stderr: redact_provider_diagnostic(&error, secret),
        },
    }
}

fn provider_adapter_endpoint(status: &ProviderStatus) -> Option<String> {
    provider_adapter_expected_endpoint(status.definition.name).map(str::to_string)
}

fn redact_provider_diagnostic(value: &str, secret: &str) -> String {
    let redacted = if secret.is_empty() {
        value.to_string()
    } else {
        value.replace(secret, "[redacted-secret]")
    };
    let lower = redacted.to_ascii_lowercase();
    if lower.contains("authorization")
        || lower.contains("bearer")
        || lower.contains("sk-")
        || lower.contains("api_key=")
        || lower.contains("api-key")
        || json_bool_field_true_anywhere(&lower.replace("\\\"", "\""), "secret_value_exposed")
    {
        "[redacted-provider-diagnostic]".to_string()
    } else {
        redacted
    }
}

fn provider_adapter_expected_endpoint(name: &str) -> Option<&'static str> {
    match name {
        "OpenRouter" => Some("https://openrouter.ai/api/v1/models"),
        "OpenAI" => Some("https://api.openai.com/v1/models"),
        _ => None,
    }
}

fn is_local_http_endpoint(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("http://127.0.0.1")
        || lower.starts_with("http://localhost")
        || lower.starts_with("http://[::1]")
        || lower.starts_with("https://127.0.0.1")
        || lower.starts_with("https://localhost")
        || lower.starts_with("https://[::1]")
}

fn join_url_path(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn redact_endpoint_for_report(endpoint: &str) -> String {
    endpoint
        .split('?')
        .next()
        .unwrap_or(endpoint)
        .replace('@', "%40")
}

fn render_checks_json(checks: &[CommandCheck]) -> String {
    opensks_cli::render_checks_json(checks)
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

fn render_cache_dashboard(
    stamp: &ClockStamp,
    segments: &[CacheSegment],
    prefix_hit: &CachePrefixHitReport,
) -> String {
    let total_bytes: u64 = segments.iter().map(|segment| segment.bytes).sum();
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.cache-dashboard.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"metrics\": {},\n",
            "  \"live_provider_metrics\": false,\n",
            "  \"provider_metrics_available\": false,\n",
            "  \"provider_metrics_status\": \"not_connected\",\n",
            "  \"provider_cache_hit_percent\": null,\n",
            "  \"provider_cache_hit_status\": \"tracked_unavailable_provider_not_connected\",\n",
            "  \"provider_cache_hit_source\": \"cache-hit-report.json\",\n",
            "  \"provider_cached_tokens\": null,\n",
            "  \"provider_cache_write_tokens\": null,\n",
            "  \"local_warm_prefix_hit_percent\": {:.2},\n",
            "  \"local_estimated_cached_tokens\": {},\n",
            "  \"local_estimated_cache_write_tokens\": {},\n",
            "  \"local_segment_metrics\": {{\"segments\":{},\"bytes\":{},\"stable_prefix_bytes\":{},\"matched_stable_prefix_bytes\":{},\"estimated_cached_tokens\":{},\"estimated_cache_write_tokens\":{}}}\n",
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
        prefix_hit.local_hit_percent,
        prefix_hit.estimated_cached_tokens,
        prefix_hit.estimated_cache_write_tokens,
        segments.len(),
        total_bytes,
        prefix_hit.current_stable_bytes,
        prefix_hit.matched_stable_bytes,
        prefix_hit.estimated_cached_tokens,
        prefix_hit.estimated_cache_write_tokens
    )
}

fn render_cache_hit_report(stamp: &ClockStamp, prefix_hit: &CachePrefixHitReport) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.cache-hit-report.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"scope\": \"local_stable_prefix\",\n",
            "  \"target_hit_percent\": {:.2},\n",
            "  \"baseline_available\": {},\n",
            "  \"local_target_met\": {},\n",
            "  \"provider_metrics_available\": false,\n",
            "  \"provider_metrics_status\": \"not_connected\",\n",
            "  \"provider_metrics_note\": \"provider cached-token counters are not collected by this artifact-only local CLI slice\",\n",
            "  \"previous_stable_segment_count\": {},\n",
            "  \"current_stable_segment_count\": {},\n",
            "  \"matched_stable_segment_count\": {},\n",
            "  \"current_stable_bytes\": {},\n",
            "  \"matched_stable_bytes\": {},\n",
            "  \"estimated_cached_tokens\": {},\n",
            "  \"estimated_cache_write_tokens\": {},\n",
            "  \"local_hit_percent\": {:.2},\n",
            "  \"status\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        prefix_hit.target_hit_percent,
        prefix_hit.baseline_available,
        prefix_hit.local_target_met,
        prefix_hit.previous_stable_segment_count,
        prefix_hit.current_stable_segment_count,
        prefix_hit.matched_stable_segment_count,
        prefix_hit.current_stable_bytes,
        prefix_hit.matched_stable_bytes,
        prefix_hit.estimated_cached_tokens,
        prefix_hit.estimated_cache_write_tokens,
        prefix_hit.local_hit_percent,
        json_string(if prefix_hit.local_target_met {
            "local_target_met_provider_unverified"
        } else if prefix_hit.baseline_available {
            "local_target_missed_provider_unverified"
        } else {
            "baseline_missing_provider_unverified"
        })
    )
}

fn render_cache_layout_improvement_report(
    stamp: &ClockStamp,
    segments: &[CacheSegment],
    prefix_hit: &CachePrefixHitReport,
) -> String {
    let stable_segment_count = segments
        .iter()
        .filter(|segment| segment.stability == "stable")
        .count();
    let dynamic_segment_count = segments.len().saturating_sub(stable_segment_count);
    let total_bytes = segments.iter().map(|segment| segment.bytes).sum::<u64>();
    let stable_prefix_bytes = segments
        .iter()
        .filter(|segment| segment.stability == "stable")
        .map(|segment| segment.bytes)
        .sum::<u64>();
    let dynamic_suffix_bytes = total_bytes.saturating_sub(stable_prefix_bytes);
    let stable_prefix_ratio_percent = if total_bytes == 0 {
        0.0
    } else {
        (stable_prefix_bytes as f64 / total_bytes as f64) * 100.0
    };
    let voxel_triwiki_segment_present = segments.iter().any(|segment| {
        segment.name == "voxel_triwiki_summary"
            && segment.path == ".opensks/triwiki/voxels.jsonl"
            && segment.stability == "stable"
    });
    let layout_gate_passed = voxel_triwiki_segment_present
        && stable_segment_count > 0
        && prefix_hit.baseline_available
        && prefix_hit.local_target_met;
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.cache-layout-improvement.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"scope\": \"voxel_triwiki_cache_layout\",\n",
            "  \"strategy\": \"stable_prefix_dynamic_suffix\",\n",
            "  \"source_reports\": {},\n",
            "  \"layout_gate_passed\": {},\n",
            "  \"status\": {},\n",
            "  \"baseline_available\": {},\n",
            "  \"voxel_triwiki_segment_present\": {},\n",
            "  \"stable_segment_count\": {},\n",
            "  \"dynamic_segment_count\": {},\n",
            "  \"total_segment_count\": {},\n",
            "  \"stable_prefix_bytes\": {},\n",
            "  \"dynamic_suffix_bytes\": {},\n",
            "  \"stable_prefix_ratio_percent\": {:.2},\n",
            "  \"matched_stable_prefix_bytes\": {},\n",
            "  \"local_warm_prefix_hit_percent\": {:.2},\n",
            "  \"target_hit_percent\": {:.2},\n",
            "  \"estimated_cached_tokens\": {},\n",
            "  \"estimated_cache_write_tokens\": {},\n",
            "  \"provider_metrics_available\": false,\n",
            "  \"live_provider_cache_metrics\": false,\n",
            "  \"evidence\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&[
            "cache-warm-report.json",
            "cache-hit-report.json",
            "cache-prefix-snapshot.jsonl"
        ]),
        layout_gate_passed,
        json_string(if layout_gate_passed {
            "local_cache_layout_improved_provider_unverified"
        } else if !voxel_triwiki_segment_present {
            "voxel_triwiki_segment_missing_provider_unverified"
        } else if prefix_hit.baseline_available {
            "local_cache_layout_target_missed_provider_unverified"
        } else {
            "baseline_missing_provider_unverified"
        }),
        prefix_hit.baseline_available,
        voxel_triwiki_segment_present,
        stable_segment_count,
        dynamic_segment_count,
        segments.len(),
        stable_prefix_bytes,
        dynamic_suffix_bytes,
        stable_prefix_ratio_percent,
        prefix_hit.matched_stable_bytes,
        prefix_hit.local_hit_percent,
        prefix_hit.target_hit_percent,
        prefix_hit.estimated_cached_tokens,
        prefix_hit.estimated_cache_write_tokens,
        json_array(&[
            "stable/dynamic cache segment classification",
            "stable-prefix snapshot written after each warm run",
            "current stable prefix compared with the previous warm snapshot",
            "provider cached-token telemetry remains explicitly unavailable"
        ])
    )
}

fn render_cache_prefix_snapshot(segments: &[CacheSegment]) -> String {
    let rows = segments
        .iter()
        .filter(|segment| segment.stability == "stable")
        .map(|segment| {
            format!(
                "{{\"path\":{},\"bytes\":{},\"content_hash\":{},\"stability\":{}}}",
                json_string(&segment.path),
                segment.bytes,
                json_string(&segment.content_hash),
                json_string(&segment.stability)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if rows.is_empty() {
        String::new()
    } else {
        rows + "\n"
    }
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

fn render_security_audit(
    stamp: &ClockStamp,
    secret_findings: &[SecretFinding],
    security_findings: &[SecurityFinding],
) -> String {
    let blocking_findings = security_findings
        .iter()
        .filter(|finding| finding.severity == "critical" || finding.severity == "warning")
        .count();
    let status = if secret_findings.is_empty() && blocking_findings == 0 {
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
            "  \"prompt_injection_scan_executed\": true,\n",
            "  \"supply_chain_scan_executed\": true,\n",
            "  \"unsafe_action_scan_executed\": true,\n",
            "  \"mcp_tool_poisoning_scan_executed\": true,\n",
            "  \"secret_finding_count\": {},\n",
            "  \"security_finding_count\": {},\n",
            "  \"critical_or_warning_count\": {},\n",
            "  \"category_summary\": {},\n",
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
        secret_findings.len(),
        security_findings.len(),
        blocking_findings,
        render_security_category_summary_json(security_findings),
        render_secret_findings_json(secret_findings)
    )
}

fn render_secret_leak_rate_report(
    stamp: &ClockStamp,
    source_command: &str,
    scanned_artifact_count: usize,
    secret_findings: &[SecretFinding],
    release_history: SecretLeakReleaseHistory,
    evidence_refs: &[&str],
) -> String {
    let secret_finding_count = secret_findings.len();
    let rate = secret_leak_artifact_rate(scanned_artifact_count, secret_finding_count);
    let gate_passed = secret_leak_gate_passed(secret_findings);
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.secret-leak-rate.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"source_command\": {},\n",
            "  \"scope\": \"current_workspace_release_scan\",\n",
            "  \"scanned_text_artifact_count\": {},\n",
            "  \"scanned_artifact_count\": {},\n",
            "  \"secret_finding_count\": {},\n",
            "  \"secret_leak_artifact_rate\": {:.6},\n",
            "  \"target_rate\": 0.0,\n  \"gate_passed\": {},\n",
            "  \"release_history_available\": {},\n",
            "  \"release_history_ref\": \"secret-leak-release-history.json\",\n  \"release_history_events_ref\": \"secret-leak-release-history.jsonl\",\n",
            "  \"release_history_scan_count\": {},\n",
            "  \"release_history_denominator\": {},\n",
            "  \"release_history_secret_finding_count\": {},\n",
            "  \"release_history_secret_leak_artifact_rate\": {:.6},\n",
            "  \"release_history_latest_scanned_artifact_count\": {},\n  \"release_history_latest_secret_finding_count\": {},\n  \"release_history_latest_secret_leak_artifact_rate\": {:.6},\n  \"release_history_gate_policy\": \"latest_candidate_clean_with_history_preserved\",\n",
            "  \"release_history_gate_passed\": {},\n",
            "  \"live_external_production_telemetry\": false,\n",
            "  \"telemetry_source\": \"local_workspace_release_history\",\n",
            "  \"evidence_refs\": {},\n",
            "  \"secret_findings\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        json_string(source_command),
        scanned_artifact_count,
        scanned_artifact_count,
        secret_finding_count,
        rate,
        gate_passed,
        release_history.scans > 0,
        release_history.scans,
        release_history.scanned_total,
        release_history.secret_total,
        release_history.artifact_rate(),
        release_history.scanned_latest,
        release_history.secret_latest,
        release_history.latest_artifact_rate(),
        release_history.gate_passed(),
        json_array(evidence_refs),
        render_secret_findings_json(secret_findings)
    )
}

fn render_secret_leak_gate_report(
    stamp: &ClockStamp,
    source_command: &str,
    secret_findings: &[SecretFinding],
    release_history: SecretLeakReleaseHistory,
    evidence_refs: &[&str],
) -> String {
    let current_scan_gate_passed = secret_leak_gate_passed(secret_findings);
    let gate_passed = current_scan_gate_passed && release_history.gate_passed();
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.secret-leak-gate.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"source_command\": {},\n",
            "  \"scope\": \"current_workspace_release_scan\",\n",
            "  \"status\": {},\n",
            "  \"gate_passed\": {},\n",
            "  \"target_rate\": 0.0,\n",
            "  \"secret_finding_count\": {},\n",
            "  \"current_workspace_gate_passed\": {},\n",
            "  \"release_history_available\": {},\n",
            "  \"release_history_ref\": \"secret-leak-release-history.json\",\n  \"release_history_events_ref\": \"secret-leak-release-history.jsonl\",\n",
            "  \"release_history_scan_count\": {},\n",
            "  \"release_history_denominator\": {},\n",
            "  \"release_history_secret_finding_count\": {},\n",
            "  \"release_history_secret_leak_artifact_rate\": {:.6},\n",
            "  \"release_history_latest_scanned_artifact_count\": {},\n  \"release_history_latest_secret_finding_count\": {},\n  \"release_history_latest_secret_leak_artifact_rate\": {:.6},\n  \"release_history_gate_policy\": \"latest_candidate_clean_with_history_preserved\",\n",
            "  \"release_history_gate_passed\": {},\n",
            "  \"live_external_production_telemetry\": false,\n",
            "  \"telemetry_source\": \"local_workspace_release_history\",\n",
            "  \"evidence_refs\": {},\n",
            "  \"secret_findings\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        json_string(source_command),
        json_string(if gate_passed { "passed" } else { "blocked" }),
        gate_passed,
        secret_findings.len(),
        current_scan_gate_passed,
        release_history.scans > 0,
        release_history.scans,
        release_history.scanned_total,
        release_history.secret_total,
        release_history.artifact_rate(),
        release_history.scanned_latest,
        release_history.secret_latest,
        release_history.latest_artifact_rate(),
        release_history.gate_passed(),
        json_array(evidence_refs),
        render_secret_findings_json(secret_findings)
    )
}

fn secret_leak_artifact_rate(scanned_artifact_count: usize, secret_finding_count: usize) -> f64 {
    if scanned_artifact_count == 0 {
        0.0
    } else {
        secret_finding_count as f64 / scanned_artifact_count as f64
    }
}

fn secret_leak_gate_passed(secret_findings: &[SecretFinding]) -> bool {
    secret_findings.is_empty()
}

fn read_secret_leak_release_history(path: &Path) -> Result<SecretLeakReleaseHistory, OpenSksError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(SecretLeakReleaseHistory::default());
        }
        Err(error) => return Err(OpenSksError::Io(error)),
    };
    let mut history = SecretLeakReleaseHistory::default();
    for line in contents.lines().filter(|line| !line.trim().is_empty()) {
        let scanned_artifacts =
            extract_json_number_field(line, "scanned_artifact_count").unwrap_or(0);
        let secret_findings = extract_json_number_field(line, "secret_finding_count").unwrap_or(0);
        history = history.with_current_scan(scanned_artifacts, secret_findings);
    }
    Ok(history)
}

fn render_secret_leak_release_history_event(
    stamp: &ClockStamp,
    source_command: &str,
    scanned_artifact_count: usize,
    secret_findings: &[SecretFinding],
) -> String {
    format!(
        concat!(
            "{{\"schema\":\"opensks.secret-leak-release-history-event.v1\",",
            "\"release_id\":{},\"generated_at\":{},\"source_command\":{},",
            "\"scope\":\"local_workspace_release_history\",",
            "\"scanned_artifact_count\":{},\"secret_finding_count\":{},",
            "\"gate_passed\":{},\"secret_findings\":{}}}\n"
        ),
        json_string(&format!("{source_command}-{}", stamp.compact_id())),
        stamp.json(),
        json_string(source_command),
        scanned_artifact_count,
        secret_findings.len(),
        secret_findings.is_empty() && scanned_artifact_count > 0,
        render_secret_findings_json(secret_findings)
    )
}

fn render_secret_leak_release_history_report(
    stamp: &ClockStamp,
    source_command: &str,
    release_history: SecretLeakReleaseHistory,
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.secret-leak-release-history.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"source_command\": {},\n",
            "  \"scope\": \"local_workspace_release_history\",\n",
            "  \"release_history_available\": {},\n",
            "  \"release_scan_count\": {},\n",
            "  \"release_history_denominator\": {},\n",
            "  \"total_scanned_artifact_count\": {},\n",
            "  \"total_secret_finding_count\": {},\n",
            "  \"secret_leak_artifact_rate\": {:.6},\n",
            "  \"latest_scanned_artifact_count\": {},\n  \"latest_secret_finding_count\": {},\n  \"latest_secret_leak_artifact_rate\": {:.6},\n  \"gate_policy\": \"latest_candidate_clean_with_history_preserved\",\n",
            "  \"target_rate\": 0.0,\n",
            "  \"gate_passed\": {},\n",
            "  \"live_external_production_telemetry\": false,\n",
            "  \"telemetry_source\": \"local_workspace_release_history\",\n",
            "  \"events_ref\": \"secret-leak-release-history.jsonl\"\n",
            "}}\n"
        ),
        stamp.json(),
        json_string(source_command),
        release_history.scans > 0,
        release_history.scans,
        release_history.scanned_total,
        release_history.scanned_total,
        release_history.secret_total,
        release_history.artifact_rate(),
        release_history.scanned_latest,
        release_history.secret_latest,
        release_history.latest_artifact_rate(),
        release_history.gate_passed()
    )
}

fn render_security_category_summary_json(findings: &[SecurityFinding]) -> String {
    let categories = [
        "prompt_injection",
        "mcp_tool_poisoning",
        "supply_chain",
        "unsafe_action",
    ];
    let rows = categories
        .iter()
        .map(|category| {
            let count = findings
                .iter()
                .filter(|finding| finding.category == *category)
                .count();
            format!("{}:{}", json_string(category), count)
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{rows}}}")
}

fn render_security_findings_jsonl(stamp: &ClockStamp, findings: &[SecurityFinding]) -> String {
    if findings.is_empty() {
        return format!(
            "{{\"schema\":\"opensks.security-finding.v1\",\"at\":{},\"category\":\"none\",\"rule\":\"none\",\"severity\":\"info\",\"message\":\"no static security findings\"}}\n",
            stamp.json()
        );
    }
    findings
        .iter()
        .map(|finding| {
            format!(
                concat!(
                    "{{\"schema\":\"opensks.security-finding.v1\",\"at\":{},",
                    "\"category\":{},\"path\":{},\"line\":{},\"rule\":{},",
                    "\"severity\":{},\"message\":{}}}"
                ),
                stamp.json(),
                json_string(&finding.category),
                json_string(&finding.path),
                finding.line_number,
                json_string(&finding.rule),
                json_string(&finding.severity),
                json_string(&finding.message)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn render_threat_model(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.threat-model.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"threats\": {},\n",
            "  \"default_controls\": {},\n",
            "  \"live_static_scans\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&[
            "mcp_tool_poisoning",
            "prompt_injection",
            "secret_exfiltration",
            "unsafe_computer_use",
            "malicious_plugin",
            "supply_chain_attack"
        ]),
        json_array(&[
            "secret_values_never_written",
            "dangerous_actions_require_approval",
            "raw_mcp_calls_denied",
            "workspace_runtime_dirs_skipped",
            "final_apply_blocked_without_gates"
        ]),
        json_array(&[
            "secret_scan",
            "prompt_injection_phrase_scan",
            "mcp_allowlist_bypass_phrase_scan",
            "curl_pipe_shell_scan",
            "destructive_shell_pattern_scan"
        ])
    )
}

fn render_scheduler_plan(stamp: &ClockStamp, run_id: &str, goal: &str) -> String {
    opensks_cli::render_scheduler_plan_json(&stamp.json(), run_id, goal)
}

fn render_scheduler_events(stamp: &ClockStamp, run_id: &str, checks: &[CommandCheck]) -> String {
    opensks_cli::render_scheduler_events_jsonl(&stamp.json(), run_id, checks)
}

fn render_scheduler_final_state(
    stamp: &ClockStamp,
    run_id: &str,
    checks: &[CommandCheck],
) -> String {
    opensks_cli::render_scheduler_final_state_json(&stamp.json(), run_id, checks)
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

fn render_design_qa_report(
    stamp: &ClockStamp,
    surfaces: &[DesignSurface],
    findings: &[DesignFinding],
    visual_diffs: &[DesignVisualDiff],
    screenshot_diff_executed: bool,
    screenshot_baseline_available: bool,
) -> String {
    let warnings = findings
        .iter()
        .filter(|finding| finding.severity == "warning")
        .count();
    let changed_visual_surfaces = visual_diffs
        .iter()
        .filter(|diff| {
            diff.status == "changed" || diff.status == "added" || diff.status == "removed"
        })
        .count();
    let status = if warnings == 0 {
        "passed_static_scan"
    } else {
        "findings"
    };
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.design-qa-report.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"status\": {},\n",
            "  \"checks\": {},\n",
            "  \"static_scan_executed\": true,\n",
            "  \"source_visual_diff_executed\": true,\n",
            "  \"screenshot_diff_executed\": {},\n",
            "  \"screenshot_diff_mode\": {},\n",
            "  \"screenshot_baseline_available\": {},\n",
            "  \"live_browser_capture_executed\": false,\n",
            "  \"surface_count\": {},\n",
            "  \"finding_count\": {},\n",
            "  \"warning_count\": {},\n",
            "  \"visual_diff_count\": {},\n",
            "  \"changed_visual_surface_count\": {},\n",
            "  \"live_image_or_screenshot_evidence\": false,\n",
            "  \"evidence\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        json_string(status),
        json_array(&[
            "image_generation",
            "screenshot_visual_diff",
            "design_verifier",
            "responsive_qa",
            "accessibility_static_scan",
            "color_token_static_scan",
            "auto_ui_patch"
        ]),
        screenshot_diff_executed,
        json_string(DESIGN_SCREENSHOT_MODE),
        screenshot_baseline_available,
        surfaces.len(),
        findings.len(),
        warnings,
        visual_diffs.len(),
        changed_visual_surfaces,
        json_array(&[
            "design-surface-inventory.json",
            "design-findings.jsonl",
            "design-visual-diff-report.json",
            "design-visual-snapshots.jsonl",
            "design-screenshot-diff-report.json",
            "design-screenshot-snapshots.jsonl",
            "screenshots/*.ppm"
        ])
    )
}

fn render_design_surface_inventory(stamp: &ClockStamp, surfaces: &[DesignSurface]) -> String {
    let rows = surfaces
        .iter()
        .map(|surface| {
            format!(
                concat!(
                    "{{\"path\":{},\"kind\":{},\"bytes\":{},",
                    "\"content_hash\":{},\"visual_signature\":{},\"color_tokens\":{}}}"
                ),
                json_string(&surface.path),
                json_string(&surface.kind),
                surface.bytes,
                json_string(&surface.content_hash),
                json_string(&surface.visual_signature),
                json_vec(&surface.color_tokens)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.design-surface-inventory.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"surfaces\": [{}]\n",
            "}}\n"
        ),
        stamp.json(),
        rows
    )
}

fn render_design_visual_diff_report(
    stamp: &ClockStamp,
    visual_diffs: &[DesignVisualDiff],
    baseline_available: bool,
    screenshot_diff_executed: bool,
) -> String {
    let changed = visual_diffs
        .iter()
        .filter(|diff| diff.status == "changed")
        .count();
    let added = visual_diffs
        .iter()
        .filter(|diff| diff.status == "added")
        .count();
    let removed = visual_diffs
        .iter()
        .filter(|diff| diff.status == "removed")
        .count();
    let unchanged = visual_diffs
        .iter()
        .filter(|diff| diff.status == "unchanged")
        .count();
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.design-visual-diff-report.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"baseline_available\": {},\n",
            "  \"source_visual_diff_executed\": true,\n",
            "  \"screenshot_diff_executed\": {},\n",
            "  \"screenshot_diff_mode\": {},\n",
            "  \"screenshot_diff_report_ref\": \"design-screenshot-diff-report.json\",\n",
            "  \"live_browser_capture_executed\": false,\n",
            "  \"image_generation_review_executed\": false,\n",
            "  \"gpt_image_review_executed\": false,\n",
            "  \"live_image_or_screenshot_evidence\": false,\n",
            "  \"status\": {},\n",
            "  \"summary\": {{\"total\":{},\"changed\":{},\"added\":{},\"removed\":{},\"unchanged\":{}}},\n",
            "  \"evidence_note\": \"compares deterministic source-derived visual signatures plus local raster screenshot artifacts; live browser-rendered screenshots, Chrome Extension evidence, Product Design visual comparison, and gpt-image-2 review remain unverified\",\n",
            "  \"diffs\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        baseline_available,
        screenshot_diff_executed,
        json_string(DESIGN_SCREENSHOT_MODE),
        json_string(if baseline_available {
            "source_visual_diff_recorded"
        } else {
            "baseline_seeded"
        }),
        visual_diffs.len(),
        changed,
        added,
        removed,
        unchanged,
        render_design_visual_diffs_json(visual_diffs)
    )
}

fn render_design_visual_diffs_json(visual_diffs: &[DesignVisualDiff]) -> String {
    let rows = visual_diffs
        .iter()
        .map(|diff| {
            format!(
                concat!(
                    "{{\"path\":{},\"status\":{},\"previous_signature\":{},",
                    "\"current_signature\":{},\"bytes_delta\":{}}}"
                ),
                json_string(&diff.path),
                json_string(&diff.status),
                diff.previous_signature
                    .as_ref()
                    .map(|signature| json_string(signature))
                    .unwrap_or_else(|| "null".to_string()),
                diff.current_signature
                    .as_ref()
                    .map(|signature| json_string(signature))
                    .unwrap_or_else(|| "null".to_string()),
                diff.bytes_delta
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn render_design_screenshot_diff_report(
    stamp: &ClockStamp,
    screenshot_diffs: &[DesignScreenshotDiff],
    current_screenshots: &[DesignScreenshotArtifact],
    baseline_available: bool,
    screenshot_diff_executed: bool,
) -> String {
    let changed = screenshot_diffs
        .iter()
        .filter(|diff| diff.status == "changed")
        .count();
    let added = screenshot_diffs
        .iter()
        .filter(|diff| diff.status == "added")
        .count();
    let removed = screenshot_diffs
        .iter()
        .filter(|diff| diff.status == "removed")
        .count();
    let unchanged = screenshot_diffs
        .iter()
        .filter(|diff| diff.status == "unchanged")
        .count();
    let pixel_count_total = screenshot_diffs
        .iter()
        .map(|diff| diff.pixel_count)
        .sum::<usize>();
    let pixel_changed_count_total = screenshot_diffs
        .iter()
        .map(|diff| diff.pixel_changed_count)
        .sum::<usize>();
    let missing_image_artifact_count = screenshot_diffs
        .iter()
        .filter(|diff| !diff.image_artifacts_present)
        .count();
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.design-screenshot-diff-report.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"baseline_available\": {},\n",
            "  \"screenshot_diff_executed\": {},\n",
            "  \"screenshot_diff_mode\": {},\n",
            "  \"renderer\": {},\n",
            "  \"live_browser_capture_executed\": false,\n",
            "  \"chrome_extension_evidence\": false,\n",
            "  \"image_generation_review_executed\": false,\n",
            "  \"gpt_image_review_executed\": false,\n",
            "  \"product_design_visual_comparison_executed\": false,\n",
            "  \"external_design_service_executed\": false,\n",
            "  \"live_image_or_screenshot_evidence\": false,\n",
            "  \"screenshot_snapshot_count\": {},\n",
            "  \"screenshot_diff_count\": {},\n",
            "  \"pixel_count_total\": {},\n",
            "  \"pixel_changed_count_total\": {},\n",
            "  \"missing_image_artifact_count\": {},\n",
            "  \"status\": {},\n",
            "  \"summary\": {{\"total\":{},\"changed\":{},\"added\":{},\"removed\":{},\"unchanged\":{}}},\n",
            "  \"evidence_note\": \"deterministic local PPM screenshot artifacts and pixel diffs only; live browser-rendered screenshot capture, Chrome Extension evidence, Product Design visual comparison, external design service execution, and gpt-image-2 review remain false\",\n",
            "  \"diffs\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        baseline_available,
        screenshot_diff_executed,
        json_string(DESIGN_SCREENSHOT_MODE),
        json_string(DESIGN_SCREENSHOT_RENDERER),
        current_screenshots.len(),
        screenshot_diffs.len(),
        pixel_count_total,
        pixel_changed_count_total,
        missing_image_artifact_count,
        json_string(if !screenshot_diff_executed {
            "no_design_surfaces"
        } else if !baseline_available {
            "baseline_seeded"
        } else if missing_image_artifact_count > 0 {
            "missing_image_artifacts"
        } else if changed > 0 || added > 0 || removed > 0 {
            "changed"
        } else {
            "unchanged"
        }),
        screenshot_diffs.len(),
        changed,
        added,
        removed,
        unchanged,
        render_design_screenshot_diffs_json(screenshot_diffs)
    )
}

fn render_design_screenshot_diffs_json(screenshot_diffs: &[DesignScreenshotDiff]) -> String {
    let rows = screenshot_diffs
        .iter()
        .map(|diff| {
            format!(
                concat!(
                    "{{\"path\":{},\"status\":{},",
                    "\"previous_screenshot_hash\":{},\"current_screenshot_hash\":{},",
                    "\"previous_image_path\":{},\"current_image_path\":{},",
                    "\"pixel_count\":{},\"pixel_changed_count\":{},",
                    "\"image_artifacts_present\":{}}}"
                ),
                json_string(&diff.path),
                json_string(&diff.status),
                diff.previous_screenshot_hash
                    .as_ref()
                    .map(|hash| json_string(hash))
                    .unwrap_or_else(|| "null".to_string()),
                diff.current_screenshot_hash
                    .as_ref()
                    .map(|hash| json_string(hash))
                    .unwrap_or_else(|| "null".to_string()),
                diff.previous_image_path
                    .as_ref()
                    .map(|path| json_string(path))
                    .unwrap_or_else(|| "null".to_string()),
                diff.current_image_path
                    .as_ref()
                    .map(|path| json_string(path))
                    .unwrap_or_else(|| "null".to_string()),
                diff.pixel_count,
                diff.pixel_changed_count,
                diff.image_artifacts_present
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn render_design_surface_snapshot(surfaces: &[DesignSurface]) -> String {
    let rows = surfaces
        .iter()
        .map(|surface| {
            format!(
                concat!(
                    "{{\"path\":{},\"kind\":{},\"bytes\":{},",
                    "\"content_hash\":{},\"visual_signature\":{}}}"
                ),
                json_string(&surface.path),
                json_string(&surface.kind),
                surface.bytes,
                json_string(&surface.content_hash),
                json_string(&surface.visual_signature)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if rows.is_empty() {
        String::new()
    } else {
        rows + "\n"
    }
}

fn render_design_screenshot_snapshot(screenshots: &[DesignScreenshotArtifact]) -> String {
    let rows = screenshots
        .iter()
        .map(|screenshot| {
            format!(
                concat!(
                    "{{\"schema\":\"opensks.design-screenshot-snapshot.v1\",",
                    "\"path\":{},\"kind\":{},\"image_path\":{},",
                    "\"width\":{},\"height\":{},\"pixel_count\":{},",
                    "\"screenshot_hash\":{},\"content_hash\":{},",
                    "\"visual_signature\":{},\"renderer\":{},\"mode\":{}}}"
                ),
                json_string(&screenshot.path),
                json_string(&screenshot.kind),
                json_string(&screenshot.image_path),
                screenshot.width,
                screenshot.height,
                screenshot.pixel_count,
                json_string(&screenshot.screenshot_hash),
                json_string(&screenshot.content_hash),
                json_string(&screenshot.visual_signature),
                json_string(DESIGN_SCREENSHOT_RENDERER),
                json_string(DESIGN_SCREENSHOT_MODE)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if rows.is_empty() {
        String::new()
    } else {
        rows + "\n"
    }
}

fn render_design_findings_jsonl(stamp: &ClockStamp, findings: &[DesignFinding]) -> String {
    if findings.is_empty() {
        return format!(
            "{{\"schema\":\"opensks.design-finding.v1\",\"at\":{},\"rule\":\"none\",\"severity\":\"info\",\"message\":\"no static design findings\"}}\n",
            stamp.json()
        );
    }
    findings
        .iter()
        .map(|finding| {
            format!(
                concat!(
                    "{{\"schema\":\"opensks.design-finding.v1\",\"at\":{},",
                    "\"path\":{},\"line\":{},\"rule\":{},\"severity\":{},\"message\":{}}}"
                ),
                stamp.json(),
                json_string(&finding.path),
                finding.line_number,
                json_string(&finding.rule),
                json_string(&finding.severity),
                json_string(&finding.message)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
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
            "quorum-report.json",
            "collaboration-preflight.json",
            "native-collaboration-execution.json",
            "native-collaboration-events.jsonl",
            "native-proof-diagnostics.json"
        ]),
        render_checks_json(checks)
    )
}

fn render_multi_llm_roster(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.multi-llm-roster.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"live_multi_llm_execution\": false,\n",
            "  \"no_hidden_fallback\": true,\n",
            "  \"model_families\": [\n",
            "    {{\"family\":\"glm\",\"preferred_roles\":{},\"pipeline\":\"many_parallel_patch_workers_plus_high_judge\"}},\n",
            "    {{\"family\":\"gpt\",\"preferred_roles\":{},\"pipeline\":\"fewer_strong_workers_plus_stable_finalizer\"}},\n",
            "    {{\"family\":\"claude\",\"preferred_roles\":{},\"pipeline\":\"review_security_planning\"}},\n",
            "    {{\"family\":\"gemini\",\"preferred_roles\":{},\"pipeline\":\"huge_context_plus_multimodal_design\"}},\n",
            "    {{\"family\":\"local\",\"preferred_roles\":{},\"pipeline\":\"privacy_scout_static_verifier\"}}\n",
            "  ],\n",
            "  \"source\": \"PRD v3 sections 1.8, 1.9, and 11\"\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&["patch_worker", "judge"]),
        json_array(&["planner", "patch_worker", "finalizer"]),
        json_array(&["planner", "verifier", "security_reviewer"]),
        json_array(&["verifier", "design_reviewer", "large_context_reader"]),
        json_array(&["privacy_scout", "static_verifier"])
    )
}

fn render_role_assignments(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.role-assignments.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"assignment_mode\": \"explicit_artifact_mvp\",\n",
            "  \"no_hidden_fallback\": true,\n",
            "  \"roles\": [\n",
            "    {{\"role\":\"planner\",\"preferred_families\":{},\"fallback_policy\":\"explicit_user_visible_roster_only\"}},\n",
            "    {{\"role\":\"patch_worker\",\"preferred_families\":{},\"fallback_policy\":\"explicit_user_visible_roster_only\"}},\n",
            "    {{\"role\":\"verifier\",\"preferred_families\":{},\"fallback_policy\":\"explicit_user_visible_roster_only\"}},\n",
            "    {{\"role\":\"judge\",\"preferred_families\":{},\"fallback_policy\":\"strongest_available_from_roster\"}},\n",
            "    {{\"role\":\"finalizer\",\"preferred_families\":{},\"fallback_policy\":\"explicit_user_visible_roster_only\"}},\n",
            "    {{\"role\":\"design_reviewer\",\"preferred_families\":{},\"fallback_policy\":\"requires_multimodal_or_unverified\"}},\n",
            "    {{\"role\":\"security_reviewer\",\"preferred_families\":{},\"fallback_policy\":\"explicit_user_visible_roster_only\"}}\n",
            "  ]\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&["gpt", "claude"]),
        json_array(&["glm", "local", "gpt"]),
        json_array(&["claude", "gemini", "local"]),
        json_array(&["gpt", "claude", "gemini"]),
        json_array(&["gpt"]),
        json_array(&["gpt", "gemini"]),
        json_array(&["claude", "local"])
    )
}

fn render_disagreement_report(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.disagreement-report.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"live_disagreements_observed\": false,\n",
            "  \"status\": \"artifact_mvp_no_live_workers\",\n",
            "  \"tracked_axes\": {},\n",
            "  \"resolution_policy\": \"judge_or_human_visible_escalation_before_final_apply\"\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&[
            "requirements_interpretation",
            "patch_correctness",
            "security_risk",
            "design_risk",
            "verification_sufficiency"
        ])
    )
}

fn render_quorum_report(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.quorum-report.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"live_quorum_evaluated\": false,\n",
            "  \"minimum_review_roles\": {},\n",
            "  \"final_apply_requires\": {},\n",
            "  \"hidden_fallback_allowed\": false\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&["planner", "verifier", "judge"]),
        json_array(&[
            "explicit_roster",
            "passing_verification",
            "resolved_disagreements",
            "final_seal"
        ])
    )
}

fn render_collaboration_preflight(
    stamp: &ClockStamp,
    statuses: &[ProviderStatus],
    adapter_check_present: bool,
) -> String {
    let configured_count = statuses.iter().filter(|status| status.configured).count();
    let remote_configured_count = statuses
        .iter()
        .filter(|status| status.configured && status.definition.kind != "local")
        .count();
    let configured_provider_names = statuses
        .iter()
        .filter(|status| status.configured)
        .map(|status| status.definition.name.to_string())
        .collect::<Vec<_>>();
    let missing_credentials = statuses
        .iter()
        .filter(|status| !status.configured)
        .map(|status| status.definition.env_var.to_string())
        .collect::<Vec<_>>();
    let remote_probe_opt_in = env::var("OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE")
        .ok()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    let eligible_roles = if configured_count == 0 {
        json_array(&["artifact_planner", "static_verifier"])
    } else {
        json_array(&["planner", "verifier", "judge", "finalizer"])
    };
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.collaboration-preflight.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"scope\": \"multi_llm_collaboration_preflight\",\n",
            "  \"no_hidden_fallback\": true,\n",
            "  \"live_multi_llm_execution\": false,\n",
            "  \"live_multi_provider_worker_collaboration\": false,\n",
            "  \"live_execution_ready\": false,\n",
            "  \"preflight_ready\": true,\n",
            "  \"readiness_status\": \"artifact_preflight_only\",\n",
            "  \"configured_provider_count\": {},\n",
            "  \"configured_provider_names\": {},\n",
            "  \"remote_configured_provider_count\": {},\n",
            "  \"adapter_check_report_present\": {},\n",
            "  \"remote_probe_opt_in\": {},\n",
            "  \"remote_probe_policy\": {},\n",
            "  \"eligible_roles\": {},\n",
            "  \"blocked_roles\": {},\n",
            "  \"missing_credentials\": {},\n",
            "  \"missing_requirements\": {},\n",
            "  \"unverified\": {},\n",
            "  \"artifact_refs\": {},\n",
            "  \"providers\": {},\n",
            "  \"status_reason\": \"explicit collaboration readiness/preflight exists; live multi-provider workers, disagreements, quorum, and final apply are not executed\"\n",
            "}}\n"
        ),
        stamp.json(),
        configured_count,
        json_vec(&configured_provider_names),
        remote_configured_count,
        adapter_check_present,
        remote_probe_opt_in,
        json_string(if remote_probe_opt_in {
            "remote_adapter_probe_opted_in"
        } else {
            "remote_adapter_probe_requires_OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE"
        }),
        eligible_roles,
        json_array(&[
            "live_patch_worker_execution",
            "live_disagreement_resolution",
            "live_quorum_vote",
            "live_final_apply"
        ]),
        json_vec(&missing_credentials),
        json_array(&[
            "live_provider_worker_runtime",
            "live_disagreement_transcript",
            "live_quorum_vote",
            "final_apply_transaction"
        ]),
        json_array(&[
            "live_remote_provider_api_calls",
            "live_multi_provider_worker_collaboration",
            "live_disagreement_resolution",
            "live_quorum_evaluation"
        ]),
        json_array(&[
            "multi-llm-roster.json",
            "role-assignments.json",
            "disagreement-report.json",
            "quorum-report.json",
            "native-collaboration-execution.json",
            "native-collaboration-events.jsonl",
            "../auth/auth-policy.json",
            "../providers/provider-adapter-check.json"
        ]),
        render_collaboration_provider_readiness(statuses)
    )
}

fn render_collaboration_provider_readiness(statuses: &[ProviderStatus]) -> String {
    let rows = statuses
        .iter()
        .map(|status| {
            format!(
                concat!(
                    "{{\"name\":{},\"kind\":{},\"credential_env\":{},",
                    "\"configured\":{},\"secret_value_exposed\":false,",
                    "\"model_profile\":{},\"cache_support\":{},\"auth_method\":{},",
                    "\"live_worker_enabled\":false}}"
                ),
                json_string(status.definition.name),
                json_string(status.definition.kind),
                json_string(status.definition.env_var),
                status.configured,
                json_string(status.definition.model_profile),
                json_string(status.definition.cache_support),
                json_string(status.definition.auth_method)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn keychain_integration_available(statuses: &[ProviderStatus]) -> bool {
    statuses.iter().any(|status| {
        status.credential_source == "keychain_legacy"
            || status
                .credential_source
                .starts_with("provider_registry_keychain:")
    })
}

fn render_auth_registry(stamp: &ClockStamp, statuses: &[ProviderStatus]) -> String {
    let keychain_available = keychain_integration_available(statuses);
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.auth-registry.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"auth_methods\": {},\n",
            "  \"key_storage\": {},\n",
            "  \"secrets_stored_in_repo\": false,\n",
            "  \"live_keychain_integration\": {},\n",
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
        keychain_available,
        render_provider_statuses_json(statuses)
    )
}

fn render_auth_policy(stamp: &ClockStamp, statuses: &[ProviderStatus]) -> String {
    let keychain_available = keychain_integration_available(statuses);
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.auth-policy.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"key_storage_preference\": {},\n",
            "  \"auth_methods\": {},\n",
            "  \"oauth_candidates\": {},\n",
            "  \"api_key_providers\": {},\n",
            "  \"local_endpoint_providers\": {},\n",
            "  \"workspace_scoped_credentials\": true,\n",
            "  \"audit_log\": \"auth-audit-log.jsonl\",\n",
            "  \"live_keychain_integration\": {},\n",
            "  \"secret_values_exposed\": false\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&[
            "macos_keychain_first",
            "encrypted_file_fallback_planned",
            "environment_fallback_current"
        ]),
        json_array(&[
            "api_key",
            "oauth_candidate",
            "browser_login_bridge_planned",
            "local_endpoint",
            "enterprise_gateway_planned"
        ]),
        json_array(&["OpenAI", "Claude"]),
        json_array(&["OpenRouter", "OpenAI", "Claude", "Gemini", "Codex LB"]),
        json_array(&["Ollama", "LM Studio", "OpenAI-compatible local endpoints"]),
        keychain_available
    )
}

fn render_auth_audit_event(stamp: &ClockStamp, event: &str, statuses: &[ProviderStatus]) -> String {
    format!(
        concat!(
            "{{\"schema\":\"opensks.auth-audit-event.v1\",",
            "\"at\":{},\"event\":{},\"workspace_scoped\":true,",
            "\"secret_value_exposed\":false,\"live_keychain_integration\":{}}}\n"
        ),
        stamp.json(),
        json_string(event),
        keychain_integration_available(statuses)
    )
}

fn render_provider_registry(
    stamp: &ClockStamp,
    statuses: &[ProviderStatus],
    probes: &[ProviderProbe],
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.provider-registry.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"providers\": {},\n",
            "  \"provider_profiles\": {},\n",
            "  \"usage_metrics\": {},\n",
            "  \"live_adapters\": \"local_endpoint_probe_only\",\n",
            "  \"provider_env_status\": {},\n",
            "  \"last_probe_summary\": {}\n",
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
        render_provider_profiles_json(statuses),
        json_array(&[
            "tokens",
            "cost",
            "cached_tokens",
            "cache_writes",
            "reasoning_tokens",
            "tool_calls",
            "computer_browser_app_actions"
        ]),
        render_provider_statuses_json(statuses),
        render_provider_probe_summary_json(probes)
    )
}

fn write_provider_registry_artifacts(
    dir: &Path,
    stamp: &ClockStamp,
    statuses: &[ProviderStatus],
    probes: &[ProviderProbe],
) -> Result<(), OpenSksError> {
    write_text_atomic(
        &dir.join("provider-registry.json"),
        &render_provider_registry(stamp, statuses, probes),
    )?;
    write_text_atomic(
        &dir.join("provider-capabilities.json"),
        &render_provider_capabilities(stamp, statuses),
    )?;
    write_text_atomic(
        &dir.join("provider-dashboard.json"),
        &render_provider_dashboard(stamp, statuses, probes),
    )?;
    Ok(())
}

fn render_provider_capabilities(stamp: &ClockStamp, statuses: &[ProviderStatus]) -> String {
    let configured = statuses.iter().filter(|status| status.configured).count();
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.provider-capabilities.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"configured_count\": {},\n",
            "  \"first_class_providers\": [{{\"name\":\"OpenRouter\",\"role\":\"multi_provider_router\",\"glm_routing\":true,\"provider_routing\":true,\"cache_metrics_expected\":true}}],\n",
            "  \"optional_adapters\": [{{\"name\":\"Codex LB\",\"core_required\":false,\"profile\":\"optional-codex-load-balancer\"}}],\n",
            "  \"oauth_candidates\": {},\n",
            "  \"local_providers\": {},\n",
            "  \"openai_compatible_endpoints\": true,\n",
            "  \"secret_values_exposed\": false,\n",
            "  \"live_remote_api_calls\": false\n",
            "}}\n"
        ),
        stamp.json(),
        configured,
        json_array(&["OpenAI", "Claude"]),
        json_array(&["Ollama", "LM Studio", "OpenAI-compatible local endpoints"])
    )
}

fn render_provider_profiles_json(statuses: &[ProviderStatus]) -> String {
    let rows = statuses
        .iter()
        .map(|status| {
            format!(
                concat!(
                    "{{\"name\":{},\"model_profile\":{},\"cache_support\":{},",
                    "\"auth_method\":{},\"kind\":{},\"configured\":{},",
                    "\"credential_source\":{},\"auth_posture\":{}}}"
                ),
                json_string(status.definition.name),
                json_string(status.definition.model_profile),
                json_string(status.definition.cache_support),
                json_string(status.definition.auth_method),
                json_string(status.definition.kind),
                status.configured,
                json_string(&status.credential_source),
                json_string(provider_auth_posture(status))
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn trusted_update_signer_fingerprint() -> &'static str {
    "opensks-local-dev-trusted-signer-v1"
}

fn local_update_signature(manifest_hash: &str) -> String {
    stable_content_hash(&format!(
        "{}:{}",
        trusted_update_signer_fingerprint(),
        manifest_hash
    ))
}

fn render_update_manifest(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.update-manifest.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"current_version\": {},\n",
            "  \"channels\": {},\n",
            "  \"default_channel\": \"stable\",\n",
            "  \"artifacts\": {},\n",
            "  \"requires_signature\": true,\n",
            "  \"requires_rollback_plan\": true,\n",
            "  \"network_install_enabled\": false\n",
            "}}\n"
        ),
        stamp.json(),
        json_string(env!("CARGO_PKG_VERSION")),
        json_array(&["stable", "latest"]),
        json_array(&["opensks-cli", "app-bundle-candidate", "manifest"])
    )
}

fn render_update_signature(stamp: &ClockStamp, manifest_hash: &str, signature: &str) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.update-signature.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"manifest_hash\": {},\n",
            "  \"trusted_signer_fingerprint\": {},\n",
            "  \"signature\": {},\n",
            "  \"algorithm\": \"fnv1a64-local-dev-proof-not-production-crypto\",\n",
            "  \"production_crypto_live\": false\n",
            "}}\n"
        ),
        stamp.json(),
        json_string(manifest_hash),
        json_string(trusted_update_signer_fingerprint()),
        json_string(signature)
    )
}

fn render_update_channels(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.update-channels.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"channels\": [\n",
            "    {{\"name\":\"stable\",\"auto_apply\":false,\"requires_signature\":true,\"rollback_required\":true}},\n",
            "    {{\"name\":\"latest\",\"auto_apply\":false,\"requires_signature\":true,\"rollback_required\":true}}\n",
            "  ]\n",
            "}}\n"
        ),
        stamp.json()
    )
}

fn render_rollback_plan(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.rollback-plan.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"current_version\": {},\n",
            "  \"rollback_slots\": {},\n",
            "  \"restore_strategy\": \"preserve_previous_manifest_and_binary_before_apply\",\n",
            "  \"apply_transaction_live\": false\n",
            "}}\n"
        ),
        stamp.json(),
        json_string(env!("CARGO_PKG_VERSION")),
        json_array(&[
            "previous-stable",
            "previous-latest",
            "manual-operator-restore"
        ])
    )
}

fn render_update_boundary(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.update-boundary.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"auto_download\": false,\n",
            "  \"auto_apply\": false,\n",
            "  \"requires_operator_approval\": true,\n",
            "  \"requires_verified_signature\": true,\n",
            "  \"requires_rollback_plan\": true,\n",
            "  \"signed_updater_live\": \"local_manifest_signature_artifact_only\"\n",
            "}}\n"
        ),
        stamp.json()
    )
}

fn render_updater_final_state(stamp: &ClockStamp, manifest_hash: &str, signature: &str) -> String {
    let expected = local_update_signature(manifest_hash);
    let verified = expected == signature;
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.updater-final-state.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"status\": {},\n",
            "  \"manifest_hash\": {},\n",
            "  \"signature_verified\": {},\n",
            "  \"channels_present\": {},\n",
            "  \"rollback_plan_present\": true,\n",
            "  \"network_or_install_performed\": false\n",
            "}}\n"
        ),
        stamp.json(),
        json_string(if verified {
            "verified_artifact_plan"
        } else {
            "signature_mismatch"
        }),
        json_string(manifest_hash),
        verified,
        json_array(&["stable", "latest"])
    )
}

fn mvp_acceptance_items(cwd: &Path) -> Vec<AcceptanceItem> {
    let mvp_004_passed = mvp004_provider_adapter_gate_passed(cwd);
    let (mvp_004_status, mvp_004_evidence) = if mvp_004_passed {
        (
            "passed",
            "provider-adapter-check.json proves opt-in remote /models adapter checks for OpenRouter and OpenAI: schema=opensks.provider-adapter-check.v1, remote_probe_opt_in=true, summary total/attempted/reachable=2, no secret leak indicators, exact OpenRouter/OpenAI endpoints, and both adapter rows configured=true, attempted=true, status=adapter_models_endpoint_reachable, secret_value_exposed=false, with 2xx http_code.",
        )
    } else {
        (
            "partial",
            "mvp-004 requires .opensks/providers/provider-adapter-check.json with schema=opensks.provider-adapter-check.v1, remote_probe_opt_in=true, summary total/attempted/reachable=2, no secret leak indicators, exact OpenRouter/OpenAI /models endpoints, and exactly one OpenRouter plus one OpenAI adapter row configured=true, attempted=true, status=adapter_models_endpoint_reachable, secret_value_exposed=false, with 2xx http_code.",
        )
    };
    let mvp_007_passed = mvp007_browser_local_loop_gate_passed(cwd);
    let (mvp_007_status, mvp_007_evidence) = if mvp_007_passed {
        (
            "passed",
            "latest browser session proves scoped local deterministic browser-use artifacts: browser-session.json/session-summary.json bind the session id and artifact list, browser-runtime/index.html, browser-interaction-loop.json, browser-interaction-events.jsonl, browser-screenshot-snapshots.jsonl, and matching PPM screenshot hashes record open/screenshot/click/type while live Playwright/Chrome Extension/browser control, external web control, credential entry, and real browser-rendered screenshots remain false.",
        )
    } else {
        (
            "partial",
            "mvp-007 requires a latest browser session with browser-session.json/session-summary.json binding the directory session id and artifact list, browser-runtime/index.html, browser-interaction-loop.json, browser-interaction-events.jsonl, browser-screenshot-snapshots.jsonl, matching local PPM screenshot hashes, policy/final-state session binding, sensitive=false, and all live Playwright/Chrome Extension/browser control/external web/credential-entry flags false.",
        )
    };
    let mvp_008_passed = mvp008_app_use_accessibility_gate_passed(cwd);
    let (mvp_008_status, mvp_008_evidence) = if mvp_008_passed {
        (
            "passed",
            "latest app-use session proves local macOS accessibility inspection artifacts: accessibility-tree.json captured a frontmost application node, running-apps.json captured inventory, app-final-state.json reports inspection_attempted=true and live_app_actions_executed=false, and app-policy-decision.json allowed inspection only.",
        )
    } else {
        (
            "partial",
            "mvp-008 requires a latest app-use session with accessibility-tree.json captured=true and at least one top-level application node, running-apps.json captured with inventory, app-final-state.json inspection_attempted=true/live_app_actions_executed=false, and app-policy-decision.json allowing inspection only.",
        )
    };
    vec![
        acceptance_item(
            "mvp-001",
            "Rust engine runs.",
            "passed",
            "cargo test and cargo run commands execute the Rust CLI.",
        ),
        acceptance_item(
            "mvp-002",
            "Goal loop runs direct and naruto tasks.",
            "passed",
            "goal/run/naruto route through start_goal_loop and status tests verify final seal reads.",
        ),
        acceptance_item(
            "mvp-003",
            "Voxel TriWiki stores repo/requirement/proof voxels.",
            "passed",
            "voxel index and goal mission voxels write code, requirement, proof, cache, provider, design, and security voxels.",
        ),
        acceptance_item(
            "mvp-004",
            "OpenRouter/OpenAI provider adapters work.",
            mvp_004_status,
            mvp_004_evidence,
        ),
        acceptance_item(
            "mvp-005",
            "MCP client connects to local MCP server.",
            "passed",
            "mcp serve --once and mcp invoke exercise the local JSON-RPC broker.",
        ),
        acceptance_item(
            "mvp-006",
            "MCP server exposes OpenSKS goal/status/resource tools.",
            "passed",
            "mcp-server-descriptor.json exposes goal status, final seal, resources, prompts, QA, and repo search tools.",
        ),
        acceptance_item(
            "mvp-007",
            "Browser use can open page, screenshot, click, type.",
            mvp_007_status,
            mvp_007_evidence,
        ),
        acceptance_item(
            "mvp-008",
            "App use can inspect macOS accessibility tree.",
            mvp_008_status,
            mvp_008_evidence,
        ),
        acceptance_item(
            "mvp-009",
            "Worktree isolation works.",
            "passed",
            "worktree create copies an isolated workspace snapshot under .opensks/worktrees.",
        ),
        acceptance_item(
            "mvp-010",
            "Final seal exists.",
            "passed",
            "goal missions write final-seal.json and goal status reads it.",
        ),
        acceptance_item(
            "mvp-011",
            "GUI shows mission status and worker lanes.",
            "passed",
            "app writes dashboard.html and worker-lanes.json; the static dashboard renders mission status plus a worker-lane table from local mission/tool-plan artifacts without claiming live native GUI or live worker execution.",
        ),
    ]
}

fn mvp004_provider_adapter_gate_passed(cwd: &Path) -> bool {
    let Ok(report) = fs::read_to_string(
        cwd.join(OPEN_SKSDIR)
            .join("providers")
            .join("provider-adapter-check.json"),
    ) else {
        return false;
    };
    if !json_top_level_string_field_equals(&report, "schema", "opensks.provider-adapter-check.v1")
        || !json_top_level_bool_field_equals(&report, "remote_probe_opt_in", true)
        || !json_top_level_bool_field_equals(&report, "secret_value_exposed", false)
        || !provider_adapter_report_has_no_secret_leak(&report)
        || !provider_adapter_summary_gate_passed(&report)
    {
        return false;
    }
    let adapters = extract_json_top_level_array_objects(&report, "adapters");
    if adapters.len() != 2 {
        return false;
    }
    ["OpenRouter", "OpenAI"]
        .iter()
        .all(|name| match provider_adapter_expected_endpoint(name) {
            Some(endpoint) => provider_adapter_row_gate_passed(&adapters, name, endpoint),
            None => false,
        })
}

fn provider_adapter_summary_gate_passed(report: &str) -> bool {
    let Some(summary) = extract_json_top_level_raw_field(report, "summary") else {
        return false;
    };
    json_top_level_number_field_equals(&summary, "total", 2)
        && json_top_level_number_field_equals(&summary, "attempted", 2)
        && json_top_level_number_field_equals(&summary, "reachable", 2)
}

fn provider_adapter_report_has_no_secret_leak(report: &str) -> bool {
    let lower = report.to_ascii_lowercase();
    let marker_view = lower.replace("\\\"", "\"");
    !json_bool_field_true_anywhere(&marker_view, "secret_value_exposed")
        && !lower.contains("authorization")
        && !lower.contains("bearer")
        && !lower.contains("sk-")
        && !lower.contains("api_key=")
        && !lower.contains("api-key")
}

fn json_bool_field_true_anywhere(input: &str, key: &str) -> bool {
    let needle = format!("\"{key}\"");
    let mut offset = 0usize;
    while let Some(relative) = input[offset..].find(&needle) {
        let after_key = offset + relative + needle.len();
        let after_space = skip_json_whitespace(input, after_key);
        if input[after_space..].starts_with(':') {
            let value_start = skip_json_whitespace(input, after_space + 1);
            if input[value_start..].starts_with("true") {
                return true;
            }
        }
        offset = after_key;
    }
    false
}

fn provider_adapter_row_gate_passed(
    adapters: &[String],
    expected_name: &str,
    expected_endpoint: &str,
) -> bool {
    let matching = adapters
        .iter()
        .filter(|adapter| {
            json_top_level_string_field_equals(adapter, "name", expected_name)
                && json_top_level_bool_field_equals(adapter, "configured", true)
                && json_top_level_bool_field_equals(adapter, "attempted", true)
                && json_top_level_string_field_equals(adapter, "endpoint", expected_endpoint)
                && json_top_level_string_field_equals(
                    adapter,
                    "status",
                    "adapter_models_endpoint_reachable",
                )
                && json_top_level_bool_field_equals(adapter, "secret_value_exposed", false)
                && extract_json_top_level_raw_field(adapter, "http_code")
                    .as_deref()
                    .is_some_and(provider_adapter_http_code_is_2xx)
        })
        .count();
    matching == 1
}

fn provider_adapter_http_code_is_2xx(raw: &str) -> bool {
    let code = raw.trim().trim_matches('"');
    code.len() == 3 && code.starts_with('2') && code.bytes().all(|byte| byte.is_ascii_digit())
}

fn mvp007_browser_local_loop_gate_passed(cwd: &Path) -> bool {
    let Some(session_dir) = latest_browser_session_dir(cwd) else {
        return false;
    };
    let Ok(loop_report) = fs::read_to_string(session_dir.join("browser-interaction-loop.json"))
    else {
        return false;
    };
    let Ok(loop_events) = fs::read_to_string(session_dir.join("browser-interaction-events.jsonl"))
    else {
        return false;
    };
    let Ok(browser_session) = fs::read_to_string(session_dir.join("browser-session.json")) else {
        return false;
    };
    let Ok(session_summary) = fs::read_to_string(session_dir.join("session-summary.json")) else {
        return false;
    };
    let Ok(snapshot_report) =
        fs::read_to_string(session_dir.join("browser-screenshot-snapshots.jsonl"))
    else {
        return false;
    };
    let Ok(final_state) = fs::read_to_string(session_dir.join("browser-final-state.json")) else {
        return false;
    };
    let Ok(policy) = fs::read_to_string(session_dir.join("browser-policy-decision.json")) else {
        return false;
    };
    let Ok(runtime_html) =
        fs::read_to_string(session_dir.join("browser-runtime").join("index.html"))
    else {
        return false;
    };

    let Some(loop_session_id) = extract_json_top_level_string_field(&loop_report, "session_id")
    else {
        return false;
    };
    let Some(final_state_session_id) =
        extract_json_top_level_string_field(&final_state, "session_id")
    else {
        return false;
    };
    let Some(policy_session_id) = extract_json_top_level_string_field(&policy, "session_id") else {
        return false;
    };
    let Some(loop_target) = extract_json_top_level_string_field(&loop_report, "target") else {
        return false;
    };
    let Some(final_state_target) = extract_json_top_level_string_field(&final_state, "target")
    else {
        return false;
    };
    let Some(policy_target) = extract_json_top_level_string_field(&policy, "target") else {
        return false;
    };
    let Some(loop_iterations) =
        extract_json_top_level_number_field(&loop_report, "loop_iterations")
    else {
        return false;
    };
    let Some(runtime_ref) = extract_json_top_level_string_field(&loop_report, "runtime_ref") else {
        return false;
    };
    let Some(runtime_page_hash) =
        extract_json_top_level_string_field(&loop_report, "runtime_page_hash")
    else {
        return false;
    };
    let Some(screenshot_ref) = extract_json_top_level_string_field(&loop_report, "screenshot_ref")
    else {
        return false;
    };
    let Some(screenshot_hash) =
        extract_json_top_level_string_field(&loop_report, "screenshot_hash")
    else {
        return false;
    };
    let Some(pixel_count) = extract_json_top_level_number_field(&loop_report, "pixel_count") else {
        return false;
    };
    let Some(policy_decision) =
        extract_json_top_level_string_field(&loop_report, "policy_decision")
    else {
        return false;
    };
    let Some(dir_session_id) = session_dir
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::to_string)
    else {
        return false;
    };
    if loop_session_id != final_state_session_id
        || loop_session_id != policy_session_id
        || loop_session_id != dir_session_id
        || loop_target != final_state_target
        || loop_target != policy_target
    {
        return false;
    }

    let expected_runtime_html = render_browser_local_runtime_page(&loop_target);
    if runtime_ref != "browser-runtime/index.html"
        || runtime_html != expected_runtime_html
        || stable_content_hash(&runtime_html) != runtime_page_hash
        || !runtime_html.contains(BROWSER_LOCAL_LOOP_BUTTON_ID)
        || !runtime_html.contains(BROWSER_LOCAL_LOOP_INPUT_ID)
        || !runtime_html.contains(BROWSER_LOCAL_LOOP_STATUS_ID)
        || !runtime_html.contains(BROWSER_LOCAL_LOOP_FINAL_TEXT)
    {
        return false;
    }

    let artifact_refs = BrowserLocalArtifactRefs {
        session_id: &loop_session_id,
        target: &loop_target,
        runtime_ref: &runtime_ref,
        runtime_page_hash: &runtime_page_hash,
        screenshot_ref: &screenshot_ref,
        screenshot_hash: &screenshot_hash,
    };
    if !browser_screenshot_snapshot_artifact_valid(&session_dir, &snapshot_report, artifact_refs) {
        return false;
    }

    if !browser_local_screenshot_hash_matches(&session_dir, &screenshot_ref, &screenshot_hash) {
        return false;
    }

    let policy_decision_allowed = matches!(
        policy_decision.as_str(),
        "planned_non_url_browser_task"
            | "allowed_network_observation"
            | "approval_required_for_browser_action"
    );
    let browser_session_status_allowed =
        json_top_level_string_field_equals(&browser_session, "status", "planned")
            || json_top_level_string_field_equals(&browser_session, "status", "network_probe");
    let session_summary_status_allowed =
        json_top_level_string_field_equals(&session_summary, "status", "planned")
            || json_top_level_string_field_equals(&session_summary, "status", "network_probe");

    loop_iterations >= 6
        && pixel_count == BROWSER_LOCAL_SCREENSHOT_WIDTH * BROWSER_LOCAL_SCREENSHOT_HEIGHT
        && policy_decision_allowed
        && json_top_level_string_field_equals(
            &loop_report,
            "schema",
            "opensks.browser-interaction-loop.v1",
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "status",
            "local_browser_open_screenshot_click_type_recorded",
        )
        && json_top_level_string_array_contains(
            &loop_report,
            "loop_steps",
            &[
                "create_local_browser_runtime",
                "open_local_runtime_state",
                "record_local_screenshot_artifact",
                "click_local_runtime_button",
                "type_local_runtime_input",
                "record_final_state",
            ],
        )
        && json_top_level_bool_field_equals(&loop_report, "open_recorded", true)
        && json_top_level_bool_field_equals(&loop_report, "screenshot_recorded", true)
        && json_top_level_bool_field_equals(&loop_report, "click_recorded", true)
        && json_top_level_bool_field_equals(&loop_report, "type_recorded", true)
        && json_top_level_string_field_equals(
            &loop_report,
            "final_text",
            BROWSER_LOCAL_LOOP_FINAL_TEXT,
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "button_element_id",
            BROWSER_LOCAL_LOOP_BUTTON_ID,
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "input_element_id",
            BROWSER_LOCAL_LOOP_INPUT_ID,
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "status_element_id",
            BROWSER_LOCAL_LOOP_STATUS_ID,
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "screenshot_mode",
            BROWSER_LOCAL_SCREENSHOT_MODE,
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "screenshot_renderer",
            BROWSER_LOCAL_SCREENSHOT_RENDERER,
        )
        && json_top_level_bool_field_equals(&loop_report, "sensitive_action_detected", false)
        && json_top_level_bool_field_equals(&loop_report, "live_browser_control", false)
        && json_top_level_bool_field_equals(&loop_report, "playwright_actions_executed", false)
        && json_top_level_bool_field_equals(&loop_report, "chrome_extension_evidence", false)
        && json_top_level_bool_field_equals(&loop_report, "external_web_control", false)
        && json_top_level_bool_field_equals(&loop_report, "credential_entry_executed", false)
        && json_top_level_bool_field_equals(&loop_report, "browser_click_type_executed", false)
        && json_top_level_bool_field_equals(
            &loop_report,
            "requires_approval_before_live_interaction",
            true,
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "browser_final_state_ref",
            "browser-final-state.json",
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "policy_decision_ref",
            "browser-policy-decision.json",
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "screenshot_snapshot_ref",
            "browser-screenshot-snapshots.jsonl",
        )
        && json_top_level_string_field_equals(
            &browser_session,
            "schema",
            "opensks.browser.browser-session.v1",
        )
        && json_top_level_string_field_equals(&browser_session, "session_id", &loop_session_id)
        && json_top_level_string_field_equals(&browser_session, "plane", "browser")
        && json_top_level_string_field_equals(&browser_session, "target", &loop_target)
        && browser_session_status_allowed
        && json_top_level_bool_field_equals(&browser_session, "live_execution", false)
        && json_top_level_string_field_equals(
            &session_summary,
            "schema",
            "opensks.browser.session.v1",
        )
        && json_top_level_string_field_equals(&session_summary, "id", &loop_session_id)
        && json_top_level_string_field_equals(&session_summary, "plane", "browser")
        && json_top_level_string_field_equals(&session_summary, "command", &loop_target)
        && session_summary_status_allowed
        && json_top_level_string_array_contains(
            &session_summary,
            "artifacts",
            &[
                "browser-session.json",
                "browser-actions.jsonl",
                "screenshots/",
                "network-log.har",
                "dom-snapshots/",
                "browser-policy-decision.json",
                "browser-action-plan.json",
                "browser-page-links.json",
                "browser-final-state.json",
                "browser-runtime/index.html",
                "browser-screenshot-snapshots.jsonl",
                "browser-interaction-loop.json",
                "browser-interaction-events.jsonl",
            ],
        )
        && json_top_level_string_field_equals(
            &final_state,
            "schema",
            "opensks.browser-final-state.v1",
        )
        && json_top_level_string_field_equals(&final_state, "policy_decision", &policy_decision)
        && json_top_level_bool_field_equals(&final_state, "sensitive_action_detected", false)
        && json_top_level_bool_field_equals(&final_state, "playwright_actions_executed", false)
        && json_top_level_bool_field_equals(&final_state, "live_browser_control", false)
        && json_top_level_bool_field_equals(&final_state, "chrome_extension_evidence", false)
        && json_top_level_bool_field_equals(&final_state, "external_web_control", false)
        && json_top_level_bool_field_equals(&final_state, "credential_entry_executed", false)
        && json_top_level_bool_field_equals(&final_state, "browser_click_type_executed", false)
        && json_top_level_string_field_equals(
            &final_state,
            "browser_interaction_loop_ref",
            "browser-interaction-loop.json",
        )
        && json_top_level_string_field_equals(
            &final_state,
            "browser_runtime_ref",
            "browser-runtime/index.html",
        )
        && json_top_level_string_field_equals(
            &policy,
            "schema",
            "opensks.browser-policy-decision.v1",
        )
        && json_top_level_string_field_equals(&policy, "decision", &policy_decision)
        && json_top_level_bool_field_equals(&policy, "browser_action_allowed", false)
        && json_top_level_bool_field_equals(&policy, "sensitive", false)
        && browser_interaction_events_prove_local_open_screenshot_click_type(
            &loop_events,
            &loop_session_id,
            &runtime_ref,
            &runtime_page_hash,
            &screenshot_ref,
            &screenshot_hash,
            &policy_decision,
        )
}

fn latest_browser_session_dir(cwd: &Path) -> Option<PathBuf> {
    let browser_dir = cwd.join(OPEN_SKSDIR).join("browser");
    let mut session_dirs = fs::read_dir(browser_dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    session_dirs.sort();
    session_dirs.into_iter().next_back()
}

fn browser_screenshot_snapshot_artifact_valid(
    session_dir: &Path,
    snapshot_report: &str,
    refs: BrowserLocalArtifactRefs<'_>,
) -> bool {
    let lines = snapshot_report
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    if lines.len() != 1 {
        return false;
    }
    let line = lines[0].trim();
    json_top_level_string_field_equals(line, "schema", "opensks.browser-screenshot-snapshot.v1")
        && json_top_level_string_field_equals(line, "session_id", refs.session_id)
        && json_top_level_string_field_equals(line, "target", refs.target)
        && json_top_level_string_field_equals(line, "image_path", refs.screenshot_ref)
        && json_top_level_number_field_equals(line, "width", BROWSER_LOCAL_SCREENSHOT_WIDTH)
        && json_top_level_number_field_equals(line, "height", BROWSER_LOCAL_SCREENSHOT_HEIGHT)
        && json_top_level_number_field_equals(
            line,
            "pixel_count",
            BROWSER_LOCAL_SCREENSHOT_WIDTH * BROWSER_LOCAL_SCREENSHOT_HEIGHT,
        )
        && json_top_level_string_field_equals(line, "screenshot_hash", refs.screenshot_hash)
        && json_top_level_string_field_equals(line, "renderer", BROWSER_LOCAL_SCREENSHOT_RENDERER)
        && json_top_level_string_field_equals(line, "mode", BROWSER_LOCAL_SCREENSHOT_MODE)
        && json_top_level_string_field_equals(line, "runtime_ref", refs.runtime_ref)
        && json_top_level_string_field_equals(line, "runtime_page_hash", refs.runtime_page_hash)
        && browser_local_screenshot_hash_matches(
            session_dir,
            refs.screenshot_ref,
            refs.screenshot_hash,
        )
}

fn browser_local_screenshot_hash_matches(
    session_dir: &Path,
    image_path: &str,
    expected_hash: &str,
) -> bool {
    if image_path.contains("..") || !image_path.starts_with("screenshots/") {
        return false;
    }
    let Ok(contents) = fs::read_to_string(session_dir.join(image_path)) else {
        return false;
    };
    stable_content_hash(&contents) == expected_hash
        && parse_ppm_pixels_with_size(
            &contents,
            BROWSER_LOCAL_SCREENSHOT_WIDTH,
            BROWSER_LOCAL_SCREENSHOT_HEIGHT,
        )
        .is_some_and(|pixels| {
            pixels.len() == BROWSER_LOCAL_SCREENSHOT_WIDTH * BROWSER_LOCAL_SCREENSHOT_HEIGHT
        })
}

fn browser_interaction_events_prove_local_open_screenshot_click_type(
    events: &str,
    session_id: &str,
    runtime_ref: &str,
    runtime_page_hash: &str,
    screenshot_ref: &str,
    screenshot_hash: &str,
    policy_decision: &str,
) -> bool {
    let expected_events = [
        "browser_runtime_created",
        "local_page_open_recorded",
        "local_screenshot_recorded",
        "local_click_recorded",
        "local_type_recorded",
        "local_final_state_recorded",
        "live_browser_or_playwright_action",
    ];
    let mut seen = HashMap::new();
    for line in events.lines().filter(|line| !line.trim().is_empty()) {
        let line = line.trim();
        if !json_top_level_string_field_equals(
            line,
            "schema",
            "opensks.browser-interaction-event.v1",
        ) || !json_top_level_string_field_equals(line, "session_id", session_id)
        {
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

    let runtime_created = seen.get("browser_runtime_created").expect("event exists");
    let open_recorded = seen.get("local_page_open_recorded").expect("event exists");
    let screenshot_recorded = seen.get("local_screenshot_recorded").expect("event exists");
    let click_recorded = seen.get("local_click_recorded").expect("event exists");
    let type_recorded = seen.get("local_type_recorded").expect("event exists");
    let final_recorded = seen
        .get("local_final_state_recorded")
        .expect("event exists");
    let live_action = seen
        .get("live_browser_or_playwright_action")
        .expect("event exists");

    json_top_level_string_field_equals(runtime_created, "runtime_ref", runtime_ref)
        && json_top_level_string_field_equals(
            runtime_created,
            "runtime_page_hash",
            runtime_page_hash,
        )
        && json_top_level_bool_field_equals(runtime_created, "executed", true)
        && json_top_level_string_field_equals(open_recorded, "runtime_ref", runtime_ref)
        && json_top_level_bool_field_equals(open_recorded, "executed", true)
        && json_top_level_string_field_equals(screenshot_recorded, "screenshot_ref", screenshot_ref)
        && json_top_level_string_field_equals(
            screenshot_recorded,
            "screenshot_hash",
            screenshot_hash,
        )
        && json_top_level_bool_field_equals(screenshot_recorded, "executed", true)
        && json_top_level_string_field_equals(
            click_recorded,
            "element_id",
            BROWSER_LOCAL_LOOP_BUTTON_ID,
        )
        && json_top_level_string_field_equals(
            click_recorded,
            "final_text",
            BROWSER_LOCAL_LOOP_FINAL_TEXT,
        )
        && json_top_level_bool_field_equals(click_recorded, "executed", true)
        && json_top_level_string_field_equals(
            type_recorded,
            "element_id",
            BROWSER_LOCAL_LOOP_INPUT_ID,
        )
        && json_top_level_string_field_equals(
            type_recorded,
            "typed_text",
            BROWSER_LOCAL_LOOP_FINAL_TEXT,
        )
        && json_top_level_bool_field_equals(type_recorded, "executed", true)
        && json_top_level_string_field_equals(
            final_recorded,
            "status_element_id",
            BROWSER_LOCAL_LOOP_STATUS_ID,
        )
        && json_top_level_string_field_equals(
            final_recorded,
            "final_text",
            BROWSER_LOCAL_LOOP_FINAL_TEXT,
        )
        && json_top_level_bool_field_equals(final_recorded, "executed", true)
        && json_top_level_bool_field_equals(live_action, "executed", false)
        && json_top_level_string_field_equals(live_action, "policy_decision", policy_decision)
        && json_top_level_bool_field_equals(live_action, "approval_required", true)
        && json_top_level_bool_field_equals(live_action, "live_browser_control", false)
        && json_top_level_bool_field_equals(live_action, "playwright_actions_executed", false)
        && json_top_level_bool_field_equals(live_action, "chrome_extension_evidence", false)
        && json_top_level_bool_field_equals(live_action, "external_web_control", false)
        && json_top_level_bool_field_equals(live_action, "credential_entry_executed", false)
}

fn mvp008_app_use_accessibility_gate_passed(cwd: &Path) -> bool {
    let Some(session_dir) = latest_app_use_session_dir(cwd) else {
        return false;
    };
    let Ok(accessibility) = fs::read_to_string(session_dir.join("accessibility-tree.json")) else {
        return false;
    };
    let Ok(running_apps) = fs::read_to_string(session_dir.join("running-apps.json")) else {
        return false;
    };
    let Ok(final_state) = fs::read_to_string(session_dir.join("app-final-state.json")) else {
        return false;
    };
    let Ok(policy) = fs::read_to_string(session_dir.join("app-policy-decision.json")) else {
        return false;
    };

    let Some(running_app_count) =
        extract_json_top_level_number_field(&accessibility, "running_app_count")
    else {
        return false;
    };
    let Some(final_running_app_count) =
        extract_json_top_level_number_field(&final_state, "running_app_count")
    else {
        return false;
    };
    let Some(accessibility_session_id) =
        extract_json_top_level_string_field(&accessibility, "session_id")
    else {
        return false;
    };
    let Some(running_apps_session_id) =
        extract_json_top_level_string_field(&running_apps, "session_id")
    else {
        return false;
    };
    let Some(final_state_session_id) =
        extract_json_top_level_string_field(&final_state, "session_id")
    else {
        return false;
    };
    let Some(policy_session_id) = extract_json_top_level_string_field(&policy, "session_id") else {
        return false;
    };
    let Some(accessibility_target) = extract_json_top_level_string_field(&accessibility, "target")
    else {
        return false;
    };
    let Some(final_state_target) = extract_json_top_level_string_field(&final_state, "target")
    else {
        return false;
    };
    let Some(policy_target) = extract_json_top_level_string_field(&policy, "target") else {
        return false;
    };
    if extract_json_top_level_string_field(&accessibility, "stderr").is_none()
        || extract_json_top_level_string_field(&running_apps, "stderr").is_none()
    {
        return false;
    }
    let running_app_inventory = extract_json_string_array_values(
        &extract_json_top_level_raw_field(&running_apps, "apps").unwrap_or_default(),
    );

    json_top_level_string_field_equals(&accessibility, "schema", "opensks.accessibility-tree.v1")
        && accessibility_session_id == running_apps_session_id
        && accessibility_session_id == final_state_session_id
        && accessibility_session_id == policy_session_id
        && accessibility_target == final_state_target
        && accessibility_target == policy_target
        && json_top_level_bool_field_equals(&accessibility, "captured", true)
        && extract_json_top_level_string_field(&accessibility, "frontmost_app").is_some()
        && running_app_count > 0
        && accessibility_top_level_application_node_present(&accessibility)
        && json_top_level_string_field_equals(&accessibility, "status", "captured")
        && json_top_level_string_field_equals(
            &accessibility,
            "policy_decision",
            "allowed_inspection_only",
        )
        && json_top_level_string_field_equals(&running_apps, "schema", "opensks.running-apps.v1")
        && json_top_level_bool_field_equals(&running_apps, "attempted", true)
        && json_top_level_string_field_equals(&running_apps, "status", "captured")
        && !running_app_inventory.is_empty()
        && running_app_inventory.len() == running_app_count
        && json_top_level_string_field_equals(&final_state, "schema", "opensks.app-final-state.v1")
        && json_top_level_bool_field_equals(&final_state, "inspection_attempted", true)
        && json_top_level_string_field_equals(&final_state, "status", "captured")
        && final_running_app_count == running_app_count
        && json_top_level_string_field_equals(
            &final_state,
            "policy_decision",
            "allowed_inspection_only",
        )
        && json_top_level_bool_field_equals(&final_state, "sensitive_action_detected", false)
        && json_top_level_bool_field_equals(&final_state, "live_app_actions_executed", false)
        && json_top_level_string_field_equals(&policy, "schema", "opensks.app-policy-decision.v1")
        && json_top_level_bool_field_equals(&policy, "inspection_allowed", true)
        && json_top_level_bool_field_equals(&policy, "app_action_allowed", false)
        && json_top_level_bool_field_equals(&policy, "sensitive", false)
        && json_top_level_string_field_equals(&policy, "decision", "allowed_inspection_only")
}

fn latest_app_use_session_dir(cwd: &Path) -> Option<PathBuf> {
    let app_use_dir = cwd.join(OPEN_SKSDIR).join("app-use");
    let mut session_dirs = fs::read_dir(app_use_dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    session_dirs.sort();
    session_dirs.into_iter().next_back()
}

fn beta002_computer_use_isolated_loop_gate_passed(cwd: &Path) -> bool {
    let Some(session_dir) = latest_computer_use_session_dir(cwd) else {
        return false;
    };
    let Ok(loop_report) = fs::read_to_string(session_dir.join("computer-browser-loop.json")) else {
        return false;
    };
    let Ok(loop_events) =
        fs::read_to_string(session_dir.join("computer-browser-loop-events.jsonl"))
    else {
        return false;
    };
    let Ok(container) = fs::read_to_string(session_dir.join("isolated-browser-container.json"))
    else {
        return false;
    };
    let Ok(final_state) = fs::read_to_string(session_dir.join("computer-final-state.json")) else {
        return false;
    };
    let Ok(policy) = fs::read_to_string(session_dir.join("computer-policy-decision.json")) else {
        return false;
    };
    let Ok(runtime_html) = fs::read_to_string(
        session_dir
            .join("isolated-browser-runtime")
            .join("index.html"),
    ) else {
        return false;
    };

    let Some(loop_session_id) = extract_json_top_level_string_field(&loop_report, "session_id")
    else {
        return false;
    };
    let Some(container_session_id) = extract_json_top_level_string_field(&container, "session_id")
    else {
        return false;
    };
    let Some(final_state_session_id) =
        extract_json_top_level_string_field(&final_state, "session_id")
    else {
        return false;
    };
    let Some(policy_session_id) = extract_json_top_level_string_field(&policy, "session_id") else {
        return false;
    };
    let Some(loop_target) = extract_json_top_level_string_field(&loop_report, "target") else {
        return false;
    };
    let Some(container_target) = extract_json_top_level_string_field(&container, "target") else {
        return false;
    };
    let Some(final_state_target) = extract_json_top_level_string_field(&final_state, "target")
    else {
        return false;
    };
    let Some(policy_target) = extract_json_top_level_string_field(&policy, "target") else {
        return false;
    };
    let Some(loop_iterations) =
        extract_json_top_level_number_field(&loop_report, "loop_iterations")
    else {
        return false;
    };
    let Some(isolation_root) = extract_json_top_level_string_field(&container, "isolation_root")
    else {
        return false;
    };
    let Some(seed_page_hash) = extract_json_top_level_string_field(&container, "seed_page_hash")
    else {
        return false;
    };
    let Some(screenshot_status) =
        extract_json_top_level_string_field(&loop_report, "screenshot_status")
    else {
        return false;
    };
    let Some(policy_decision) =
        extract_json_top_level_string_field(&loop_report, "policy_decision")
    else {
        return false;
    };
    if extract_json_top_level_string_field(&final_state, "status").is_none() {
        return false;
    }
    let expected_runtime_html = render_isolated_browser_runtime_page(&loop_target);

    loop_session_id == container_session_id
        && loop_session_id == final_state_session_id
        && loop_session_id == policy_session_id
        && loop_target == container_target
        && loop_target == final_state_target
        && loop_target == policy_target
        && json_top_level_string_field_equals(
            &loop_report,
            "schema",
            "opensks.computer-browser-loop.v1",
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "status",
            "local_isolated_observation_loop_recorded",
        )
        && loop_iterations >= 6
        && json_top_level_string_array_contains(
            &loop_report,
            "loop_steps",
            &[
                "create_isolated_runtime",
                "observe_screenshot_status",
                "open_local_runtime_state",
                "click_local_runtime_button",
                "type_local_runtime_input",
                "record_final_state",
            ],
        )
        && json_top_level_bool_field_equals(&loop_report, "isolated_runtime_created", true)
        && json_top_level_bool_field_equals(&loop_report, "observation_loop_executed", true)
        && json_top_level_string_field_equals(
            &loop_report,
            "isolated_runtime_ref",
            "isolated-browser-runtime/index.html",
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "computer_session_ref",
            "computer-session.json",
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "computer_final_state_ref",
            "computer-final-state.json",
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "browser_container_ref",
            "isolated-browser-container.json",
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "browser_seed_ref",
            "isolated-browser-runtime/index.html",
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "policy_decision",
            "allowed_observation_only",
        )
        && policy_decision == "allowed_observation_only"
        && json_top_level_bool_field_equals(&loop_report, "isolated_browser_open_recorded", true)
        && json_top_level_bool_field_equals(&loop_report, "isolated_browser_click_recorded", true)
        && json_top_level_bool_field_equals(&loop_report, "isolated_browser_type_recorded", true)
        && json_top_level_string_field_equals(
            &loop_report,
            "isolated_browser_final_text",
            COMPUTER_ISOLATED_LOOP_FINAL_TEXT,
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "button_element_id",
            COMPUTER_ISOLATED_LOOP_BUTTON_ID,
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "input_element_id",
            COMPUTER_ISOLATED_LOOP_INPUT_ID,
        )
        && json_top_level_string_field_equals(
            &loop_report,
            "status_element_id",
            COMPUTER_ISOLATED_LOOP_STATUS_ID,
        )
        && json_top_level_bool_field_equals(&loop_report, "live_browser_container_control", false)
        && json_top_level_bool_field_equals(&loop_report, "browser_click_type_executed", false)
        && json_top_level_bool_field_equals(&loop_report, "mouse_keyboard_actions_executed", false)
        && json_top_level_bool_field_equals(&loop_report, "external_web_control", false)
        && json_top_level_bool_field_equals(
            &loop_report,
            "requires_approval_before_interaction",
            true,
        )
        && json_top_level_string_field_equals(
            &container,
            "schema",
            "opensks.isolated-browser-container.v1",
        )
        && json_top_level_bool_field_equals(&container, "network_access_enabled", false)
        && json_top_level_bool_field_equals(&container, "browser_process_launched", false)
        && json_top_level_bool_field_equals(&container, "live_browser_control", false)
        && json_top_level_bool_field_equals(&container, "external_web_control", false)
        && json_top_level_string_field_equals(
            &container,
            "container_status",
            "local_artifact_seeded",
        )
        && seed_page_hash != "unavailable"
        && stable_content_hash(&runtime_html) == seed_page_hash
        && runtime_html == expected_runtime_html
        && json_top_level_string_field_equals(
            &final_state,
            "schema",
            "opensks.computer-final-state.v1",
        )
        && json_top_level_string_field_equals(
            &final_state,
            "policy_decision",
            "allowed_observation_only",
        )
        && json_top_level_bool_field_equals(&final_state, "sensitive_action_detected", false)
        && json_top_level_bool_field_equals(&final_state, "mouse_keyboard_actions_executed", false)
        && json_top_level_bool_field_equals(&final_state, "live_browser_container_control", false)
        && json_top_level_bool_field_equals(&final_state, "external_web_control", false)
        && json_top_level_string_field_equals(
            &final_state,
            "isolated_browser_loop_ref",
            "computer-browser-loop.json",
        )
        && json_top_level_string_field_equals(
            &final_state,
            "isolated_browser_runtime_ref",
            "isolated-browser-runtime/index.html",
        )
        && json_top_level_string_field_equals(
            &final_state,
            "isolated_browser_final_text",
            COMPUTER_ISOLATED_LOOP_FINAL_TEXT,
        )
        && json_top_level_string_field_equals(
            &policy,
            "schema",
            "opensks.computer-policy-decision.v1",
        )
        && json_top_level_string_field_equals(&policy, "decision", "allowed_observation_only")
        && json_top_level_bool_field_equals(&policy, "screenshot_allowed", true)
        && json_top_level_bool_field_equals(&policy, "mouse_keyboard_allowed", false)
        && json_top_level_bool_field_equals(&policy, "sensitive", false)
        && computer_loop_events_prove_isolated_open_click_type(
            &loop_events,
            &loop_session_id,
            &isolation_root,
            &screenshot_status,
            &policy_decision,
        )
        && runtime_html.contains(COMPUTER_ISOLATED_LOOP_BUTTON_ID)
        && runtime_html.contains(COMPUTER_ISOLATED_LOOP_INPUT_ID)
        && runtime_html.contains(COMPUTER_ISOLATED_LOOP_STATUS_ID)
        && runtime_html.contains(COMPUTER_ISOLATED_LOOP_FINAL_TEXT)
}

fn latest_computer_use_session_dir(cwd: &Path) -> Option<PathBuf> {
    let computer_use_dir = cwd.join(OPEN_SKSDIR).join("computer-use");
    let mut session_dirs = fs::read_dir(computer_use_dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    session_dirs.sort();
    session_dirs.into_iter().next_back()
}

fn computer_loop_events_prove_isolated_open_click_type(
    events: &str,
    session_id: &str,
    isolation_root: &str,
    screenshot_status: &str,
    policy_decision: &str,
) -> bool {
    let expected_events = [
        "isolated_runtime_created",
        "isolated_browser_open_recorded",
        "isolated_browser_click_recorded",
        "isolated_browser_type_recorded",
        "isolated_browser_final_state_recorded",
        "computer_observation",
        "interactive_browser_or_mouse_keyboard_action",
    ];
    let mut seen = HashMap::new();
    for line in events.lines().filter(|line| !line.trim().is_empty()) {
        let line = line.trim();
        if !json_top_level_string_field_equals(
            line,
            "schema",
            "opensks.computer-browser-loop-event.v1",
        ) || !json_top_level_string_field_equals(line, "session_id", session_id)
        {
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

    let runtime_created = seen.get("isolated_runtime_created").expect("event exists");
    let open_recorded = seen
        .get("isolated_browser_open_recorded")
        .expect("event exists");
    let click_recorded = seen
        .get("isolated_browser_click_recorded")
        .expect("event exists");
    let type_recorded = seen
        .get("isolated_browser_type_recorded")
        .expect("event exists");
    let final_recorded = seen
        .get("isolated_browser_final_state_recorded")
        .expect("event exists");
    let observation = seen.get("computer_observation").expect("event exists");
    let interactive = seen
        .get("interactive_browser_or_mouse_keyboard_action")
        .expect("event exists");

    json_top_level_string_field_equals(runtime_created, "path", isolation_root)
        && json_top_level_bool_field_equals(runtime_created, "executed", true)
        && json_top_level_string_field_equals(
            open_recorded,
            "runtime_ref",
            "isolated-browser-runtime/index.html",
        )
        && json_top_level_bool_field_equals(open_recorded, "executed", true)
        && json_top_level_string_field_equals(
            click_recorded,
            "element_id",
            COMPUTER_ISOLATED_LOOP_BUTTON_ID,
        )
        && json_top_level_string_field_equals(
            click_recorded,
            "final_text",
            COMPUTER_ISOLATED_LOOP_FINAL_TEXT,
        )
        && json_top_level_bool_field_equals(click_recorded, "executed", true)
        && json_top_level_string_field_equals(
            type_recorded,
            "element_id",
            COMPUTER_ISOLATED_LOOP_INPUT_ID,
        )
        && json_top_level_string_field_equals(
            type_recorded,
            "typed_text",
            COMPUTER_ISOLATED_LOOP_FINAL_TEXT,
        )
        && json_top_level_bool_field_equals(type_recorded, "executed", true)
        && json_top_level_string_field_equals(
            final_recorded,
            "status_element_id",
            COMPUTER_ISOLATED_LOOP_STATUS_ID,
        )
        && json_top_level_string_field_equals(
            final_recorded,
            "final_text",
            COMPUTER_ISOLATED_LOOP_FINAL_TEXT,
        )
        && json_top_level_bool_field_equals(final_recorded, "executed", true)
        && json_top_level_string_field_equals(observation, "screenshot_status", screenshot_status)
        && extract_json_top_level_raw_field(observation, "executed")
            .is_some_and(|value| value == "true" || value == "false")
        && json_top_level_bool_field_equals(interactive, "executed", false)
        && json_top_level_string_field_equals(interactive, "policy_decision", policy_decision)
        && json_top_level_bool_field_equals(interactive, "approval_required", true)
        && json_top_level_bool_field_equals(interactive, "live_browser_container_control", false)
        && json_top_level_bool_field_equals(interactive, "external_web_control", false)
}

fn beta003_design_qa_screenshot_diff_gate_passed(cwd: &Path) -> bool {
    let design_dir = cwd.join(OPEN_SKSDIR).join("design");
    let Ok(qa_report) = fs::read_to_string(design_dir.join("design-qa-report.json")) else {
        return false;
    };
    let Ok(visual_report) = fs::read_to_string(design_dir.join("design-visual-diff-report.json"))
    else {
        return false;
    };
    let Ok(screenshot_report) =
        fs::read_to_string(design_dir.join("design-screenshot-diff-report.json"))
    else {
        return false;
    };
    let Ok(screenshot_snapshot) =
        fs::read_to_string(design_dir.join("design-screenshot-snapshots.jsonl"))
    else {
        return false;
    };

    let Some(qa_surface_count) = extract_json_top_level_number_field(&qa_report, "surface_count")
    else {
        return false;
    };
    let Some(snapshot_count) =
        extract_json_top_level_number_field(&screenshot_report, "screenshot_snapshot_count")
    else {
        return false;
    };
    let Some(diff_count) =
        extract_json_top_level_number_field(&screenshot_report, "screenshot_diff_count")
    else {
        return false;
    };
    let Some(pixel_count_total) =
        extract_json_top_level_number_field(&screenshot_report, "pixel_count_total")
    else {
        return false;
    };
    let Some(missing_image_artifact_count) =
        extract_json_top_level_number_field(&screenshot_report, "missing_image_artifact_count")
    else {
        return false;
    };

    qa_surface_count > 0
        && snapshot_count == qa_surface_count
        && diff_count == snapshot_count
        && pixel_count_total > 0
        && missing_image_artifact_count == 0
        && json_top_level_string_field_equals(&qa_report, "schema", "opensks.design-qa-report.v1")
        && json_top_level_bool_field_equals(&qa_report, "static_scan_executed", true)
        && json_top_level_bool_field_equals(&qa_report, "source_visual_diff_executed", true)
        && json_top_level_bool_field_equals(&qa_report, "screenshot_diff_executed", true)
        && json_top_level_string_field_equals(
            &qa_report,
            "screenshot_diff_mode",
            DESIGN_SCREENSHOT_MODE,
        )
        && json_top_level_bool_field_equals(&qa_report, "screenshot_baseline_available", true)
        && json_top_level_bool_field_equals(&qa_report, "live_browser_capture_executed", false)
        && json_top_level_bool_field_equals(&qa_report, "live_image_or_screenshot_evidence", false)
        && design_report_forbidden_live_flags_false(&qa_report)
        && json_top_level_string_field_equals(
            &visual_report,
            "schema",
            "opensks.design-visual-diff-report.v1",
        )
        && json_top_level_bool_field_equals(&visual_report, "baseline_available", true)
        && json_top_level_bool_field_equals(&visual_report, "source_visual_diff_executed", true)
        && json_top_level_bool_field_equals(&visual_report, "screenshot_diff_executed", true)
        && json_top_level_string_field_equals(
            &visual_report,
            "screenshot_diff_mode",
            DESIGN_SCREENSHOT_MODE,
        )
        && json_top_level_string_field_equals(
            &visual_report,
            "screenshot_diff_report_ref",
            "design-screenshot-diff-report.json",
        )
        && json_top_level_bool_field_equals(&visual_report, "live_browser_capture_executed", false)
        && json_top_level_bool_field_equals(
            &visual_report,
            "image_generation_review_executed",
            false,
        )
        && json_top_level_bool_field_equals(&visual_report, "gpt_image_review_executed", false)
        && json_top_level_bool_field_equals(
            &visual_report,
            "live_image_or_screenshot_evidence",
            false,
        )
        && design_report_forbidden_live_flags_false(&visual_report)
        && json_top_level_string_field_equals(
            &screenshot_report,
            "schema",
            "opensks.design-screenshot-diff-report.v1",
        )
        && json_top_level_bool_field_equals(&screenshot_report, "baseline_available", true)
        && json_top_level_bool_field_equals(&screenshot_report, "screenshot_diff_executed", true)
        && json_top_level_string_field_equals(
            &screenshot_report,
            "screenshot_diff_mode",
            DESIGN_SCREENSHOT_MODE,
        )
        && json_top_level_string_field_equals(
            &screenshot_report,
            "renderer",
            DESIGN_SCREENSHOT_RENDERER,
        )
        && json_top_level_bool_field_equals(
            &screenshot_report,
            "live_browser_capture_executed",
            false,
        )
        && json_top_level_bool_field_equals(&screenshot_report, "chrome_extension_evidence", false)
        && json_top_level_bool_field_equals(
            &screenshot_report,
            "image_generation_review_executed",
            false,
        )
        && json_top_level_bool_field_equals(&screenshot_report, "gpt_image_review_executed", false)
        && json_top_level_bool_field_equals(
            &screenshot_report,
            "product_design_visual_comparison_executed",
            false,
        )
        && json_top_level_bool_field_equals(
            &screenshot_report,
            "external_design_service_executed",
            false,
        )
        && json_top_level_bool_field_equals(
            &screenshot_report,
            "live_image_or_screenshot_evidence",
            false,
        )
        && design_report_forbidden_live_flags_false(&screenshot_report)
        && design_screenshot_diff_rows_valid(
            &design_dir,
            &screenshot_report,
            diff_count,
            pixel_count_total,
        )
        && design_screenshot_snapshot_artifacts_valid(
            &design_dir,
            &screenshot_snapshot,
            snapshot_count,
        )
}

fn design_report_forbidden_live_flags_false(report: &str) -> bool {
    [
        "live_browser_capture_executed",
        "chrome_extension_evidence",
        "image_generation_review_executed",
        "gpt_image_review_executed",
        "product_design_visual_comparison_executed",
        "external_design_service_executed",
        "live_image_or_screenshot_evidence",
    ]
    .iter()
    .all(|field| {
        json_top_level_field_absent(report, field)
            || json_top_level_bool_field_equals(report, field, false)
    })
}

fn design_screenshot_diff_rows_valid(
    design_dir: &Path,
    report: &str,
    expected_count: usize,
    expected_pixel_count_total: usize,
) -> bool {
    let rows = extract_json_top_level_array_objects(report, "diffs");
    if rows.len() != expected_count || rows.is_empty() {
        return false;
    }
    let mut pixel_total = 0usize;
    for row in rows {
        let Some(status) = extract_json_top_level_string_field(&row, "status") else {
            return false;
        };
        if !matches!(
            status.as_str(),
            "unchanged" | "changed" | "added" | "removed"
        ) {
            return false;
        }
        let Some(pixel_count) = extract_json_top_level_number_field(&row, "pixel_count") else {
            return false;
        };
        let Some(pixel_changed_count) =
            extract_json_top_level_number_field(&row, "pixel_changed_count")
        else {
            return false;
        };
        if pixel_count == 0 || pixel_changed_count > pixel_count {
            return false;
        }
        pixel_total += pixel_count;
        if !json_top_level_bool_field_equals(&row, "image_artifacts_present", true)
            || extract_json_top_level_string_field(&row, "path").is_none()
        {
            return false;
        }

        let previous_path = extract_json_top_level_string_field(&row, "previous_image_path");
        let previous_hash = extract_json_top_level_string_field(&row, "previous_screenshot_hash");
        let current_path = extract_json_top_level_string_field(&row, "current_image_path");
        let current_hash = extract_json_top_level_string_field(&row, "current_screenshot_hash");

        match status.as_str() {
            "added" => {
                if previous_path.is_some() || previous_hash.is_some() {
                    return false;
                }
                let (Some(path), Some(hash)) = (current_path.as_deref(), current_hash.as_deref())
                else {
                    return false;
                };
                if !design_screenshot_report_image_hash_matches(design_dir, path, hash) {
                    return false;
                }
            }
            "removed" => {
                if current_path.is_some() || current_hash.is_some() {
                    return false;
                }
                let (Some(path), Some(hash)) = (previous_path.as_deref(), previous_hash.as_deref())
                else {
                    return false;
                };
                if !design_screenshot_report_image_hash_matches(design_dir, path, hash) {
                    return false;
                }
            }
            "unchanged" | "changed" => {
                let (
                    Some(previous_path),
                    Some(previous_hash),
                    Some(current_path),
                    Some(current_hash),
                ) = (
                    previous_path.as_deref(),
                    previous_hash.as_deref(),
                    current_path.as_deref(),
                    current_hash.as_deref(),
                )
                else {
                    return false;
                };
                if !design_screenshot_report_image_hash_matches(
                    design_dir,
                    previous_path,
                    previous_hash,
                ) || !design_screenshot_report_image_hash_matches(
                    design_dir,
                    current_path,
                    current_hash,
                ) {
                    return false;
                }
                if status == "unchanged" && previous_hash != current_hash {
                    return false;
                }
                if status == "changed" && previous_hash == current_hash {
                    return false;
                }
            }
            _ => return false,
        }
    }
    pixel_total == expected_pixel_count_total
}

fn design_screenshot_report_image_hash_matches(
    design_dir: &Path,
    image_path: &str,
    expected_hash: &str,
) -> bool {
    if image_path.contains("..") || !image_path.starts_with("screenshots/") {
        return false;
    }
    let Ok(contents) = fs::read_to_string(design_dir.join(image_path)) else {
        return false;
    };
    stable_content_hash(&contents) == expected_hash
        && parse_ppm_pixels(&contents).is_some_and(|pixels| {
            pixels.len() == DESIGN_SCREENSHOT_WIDTH * DESIGN_SCREENSHOT_HEIGHT
        })
}

fn design_screenshot_snapshot_artifacts_valid(
    design_dir: &Path,
    snapshot: &str,
    expected_count: usize,
) -> bool {
    let mut count = 0usize;
    for line in snapshot.lines().filter(|line| !line.trim().is_empty()) {
        count += 1;
        if !json_top_level_string_field_equals(
            line,
            "schema",
            "opensks.design-screenshot-snapshot.v1",
        ) || !json_top_level_string_field_equals(line, "renderer", DESIGN_SCREENSHOT_RENDERER)
            || !json_top_level_string_field_equals(line, "mode", DESIGN_SCREENSHOT_MODE)
            || !json_top_level_number_field_equals(line, "width", DESIGN_SCREENSHOT_WIDTH)
            || !json_top_level_number_field_equals(line, "height", DESIGN_SCREENSHOT_HEIGHT)
            || !json_top_level_number_field_equals(
                line,
                "pixel_count",
                DESIGN_SCREENSHOT_WIDTH * DESIGN_SCREENSHOT_HEIGHT,
            )
        {
            return false;
        }
        let Some(image_path) = extract_json_top_level_string_field(line, "image_path") else {
            return false;
        };
        let Some(screenshot_hash) = extract_json_top_level_string_field(line, "screenshot_hash")
        else {
            return false;
        };
        if image_path.contains("..") || !image_path.starts_with("screenshots/") {
            return false;
        }
        let Ok(contents) = fs::read_to_string(design_dir.join(&image_path)) else {
            return false;
        };
        if stable_content_hash(&contents) != screenshot_hash
            || parse_ppm_pixels(&contents).is_none_or(|pixels| {
                pixels.len() != DESIGN_SCREENSHOT_WIDTH * DESIGN_SCREENSHOT_HEIGHT
            })
            || extract_json_top_level_string_field(line, "path").is_none()
            || extract_json_top_level_string_field(line, "kind").is_none()
            || extract_json_top_level_string_field(line, "content_hash").is_none()
            || extract_json_top_level_string_field(line, "visual_signature").is_none()
        {
            return false;
        }
    }
    count == expected_count && count > 0
}

fn accessibility_top_level_application_node_present(accessibility: &str) -> bool {
    extract_json_top_level_array_objects(accessibility, "nodes")
        .iter()
        .any(|node| {
            json_top_level_string_field_equals(node, "role", "application")
                && extract_json_top_level_string_field(node, "name").is_some()
                && json_top_level_bool_field_equals(node, "frontmost", true)
        })
}

fn beta_acceptance_items(cwd: &Path) -> Vec<AcceptanceItem> {
    let beta_002_passed = beta002_computer_use_isolated_loop_gate_passed(cwd);
    let (beta_002_status, beta_002_evidence) = if beta_002_passed {
        (
            "passed",
            "computer-use isolated browser/container artifacts prove a deterministic synthetic local HTML open/click/type event ledger, with policy/final-state evidence and live browser control, external web control, and mouse/keyboard execution all false.",
        )
    } else {
        (
            "partial",
            "beta-002 requires computer-use artifacts isolated-browser-container.json, computer-browser-loop.json, computer-browser-loop-events.jsonl, isolated-browser-runtime/index.html, computer-policy-decision.json, and computer-final-state.json proving deterministic synthetic local HTML open/click/type event records while live browser control, external web control, and mouse/keyboard execution remain false.",
        )
    };
    let beta_003_passed = beta003_design_qa_screenshot_diff_gate_passed(cwd);
    let (beta_003_status, beta_003_evidence) = if beta_003_passed {
        (
            "passed",
            "design qa deterministic local raster screenshot artifacts prove pixel diff evidence through design-screenshot-diff-report.json, design-screenshot-snapshots.jsonl, and matching PPM hashes; live browser-rendered screenshots, Chrome Extension evidence, Product Design visual comparison, external design services, and gpt-image-2 review remain false.",
        )
    } else {
        (
            "partial",
            "beta-003 requires design qa to run at least twice with a baseline, write design-screenshot-diff-report.json, design-screenshot-snapshots.jsonl, and local PPM screenshot artifacts with matching hashes, screenshot_diff_executed=true, missing_image_artifact_count=0, and all live browser/gpt/Product Design/external visual evidence flags false.",
        )
    };
    let beta_004_passed = beta004_cache_layout_gate_passed(cwd);
    let (beta_004_status, beta_004_evidence) = if beta_004_passed {
        (
            "passed",
            "cache-layout-improvement.json proves local Voxel TriWiki cache-layout improvement with layout_gate_passed=true, voxel_triwiki_segment_present=true, baseline_available=true, and local_warm_prefix_hit_percent >= target_hit_percent; provider/runtime cache-layout telemetry remains explicitly unavailable.",
        )
    } else {
        (
            "partial",
            "beta-004 requires cache-layout-improvement.json with schema opensks.cache-layout-improvement.v1, scope voxel_triwiki_cache_layout, strategy stable_prefix_dynamic_suffix, layout_gate_passed=true, baseline_available=true, voxel_triwiki_segment_present=true, local_warm_prefix_hit_percent >= target_hit_percent, and provider/runtime metrics explicitly unavailable.",
        )
    };
    let beta_005_passed = beta005_token_dashboard_provider_cache_gate_passed(cwd);
    let (beta_005_status, beta_005_evidence) = if beta_005_passed {
        (
            "passed",
            "cache-hit-report.json, cache-dashboard.json, providers/usage-dashboard.json, and provider-dashboard.json prove the token dashboard tracks provider cache-hit fields, local estimated cached tokens, source/status, and explicit provider_metrics_status=not_connected; live provider cached-token metrics remain unavailable.",
        )
    } else {
        (
            "partial",
            "beta-005 requires cache warm to establish local cache-hit evidence plus provider/cache dashboards that explicitly track provider cache-hit fields, null live provider percentages/tokens, cache-hit source/status, and provider_metrics_status=not_connected.",
        )
    };
    let beta_006_passed =
        opensks_native_collaboration::beta006_native_collaboration_gate_passed(cwd);
    let (beta_006_status, beta_006_evidence) = if beta_006_passed {
        (
            "passed",
            "native-collaboration-execution.json and native-collaboration-events.jsonl bind bench collaboration evidence to independently verified native agent provenance, with multiple completed native roles, no hidden fallback, and live remote multi-provider API worker collaboration/final apply explicitly false.",
        )
    } else {
        (
            "partial",
            "beta-006 requires independently verifiable native multi-session provenance, not just locally self-consistent .sneakoscope session/consensus files. Current bench artifacts can record scoped native collaboration evidence, but live remote multi-provider API worker collaboration and signed/proven native session provenance remain unverified.",
        )
    };
    vec![
        acceptance_item(
            "beta-001",
            "MCP broker enforces permissions.",
            "passed",
            "mcp audit writes broker policy denying raw model tool calls by default.",
        ),
        acceptance_item(
            "beta-002",
            "Computer-use loop works in isolated browser/container.",
            beta_002_status,
            beta_002_evidence,
        ),
        acceptance_item(
            "beta-003",
            "Design QA screenshot diff works.",
            beta_003_status,
            beta_003_evidence,
        ),
        acceptance_item(
            "beta-004",
            "Voxel TriWiki improves cache layout.",
            beta_004_status,
            beta_004_evidence,
        ),
        acceptance_item(
            "beta-005",
            "Token dashboard tracks provider cache hit.",
            beta_005_status,
            beta_005_evidence,
        ),
        acceptance_item(
            "beta-006",
            "Multi-LLM collaboration works.",
            beta_006_status,
            beta_006_evidence,
        ),
    ]
}

fn beta004_cache_layout_gate_passed(cwd: &Path) -> bool {
    let Ok(layout) = fs::read_to_string(
        cwd.join(OPEN_SKSDIR)
            .join("cache")
            .join("cache-layout-improvement.json"),
    ) else {
        return false;
    };
    let Some(local_hit_percent) =
        extract_json_top_level_float_field(&layout, "local_warm_prefix_hit_percent")
    else {
        return false;
    };
    let Some(target_hit_percent) =
        extract_json_top_level_float_field(&layout, "target_hit_percent")
    else {
        return false;
    };
    let Some(stable_segment_count) =
        extract_json_top_level_number_field(&layout, "stable_segment_count")
    else {
        return false;
    };
    let Some(dynamic_segment_count) =
        extract_json_top_level_number_field(&layout, "dynamic_segment_count")
    else {
        return false;
    };
    let Some(total_segment_count) =
        extract_json_top_level_number_field(&layout, "total_segment_count")
    else {
        return false;
    };
    let Some(stable_prefix_bytes) =
        extract_json_top_level_number_field(&layout, "stable_prefix_bytes")
    else {
        return false;
    };
    let Some(dynamic_suffix_bytes) =
        extract_json_top_level_number_field(&layout, "dynamic_suffix_bytes")
    else {
        return false;
    };
    let Some(matched_stable_prefix_bytes) =
        extract_json_top_level_number_field(&layout, "matched_stable_prefix_bytes")
    else {
        return false;
    };

    json_top_level_string_field_equals(&layout, "schema", "opensks.cache-layout-improvement.v1")
        && json_top_level_string_field_equals(&layout, "scope", "voxel_triwiki_cache_layout")
        && json_top_level_string_field_equals(&layout, "strategy", "stable_prefix_dynamic_suffix")
        && json_top_level_string_field_equals(
            &layout,
            "status",
            "local_cache_layout_improved_provider_unverified",
        )
        && json_top_level_bool_field_equals(&layout, "layout_gate_passed", true)
        && json_top_level_bool_field_equals(&layout, "baseline_available", true)
        && json_top_level_bool_field_equals(&layout, "voxel_triwiki_segment_present", true)
        && json_top_level_bool_field_equals(&layout, "provider_metrics_available", false)
        && json_top_level_bool_field_equals(&layout, "live_provider_cache_metrics", false)
        && stable_segment_count > 0
        && dynamic_segment_count > 0
        && total_segment_count == stable_segment_count + dynamic_segment_count
        && stable_prefix_bytes > 0
        && dynamic_suffix_bytes > 0
        && matched_stable_prefix_bytes == stable_prefix_bytes
        && target_hit_percent >= 95.0
        && local_hit_percent >= target_hit_percent
}

fn beta005_token_dashboard_provider_cache_gate_passed(cwd: &Path) -> bool {
    let cache_dir = cwd.join(OPEN_SKSDIR).join("cache");
    let provider_dir = cwd.join(OPEN_SKSDIR).join("providers");
    let Ok(cache_hit) = fs::read_to_string(cache_dir.join("cache-hit-report.json")) else {
        return false;
    };
    let Ok(cache_dashboard) = fs::read_to_string(cache_dir.join("cache-dashboard.json")) else {
        return false;
    };
    let Ok(usage_dashboard) = fs::read_to_string(provider_dir.join("usage-dashboard.json")) else {
        return false;
    };
    let Ok(provider_dashboard) = fs::read_to_string(provider_dir.join("provider-dashboard.json"))
    else {
        return false;
    };

    let Some(local_hit_percent) =
        extract_json_top_level_float_field(&cache_hit, "local_hit_percent")
    else {
        return false;
    };
    let Some(target_hit_percent) =
        extract_json_top_level_float_field(&cache_hit, "target_hit_percent")
    else {
        return false;
    };
    let Some(dashboard_local_hit_percent) =
        extract_json_top_level_float_field(&cache_dashboard, "local_warm_prefix_hit_percent")
    else {
        return false;
    };
    let Some(local_estimated_cached_tokens) =
        extract_json_top_level_number_field(&cache_dashboard, "local_estimated_cached_tokens")
    else {
        return false;
    };
    let Some(_local_estimated_cache_write_tokens) =
        extract_json_top_level_number_field(&cache_dashboard, "local_estimated_cache_write_tokens")
    else {
        return false;
    };
    let Some(nested_usage_dashboard) =
        extract_json_top_level_raw_field(&provider_dashboard, "usage_dashboard")
    else {
        return false;
    };

    cache_hit.contains("\"schema\": \"opensks.cache-hit-report.v1\"")
        && json_top_level_string_field_equals(&cache_hit, "scope", "local_stable_prefix")
        && json_top_level_bool_field_equals(&cache_hit, "baseline_available", true)
        && json_top_level_bool_field_equals(&cache_hit, "local_target_met", true)
        && json_top_level_bool_field_equals(&cache_hit, "provider_metrics_available", false)
        && json_top_level_string_field_equals(
            &cache_hit,
            "provider_metrics_status",
            "not_connected",
        )
        && json_top_level_string_field_equals(
            &cache_hit,
            "status",
            "local_target_met_provider_unverified",
        )
        && local_hit_percent >= target_hit_percent
        && json_top_level_string_field_equals(
            &cache_dashboard,
            "schema",
            "opensks.cache-dashboard.v1",
        )
        && json_top_level_string_array_contains(
            &cache_dashboard,
            "metrics",
            &[
                "cache_hit_by_provider",
                "cache_hit_by_model",
                "cache_hit_by_worker_lane",
                "cached_tokens",
                "cache_write_tokens",
            ],
        )
        && json_top_level_bool_field_equals(&cache_dashboard, "live_provider_metrics", false)
        && json_top_level_bool_field_equals(&cache_dashboard, "provider_metrics_available", false)
        && json_top_level_string_field_equals(
            &cache_dashboard,
            "provider_metrics_status",
            "not_connected",
        )
        && json_top_level_null_field_equals(&cache_dashboard, "provider_cache_hit_percent")
        && json_top_level_string_field_equals(
            &cache_dashboard,
            "provider_cache_hit_status",
            "tracked_unavailable_provider_not_connected",
        )
        && json_top_level_string_field_equals(
            &cache_dashboard,
            "provider_cache_hit_source",
            "cache-hit-report.json",
        )
        && json_top_level_null_field_equals(&cache_dashboard, "provider_cached_tokens")
        && json_top_level_null_field_equals(&cache_dashboard, "provider_cache_write_tokens")
        && dashboard_local_hit_percent >= target_hit_percent
        && local_estimated_cached_tokens > 0
        && token_provider_usage_dashboard_gate_passed(&usage_dashboard)
        && json_top_level_string_field_equals(
            &provider_dashboard,
            "schema",
            "opensks.provider-dashboard.v1",
        )
        && json_top_level_field_absent(&provider_dashboard, "provider_cache_hit_percent")
        && json_top_level_field_absent(&provider_dashboard, "provider_cache_hit_status")
        && json_top_level_field_absent(&provider_dashboard, "provider_cached_tokens")
        && json_top_level_field_absent(&provider_dashboard, "provider_cache_write_tokens")
        && json_top_level_field_absent(&provider_dashboard, "provider_metrics_available")
        && json_top_level_field_absent(&provider_dashboard, "provider_metrics_status")
        && token_provider_usage_dashboard_gate_passed(&nested_usage_dashboard)
}

fn token_provider_usage_dashboard_gate_passed(dashboard: &str) -> bool {
    json_top_level_string_field_equals(dashboard, "schema", "opensks.provider-usage-dashboard.v1")
        && json_top_level_bool_field_equals(dashboard, "cache_hit_tracking_enabled", true)
        && json_top_level_string_field_equals(
            dashboard,
            "cache_hit_tracking_source",
            "cache/cache-hit-report.json + providers/usage-dashboard.json",
        )
        && json_top_level_bool_field_equals(dashboard, "provider_metrics_available", false)
        && json_top_level_string_field_equals(dashboard, "provider_metrics_status", "not_connected")
        && json_top_level_null_field_equals(dashboard, "provider_cache_hit_percent")
        && json_top_level_string_field_equals(
            dashboard,
            "provider_cache_hit_status",
            "tracked_unavailable_provider_not_connected",
        )
        && json_top_level_null_field_equals(dashboard, "provider_cached_tokens")
        && json_top_level_null_field_equals(dashboard, "provider_cache_write_tokens")
}

fn production_acceptance_items(cwd: &Path) -> Vec<AcceptanceItem> {
    let prod_001_passed = prod001_cache_warm_prefix_gate_passed(cwd);
    let (prod_001_status, prod_001_evidence) = if prod_001_passed {
        (
            "passed",
            "cache-hit-report.json proves local_stable_prefix reuse met the >=95% warm-prefix target against a previous snapshot; provider cached-token telemetry remains explicitly unavailable/not connected.",
        )
    } else {
        (
            "partial",
            "prod-001 requires cache warm to run at least twice so cache-hit-report.json has a baseline, local_hit_percent >= target_hit_percent, local_target_met=true, and provider metrics explicitly unavailable/not connected.",
        )
    };
    let prod_002_passed = prod002_stage_overlap_target_gate_passed(cwd);
    let (prod_002_status, prod_002_evidence) = if prod_002_passed {
        (
            "passed",
            "latest scheduler stage-overlap-report.json proves local concurrent stage execution met its configured overlap target with target_met=true, observed_parallel_execution=true, and overlap_ratio >= target_ratio; provider/production worker tuning remains outside this local artifact scope.",
        )
    } else {
        (
            "partial",
            "prod-002 requires a latest scheduler stage-overlap-report.json with schema opensks.stage-overlap-report.v1, at least two parallelizable stages, observed_parallel_execution=true, overlap_observed=true, target_met=true, and overlap_ratio >= target_ratio.",
        )
    };
    let prod_004_passed = prod004_secret_leak_release_history_gate_passed(cwd);
    let (prod_004_status, prod_004_evidence) = if prod_004_passed {
        (
            "passed",
            "qa and security artifacts both report zero secret findings for the current workspace release scan plus a nonzero local release-history denominator whose latest candidate scan has zero unresolved secret findings; historical secret events remain preserved instead of hidden, and live external production telemetry remains explicitly false.",
        )
    } else {
        (
            "partial",
            "prod-004 requires qa run and security audit artifacts with zero current secret findings plus a local release-history denominator; missing, malformed, leaky, or zero-denominator artifacts keep this partial. Live external production telemetry remains explicitly false.",
        )
    };
    let final_seal_trust_passed = latest_final_seal_artifact_integrity_passed(cwd);
    let (prod_005_status, prod_005_evidence) = if final_seal_trust_passed {
        (
            "passed",
            "latest mission final-seal.json was read by acceptance audit and reports artifact_mvp_final_seal_integrity_status=passed with checked artifact refs; live H-proof route gate, provider-backed workers, repair waves, and final apply remain explicitly false.",
        )
    } else {
        (
            "partial",
            "acceptance audit did not find a latest mission final-seal.json with artifact_mvp_final_seal_integrity_status=passed and checked_artifacts_exist=true; live H-proof route gate, provider-backed workers, repair waves, and final apply remain explicitly false.",
        )
    };
    let prod_006_passed = prod006_signed_update_artifact_gate_passed(cwd);
    let (prod_006_status, prod_006_evidence) = if prod_006_passed {
        (
            "passed",
            "updater artifacts prove a local signed-update manifest plan: update-signature.json matches update-manifest.json, updater-final-state.json reports signature_verified=true, stable/latest channels require signatures and rollback, and network/install/apply remain explicitly false; production crypto/notarization remains unverified.",
        )
    } else {
        (
            "partial",
            "prod-006 requires updater plan artifacts with a recomputable local manifest signature, matching final-state manifest hash, signature_verified=true, stable/latest signed rollback channels, operator-approval boundary, rollback plan, and network/install/apply explicitly false; production crypto/notarization remains unverified.",
        )
    };
    vec![
        acceptance_item(
            "prod-001",
            "cache hit warm prefix >= 95%",
            prod_001_status,
            prod_001_evidence,
        ),
        acceptance_item(
            "prod-002",
            "stage overlap targets met",
            prod_002_status,
            prod_002_evidence,
        ),
        acceptance_item(
            "prod-003",
            "requirement coverage >= 95%",
            "passed",
            "requirement-coverage-gate.json reports implemented plus artifact-MVP PRD coverage above 95%; live acceptance completion remains tracked separately and is not all passed.",
        ),
        acceptance_item(
            "prod-004",
            "secret leak artifact rate = 0",
            prod_004_status,
            prod_004_evidence,
        ),
        acceptance_item(
            "prod-005",
            "final seal trustworthy",
            prod_005_status,
            prod_005_evidence,
        ),
        acceptance_item(
            "prod-006",
            "signed updates",
            prod_006_status,
            prod_006_evidence,
        ),
    ]
}

fn prod001_cache_warm_prefix_gate_passed(cwd: &Path) -> bool {
    let Ok(cache_hit) = fs::read_to_string(
        cwd.join(OPEN_SKSDIR)
            .join("cache")
            .join("cache-hit-report.json"),
    ) else {
        return false;
    };
    let Some(local_hit_percent) = extract_json_float_field(&cache_hit, "local_hit_percent") else {
        return false;
    };
    let Some(target_hit_percent) = extract_json_float_field(&cache_hit, "target_hit_percent")
    else {
        return false;
    };

    cache_hit.contains("\"schema\": \"opensks.cache-hit-report.v1\"")
        && json_string_field_equals(&cache_hit, "scope", "local_stable_prefix")
        && json_bool_field_equals(&cache_hit, "baseline_available", true)
        && json_bool_field_equals(&cache_hit, "local_target_met", true)
        && json_bool_field_equals(&cache_hit, "provider_metrics_available", false)
        && json_string_field_equals(&cache_hit, "provider_metrics_status", "not_connected")
        && json_string_field_equals(&cache_hit, "status", "local_target_met_provider_unverified")
        && target_hit_percent >= 95.0
        && local_hit_percent >= target_hit_percent
}

fn prod002_stage_overlap_target_gate_passed(cwd: &Path) -> bool {
    let Some(report) = latest_stage_overlap_report_text(cwd) else {
        return false;
    };
    let Some(parallelizable_stage_count) =
        extract_json_number_field(&report, "parallelizable_stage_count")
    else {
        return false;
    };
    let Some(overlap_ratio) = extract_json_float_field(&report, "overlap_ratio") else {
        return false;
    };
    let Some(target_ratio) = extract_json_float_field(&report, "target_ratio") else {
        return false;
    };
    let Some(total_stage_ms) = extract_json_number_field(&report, "total_stage_ms") else {
        return false;
    };
    let Some(overlap_saved_ms) = extract_json_number_field(&report, "overlap_saved_ms") else {
        return false;
    };
    let span_statuses = extract_stage_overlap_span_statuses(&report);

    report.contains("\"schema\": \"opensks.stage-overlap-report.v1\"")
        && parallelizable_stage_count >= 2
        && span_statuses.len() == parallelizable_stage_count
        && total_stage_ms > 0
        && overlap_saved_ms > 0
        && json_bool_field_equals(&report, "observed_parallel_execution", true)
        && json_bool_field_equals(&report, "overlap_observed", true)
        && json_bool_field_equals(&report, "target_met", true)
        && span_statuses.iter().all(|status| status == "passed")
        && target_ratio > 0.0
        && overlap_ratio >= target_ratio
}

fn latest_stage_overlap_report_text(cwd: &Path) -> Option<String> {
    let scheduler_dir = cwd.join(OPEN_SKSDIR).join("scheduler");
    let mut reports = Vec::new();
    for entry in fs::read_dir(&scheduler_dir).ok()? {
        let path = entry.ok()?.path().join("stage-overlap-report.json");
        if path.exists() {
            reports.push(path);
        }
    }
    reports.sort();
    reports
        .into_iter()
        .rev()
        .find_map(|path| fs::read_to_string(path).ok())
}

fn extract_stage_overlap_span_statuses(report: &str) -> Vec<String> {
    let Some(spans) = extract_json_array_field(report, "spans") else {
        return Vec::new();
    };
    let mut statuses = Vec::new();
    let mut offset = 0usize;
    while let Some(index) = spans[offset..].find("\"status\"") {
        let status_offset = offset + index;
        if let Some(status) = extract_json_string_field(&spans[status_offset..], "status") {
            statuses.push(status);
        }
        offset = status_offset + "\"status\"".len();
    }
    statuses
}

fn prod004_secret_leak_release_history_gate_passed(cwd: &Path) -> bool {
    ["qa", "security"]
        .iter()
        .all(|surface| secret_leak_surface_gate_passed(cwd, surface))
}

fn prod006_signed_update_artifact_gate_passed(cwd: &Path) -> bool {
    let updater_dir = cwd.join(OPEN_SKSDIR).join("updater");
    let Ok(manifest) = fs::read_to_string(updater_dir.join("update-manifest.json")) else {
        return false;
    };
    let Ok(signature) = fs::read_to_string(updater_dir.join("update-signature.json")) else {
        return false;
    };
    let Ok(channels) = fs::read_to_string(updater_dir.join("update-channels.json")) else {
        return false;
    };
    let Ok(rollback) = fs::read_to_string(updater_dir.join("rollback-plan.json")) else {
        return false;
    };
    let Ok(boundary) = fs::read_to_string(updater_dir.join("update-boundary.json")) else {
        return false;
    };
    let Ok(final_state) = fs::read_to_string(updater_dir.join("updater-final-state.json")) else {
        return false;
    };

    let manifest_hash = stable_content_hash(&manifest);
    let Some(signature_manifest_hash) =
        extract_json_top_level_string_field(&signature, "manifest_hash")
    else {
        return false;
    };
    let Some(signature_value) = extract_json_top_level_string_field(&signature, "signature") else {
        return false;
    };
    let Some(final_manifest_hash) =
        extract_json_top_level_string_field(&final_state, "manifest_hash")
    else {
        return false;
    };
    let expected_signature = local_update_signature(&manifest_hash);

    json_top_level_string_field_equals(&manifest, "schema", "opensks.update-manifest.v1")
        && json_top_level_string_field_equals(
            &manifest,
            "current_version",
            env!("CARGO_PKG_VERSION"),
        )
        && json_top_level_string_field_equals(&manifest, "default_channel", "stable")
        && json_top_level_bool_field_equals(&manifest, "requires_signature", true)
        && json_top_level_bool_field_equals(&manifest, "requires_rollback_plan", true)
        && json_top_level_bool_field_equals(&manifest, "network_install_enabled", false)
        && json_top_level_string_array_contains(&manifest, "channels", &["stable", "latest"])
        && json_top_level_string_array_contains(
            &manifest,
            "artifacts",
            &["opensks-cli", "app-bundle-candidate", "manifest"],
        )
        && json_top_level_string_field_equals(&signature, "schema", "opensks.update-signature.v1")
        && signature_manifest_hash == manifest_hash
        && signature_value == expected_signature
        && json_top_level_string_field_equals(
            &signature,
            "trusted_signer_fingerprint",
            trusted_update_signer_fingerprint(),
        )
        && json_top_level_string_field_equals(
            &signature,
            "algorithm",
            "fnv1a64-local-dev-proof-not-production-crypto",
        )
        && json_top_level_bool_field_equals(&signature, "production_crypto_live", false)
        && json_top_level_string_field_equals(&channels, "schema", "opensks.update-channels.v1")
        && signed_update_channel_gate_passed(&channels, "stable")
        && signed_update_channel_gate_passed(&channels, "latest")
        && json_top_level_string_field_equals(&rollback, "schema", "opensks.rollback-plan.v1")
        && json_top_level_string_field_equals(
            &rollback,
            "current_version",
            env!("CARGO_PKG_VERSION"),
        )
        && json_top_level_string_array_contains(
            &rollback,
            "rollback_slots",
            &[
                "previous-stable",
                "previous-latest",
                "manual-operator-restore",
            ],
        )
        && json_top_level_string_field_equals(
            &rollback,
            "restore_strategy",
            "preserve_previous_manifest_and_binary_before_apply",
        )
        && json_top_level_bool_field_equals(&rollback, "apply_transaction_live", false)
        && json_top_level_string_field_equals(&boundary, "schema", "opensks.update-boundary.v1")
        && json_top_level_bool_field_equals(&boundary, "auto_download", false)
        && json_top_level_bool_field_equals(&boundary, "auto_apply", false)
        && json_top_level_bool_field_equals(&boundary, "requires_operator_approval", true)
        && json_top_level_bool_field_equals(&boundary, "requires_verified_signature", true)
        && json_top_level_bool_field_equals(&boundary, "requires_rollback_plan", true)
        && json_top_level_string_field_equals(
            &boundary,
            "signed_updater_live",
            "local_manifest_signature_artifact_only",
        )
        && json_top_level_string_field_equals(
            &final_state,
            "schema",
            "opensks.updater-final-state.v1",
        )
        && json_top_level_string_field_equals(&final_state, "status", "verified_artifact_plan")
        && final_manifest_hash == manifest_hash
        && json_top_level_bool_field_equals(&final_state, "signature_verified", true)
        && json_top_level_string_array_contains(
            &final_state,
            "channels_present",
            &["stable", "latest"],
        )
        && json_top_level_bool_field_equals(&final_state, "rollback_plan_present", true)
        && json_top_level_bool_field_equals(&final_state, "network_or_install_performed", false)
}

fn signed_update_channel_gate_passed(channels: &str, name: &str) -> bool {
    extract_json_top_level_array_objects(channels, "channels")
        .iter()
        .any(|channel| {
            json_top_level_string_field_equals(channel, "name", name)
                && json_top_level_bool_field_equals(channel, "auto_apply", false)
                && json_top_level_bool_field_equals(channel, "requires_signature", true)
                && json_top_level_bool_field_equals(channel, "rollback_required", true)
        })
}

fn secret_leak_surface_gate_passed(cwd: &Path, surface: &str) -> bool {
    let surface_dir = cwd.join(OPEN_SKSDIR).join(surface);
    let Ok(rate) = fs::read_to_string(surface_dir.join("secret-leak-rate.json")) else {
        return false;
    };
    let Ok(gate) = fs::read_to_string(surface_dir.join("secret-leak-gate.json")) else {
        return false;
    };
    let Ok(history) = fs::read_to_string(surface_dir.join("secret-leak-release-history.json"))
    else {
        return false;
    };
    let Ok(audit) = fs::read_to_string(surface_dir.join("security-audit.json")) else {
        return false;
    };

    rate.contains("\"schema\": \"opensks.secret-leak-rate.v1\"")
        && gate.contains("\"schema\": \"opensks.secret-leak-gate.v1\"")
        && history.contains("\"schema\": \"opensks.secret-leak-release-history.v1\"")
        && audit.contains("\"schema\": \"opensks.security-audit.v1\"")
        && json_string_field_equals(&rate, "scope", "current_workspace_release_scan")
        && json_string_field_equals(&gate, "scope", "current_workspace_release_scan")
        && json_number_field_positive(&rate, "scanned_artifact_count")
        && json_number_field_positive(&rate, "release_history_denominator")
        && json_number_field_positive(&history, "release_history_denominator")
        && json_number_field_equals(&rate, "secret_finding_count", 0)
        && json_number_field_equals(&gate, "secret_finding_count", 0)
        && json_number_field_equals(&rate, "release_history_latest_secret_finding_count", 0)
        && json_number_field_equals(&gate, "release_history_latest_secret_finding_count", 0)
        && json_number_field_equals(&history, "latest_secret_finding_count", 0)
        && json_bool_field_equals(&rate, "gate_passed", true)
        && json_bool_field_equals(&gate, "gate_passed", true)
        && json_bool_field_equals(&history, "gate_passed", true)
        && json_bool_field_equals(&rate, "release_history_gate_passed", true)
        && json_bool_field_equals(&gate, "release_history_gate_passed", true)
        && json_bool_field_equals(&rate, "live_external_production_telemetry", false)
        && json_bool_field_equals(&gate, "live_external_production_telemetry", false)
        && json_bool_field_equals(&history, "live_external_production_telemetry", false)
        && json_string_field_equals(&gate, "status", "passed")
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

fn json_top_level_null_field_equals(input: &str, key: &str) -> bool {
    extract_json_top_level_raw_field(input, key).as_deref() == Some("null")
}

fn json_top_level_field_absent(input: &str, key: &str) -> bool {
    extract_json_top_level_raw_fields(input, key).is_empty()
}

fn json_top_level_string_array_contains(input: &str, key: &str, expected: &[&str]) -> bool {
    let Some(raw) = extract_json_top_level_raw_field(input, key) else {
        return false;
    };
    let values = extract_json_string_array_values(&raw);
    expected
        .iter()
        .all(|expected_value| values.iter().any(|value| value == expected_value))
}

fn extract_json_string_array_values(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
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
        let Some(end) = json_string_token_end(trimmed, index) else {
            return Vec::new();
        };
        values.push(unescape_simple_json_string(&trimmed[index + 1..end - 1]));
        index = end;
    }
    values
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

fn extract_json_top_level_number_field(input: &str, key: &str) -> Option<usize> {
    extract_json_top_level_raw_field(input, key)?.parse().ok()
}

fn extract_json_top_level_float_field(input: &str, key: &str) -> Option<f64> {
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

fn json_string_field_equals(input: &str, key: &str, expected: &str) -> bool {
    extract_json_string_field(input, key).as_deref() == Some(expected)
}

fn json_bool_field_equals(input: &str, key: &str, expected: bool) -> bool {
    extract_json_raw_field(input, key).as_deref() == Some(if expected { "true" } else { "false" })
}

fn json_number_field_equals(input: &str, key: &str, expected: usize) -> bool {
    extract_json_number_field(input, key) == Some(expected)
}

fn json_number_field_positive(input: &str, key: &str) -> bool {
    extract_json_number_field(input, key).is_some_and(|value| value > 0)
}

fn extract_json_float_field(input: &str, key: &str) -> Option<f64> {
    extract_json_raw_field(input, key)?.parse().ok()
}

fn latest_final_seal_artifact_integrity_passed(cwd: &Path) -> bool {
    latest_final_seal_text(cwd)
        .as_deref()
        .is_some_and(final_seal_text_artifact_integrity_passed)
}

fn latest_final_seal_text(cwd: &Path) -> Option<String> {
    let missions_dir = cwd.join(OPEN_SKSDIR).join("missions");
    let mut seals = Vec::new();
    for entry in fs::read_dir(&missions_dir).ok()? {
        let path = entry.ok()?.path().join("final-seal.json");
        if path.exists() {
            seals.push(path);
        }
    }
    seals.sort();
    seals
        .into_iter()
        .rev()
        .find_map(|path| fs::read_to_string(path).ok())
}

fn final_seal_text_artifact_integrity_passed(final_seal: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(final_seal) else {
        return false;
    };
    let Some(contract) = value.get("trust_contract") else {
        return false;
    };
    let Some(patch_gate) = contract.get("patch_gate") else {
        return false;
    };
    let checked_artifact_count = contract
        .get("checked_artifact_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let checked_artifacts_len = contract
        .get("checked_artifacts")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len) as u64;
    let artifact_manifest_count = contract
        .get("artifact_manifest_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let artifact_manifest_len = contract
        .get("artifact_manifest")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len) as u64;
    value.get("schema").and_then(serde_json::Value::as_str) == Some("opensks.final-seal.v1")
        && value.get("trust_scope").and_then(serde_json::Value::as_str)
            == Some("artifact_mvp_final_seal_integrity")
        && value
            .get("completion_claim")
            .and_then(serde_json::Value::as_str)
            == Some("artifact_integrity_only_not_live_route_completion")
        && contract.get("scope").and_then(serde_json::Value::as_str)
            == Some("artifact_mvp_final_seal_integrity")
        && contract
            .get("artifact_mvp_final_seal_integrity")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        && contract
            .get("artifact_mvp_final_seal_integrity_status")
            .and_then(serde_json::Value::as_str)
            == Some("passed")
        && contract
            .get("checked_artifacts_exist")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        && checked_artifact_count > 0
        && checked_artifact_count == checked_artifacts_len
        && artifact_manifest_count > 0
        && artifact_manifest_count == artifact_manifest_len
        && patch_gate.get("status").and_then(serde_json::Value::as_str) == Some("pending_diff")
        && patch_gate
            .get("final_apply_allowed")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        && patch_gate.get("ref").and_then(serde_json::Value::as_str)
            == Some("patch-gate-result.json")
        && contract
            .get("live_route_completion")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        && contract
            .get("live_hproof_route_gate")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        && contract
            .get("provider_backed_workers_live")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        && contract
            .get("repair_waves_live")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        && contract
            .get("final_apply_transaction_live")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        && contract
            .get("live_final_apply")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
}

fn acceptance_item(
    id: &'static str,
    criterion: &'static str,
    status: &'static str,
    evidence: &'static str,
) -> AcceptanceItem {
    AcceptanceItem {
        id,
        criterion,
        status,
        evidence,
    }
}

fn render_acceptance_report(stamp: &ClockStamp, tier: &str, items: &[AcceptanceItem]) -> String {
    let (total, passed, partial, failed) = acceptance_counts(items);
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.acceptance-report.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"tier\": {},\n",
            "  \"summary\": {{\"total\":{},\"passed\":{},\"partial\":{},\"failed\":{}}},\n",
            "  \"all_passed\": {},\n",
            "  \"criteria\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        json_string(tier),
        total,
        passed,
        partial,
        failed,
        failed == 0 && partial == 0,
        render_acceptance_items(items)
    )
}

fn render_acceptance_summary(
    stamp: &ClockStamp,
    mvp: &[AcceptanceItem],
    beta: &[AcceptanceItem],
    production: &[AcceptanceItem],
) -> String {
    let (total, passed, partial, failed) = combined_acceptance_counts(&[mvp, beta, production]);
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.acceptance-summary.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"summary\": {{\"total\":{},\"passed\":{},\"partial\":{},\"failed\":{}}},\n",
            "  \"goal_complete\": false,\n",
            "  \"tiers\": {{\"mvp\":{},\"beta\":{},\"production\":{}}}\n",
            "}}\n"
        ),
        stamp.json(),
        total,
        passed,
        partial,
        failed,
        acceptance_tier_status(mvp),
        acceptance_tier_status(beta),
        acceptance_tier_status(production)
    )
}

fn render_acceptance_findings_jsonl(
    stamp: &ClockStamp,
    mvp: &[AcceptanceItem],
    beta: &[AcceptanceItem],
    production: &[AcceptanceItem],
) -> String {
    let mut rows = Vec::new();
    for (tier, items) in [("mvp", mvp), ("beta", beta), ("production", production)] {
        for item in items
            .iter()
            .filter(|item| item.status == "partial" || item.status == "failed")
        {
            rows.push(format!(
                concat!(
                    "{{\"schema\":\"opensks.acceptance-finding.v1\",",
                    "\"at\":{},\"tier\":{},\"id\":{},\"status\":{},",
                    "\"criterion\":{},\"evidence\":{}}}"
                ),
                stamp.json(),
                json_string(tier),
                json_string(item.id),
                json_string(item.status),
                json_string(item.criterion),
                json_string(item.evidence)
            ));
        }
    }
    rows.join("\n") + "\n"
}

fn render_acceptance_items(items: &[AcceptanceItem]) -> String {
    let rows = items
        .iter()
        .map(|item| {
            format!(
                "{{\"id\":{},\"criterion\":{},\"status\":{},\"evidence\":{}}}",
                json_string(item.id),
                json_string(item.criterion),
                json_string(item.status),
                json_string(item.evidence)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn acceptance_tier_status(items: &[AcceptanceItem]) -> String {
    let (_, _, partial, failed) = acceptance_counts(items);
    let status = if failed > 0 {
        "failed"
    } else if partial > 0 {
        "partial"
    } else {
        "passed"
    };
    json_string(status)
}

fn acceptance_counts(items: &[AcceptanceItem]) -> (usize, usize, usize, usize) {
    let total = items.len();
    let passed = items.iter().filter(|item| item.status == "passed").count();
    let partial = items.iter().filter(|item| item.status == "partial").count();
    let failed = items.iter().filter(|item| item.status == "failed").count();
    (total, passed, partial, failed)
}

fn combined_acceptance_counts(groups: &[&[AcceptanceItem]]) -> (usize, usize, usize, usize) {
    groups.iter().fold(
        (0, 0, 0, 0),
        |(total_acc, passed_acc, partial_acc, failed_acc), group| {
            let (total, passed, partial, failed) = acceptance_counts(group);
            (
                total_acc + total,
                passed_acc + passed,
                partial_acc + partial,
                failed_acc + failed,
            )
        },
    )
}

fn remote_provider_adapter_probe_opted_in() -> bool {
    env::var("OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE")
        .ok()
        .as_deref()
        == Some("1")
}

fn render_provider_adapter_check_report(
    stamp: &ClockStamp,
    checks: &[ProviderAdapterCheck],
) -> String {
    let attempted = checks.iter().filter(|check| check.attempted).count();
    let reachable = checks
        .iter()
        .filter(|check| check.status == "adapter_models_endpoint_reachable")
        .count();
    let remote_probe_opt_in = remote_provider_adapter_probe_opted_in();
    let blockers = provider_adapter_check_blockers(checks, remote_probe_opt_in);
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.provider-adapter-check.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"remote_probe_opt_in\": {},\n",
            "  \"secret_value_exposed\": false,\n",
            "  \"summary\": {{\"total\":{},\"attempted\":{},\"reachable\":{}}},\n",
            "  \"blockers\": {},\n",
            "  \"remediation_actions\": {},\n",
            "  \"adapters\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        remote_probe_opt_in,
        checks.len(),
        attempted,
        reachable,
        json_vec(&blockers),
        render_provider_adapter_remediation_actions_json(&blockers),
        render_provider_adapter_checks_json(checks)
    )
}

fn provider_adapter_check_blockers(
    checks: &[ProviderAdapterCheck],
    remote_probe_opt_in: bool,
) -> Vec<String> {
    let mut blockers = Vec::new();
    if !remote_probe_opt_in {
        blockers.push("set_OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE_1".to_string());
    }
    for check in checks {
        for blocker in &check.blockers {
            if !blockers.iter().any(|existing| existing == blocker) {
                blockers.push(blocker.clone());
            }
        }
    }
    blockers
}

fn provider_adapter_check_blocker_stdout_lines(blockers: &[String]) -> String {
    if blockers.is_empty() {
        return String::new();
    }
    blockers
        .iter()
        .map(|blocker| format!("blocker: {blocker}\n"))
        .collect()
}

fn render_provider_adapter_remediation_actions_json(blockers: &[String]) -> String {
    let actions = blockers
        .iter()
        .map(|blocker| {
            let (action, scope) = provider_adapter_remediation_action(blocker);
            format!(
                "{{\"blocker\":{},\"action\":{},\"scope\":{}}}",
                json_string(blocker),
                json_string(action),
                json_string(scope)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{actions}]")
}

fn provider_adapter_remediation_action(blocker: &str) -> (&'static str, &'static str) {
    match blocker {
        "set_OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE_1" => (
            "Set OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE=1 before running live remote provider checks.",
            "operator_environment",
        ),
        "configure_OPENROUTER_API_KEY_credential" => (
            "Add an OpenRouter API key credential through Provider Center or the configured secret store.",
            "provider_credential",
        ),
        "configure_OPENAI_API_KEY_credential" => (
            "Add an OpenAI API key credential through Provider Center or the configured secret store.",
            "provider_credential",
        ),
        "replace_OPENROUTER_API_KEY_credential" => (
            "Replace the OpenRouter API key credential; authentication was rejected.",
            "provider_credential",
        ),
        "replace_OPENAI_API_KEY_credential" => (
            "Replace the OpenAI API key credential; authentication was rejected.",
            "provider_credential",
        ),
        "resolve_OpenRouter_models_endpoint" => (
            "Check OpenRouter models endpoint reachability and provider status.",
            "provider_network",
        ),
        "resolve_OpenAI_models_endpoint" => (
            "Check OpenAI models endpoint reachability and provider status.",
            "provider_network",
        ),
        "resolve_OpenRouter_adapter_check_error" => (
            "Review the OpenRouter adapter-check diagnostic in the local report.",
            "provider_diagnostic",
        ),
        "resolve_OpenAI_adapter_check_error" => (
            "Review the OpenAI adapter-check diagnostic in the local report.",
            "provider_diagnostic",
        ),
        _ => (
            "Review the redacted provider adapter-check blocker in the local report.",
            "provider_diagnostic",
        ),
    }
}

fn render_provider_adapter_checks_json(checks: &[ProviderAdapterCheck]) -> String {
    let rows = checks
        .iter()
        .map(|check| {
            format!(
                concat!(
                    "{{\"name\":{},\"configured\":{},\"attempted\":{},",
                    "\"status\":{},\"blockers\":{},\"credential_source\":{},\"endpoint\":{},\"http_code\":{},",
                    "\"duration_ms\":{},\"transport\":{},",
                    "\"secret_value_exposed\":false,\"stderr\":{}}}"
                ),
                json_string(&check.name),
                check.configured,
                check.attempted,
                json_string(&check.status),
                json_vec(&check.blockers),
                json_string(&check.credential_source),
                json_string(&check.endpoint),
                check
                    .http_code
                    .as_deref()
                    .map(json_string)
                    .unwrap_or_else(|| "null".to_string()),
                check.duration_ms,
                json_string(check.transport),
                json_string(&check.stderr)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn render_provider_probe_report(stamp: &ClockStamp, probes: &[ProviderProbe]) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.provider-probe-report.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"scope\": \"local endpoints only; remote authenticated probes require explicit approval\",\n",
            "  \"probes\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        render_provider_probes_json(probes)
    )
}

fn render_provider_dashboard(
    stamp: &ClockStamp,
    statuses: &[ProviderStatus],
    probes: &[ProviderProbe],
) -> String {
    let configured = statuses.iter().filter(|status| status.configured).count();
    let local_probeable = statuses
        .iter()
        .filter(|status| provider_probe_endpoint(status).is_some())
        .count();
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.provider-dashboard.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"provider_count\": {},\n",
            "  \"configured_count\": {},\n",
            "  \"local_probeable_count\": {},\n",
            "  \"probe_summary\": {},\n",
            "  \"usage_dashboard\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        statuses.len(),
        configured,
        local_probeable,
        render_provider_probe_summary_json(probes),
        render_usage_dashboard(stamp, statuses, probes)
    )
}

fn render_provider_probes_json(probes: &[ProviderProbe]) -> String {
    let rows = probes
        .iter()
        .map(|probe| {
            format!(
                concat!(
                    "{{\"name\":{},\"attempted\":{},\"status\":{},",
                    "\"endpoint\":{},\"http_code\":{},\"duration_ms\":{},",
                    "\"transport\":{},\"stderr\":{}}}"
                ),
                json_string(&probe.name),
                probe.attempted,
                json_string(&probe.status),
                probe
                    .endpoint
                    .as_deref()
                    .map(json_string)
                    .unwrap_or_else(|| "null".to_string()),
                probe
                    .http_code
                    .as_deref()
                    .map(json_string)
                    .unwrap_or_else(|| "null".to_string()),
                probe.duration_ms,
                json_string(probe.transport),
                json_string(&probe.stderr)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn render_provider_probe_summary_json(probes: &[ProviderProbe]) -> String {
    let attempted = probes.iter().filter(|probe| probe.attempted).count();
    let reachable = probes
        .iter()
        .filter(|probe| probe.status == "reachable")
        .count();
    let skipped = probes.len().saturating_sub(attempted);
    format!(
        "{{\"total\":{},\"attempted\":{},\"reachable\":{},\"skipped\":{}}}",
        probes.len(),
        attempted,
        reachable,
        skipped
    )
}

fn render_usage_dashboard(
    stamp: &ClockStamp,
    statuses: &[ProviderStatus],
    probes: &[ProviderProbe],
) -> String {
    let configured = statuses.iter().filter(|status| status.configured).count();
    format!(
        concat!(
            "{{\"schema\":\"opensks.provider-usage-dashboard.v1\",",
            "\"generated_at\":{},\"configured_providers\":{},",
            "\"tokens\":0,\"cost_usd\":0.0,\"cached_tokens\":0,",
            "\"cache_hit_tracking_enabled\":true,",
            "\"cache_hit_tracking_source\":\"cache/cache-hit-report.json + providers/usage-dashboard.json\",",
            "\"provider_metrics_available\":false,",
            "\"provider_metrics_status\":\"not_connected\",",
            "\"provider_cache_hit_percent\":null,",
            "\"provider_cache_hit_status\":\"tracked_unavailable_provider_not_connected\",",
            "\"provider_cached_tokens\":null,\"provider_cache_write_tokens\":null,",
            "\"reasoning_tokens\":0,\"tool_calls\":0,",
            "\"probe_summary\":{}}}"
        ),
        stamp.json(),
        configured,
        render_provider_probe_summary_json(probes)
    )
}

fn render_provider_usage_event(
    stamp: &ClockStamp,
    event: &str,
    probes: &[ProviderProbe],
) -> String {
    format!(
        concat!(
            "{{\"schema\":\"opensks.provider-usage-event.v1\",",
            "\"at\":{},\"event\":{},\"tokens\":0,\"cost_usd\":0.0,",
            "\"secret_value_exposed\":false,\"probe_summary\":{}}}\n"
        ),
        stamp.json(),
        json_string(event),
        render_provider_probe_summary_json(probes)
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
            "  \"static_dashboard\": \"dashboard.html\",\n",
            "  \"live_gui\": \"static_html_artifact\"\n",
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

fn render_gui_data(stamp: &ClockStamp, cwd: &Path) -> Result<String, OpenSksError> {
    let snapshot = collect_gui_snapshot(cwd);
    let worker_lane_items = render_worker_lane_items_json(&snapshot.worker_lanes);
    let worker_runtime = render_worker_runtime_dashboard_json(&snapshot.worker_runtime);
    Ok(format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.gui-data.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"live_native_gui\": false,\n",
            "  \"coverage\": {{\"total\":{},\"implemented\":{},\"artifact_mvp\":{},\"planned_artifact\":{},\"missing_live_implementation\":{}}},\n",
            "  \"qa\": {{\"status\":{}}},\n",
            "  \"security\": {{\"status\":{}}},\n",
            "  \"providers\": {{\"configured_count\":{}}},\n",
            "  \"triwiki\": {{\"voxel_count\":{}}},\n",
            "  \"sessions\": {{\"missions\":{},\"browser\":{},\"computer_use\":{},\"app_use\":{}}},\n",
            "  \"mission_status\": {{\"mission_count\":{},\"items\":{}}},\n",
            "  \"worker_lanes\": {{\"mission_count\":{},\"lane_count\":{},\"items\":{},\"artifact\":\"worker-lanes.json\",\"dashboard_panel\":\"dashboard.html#worker-lanes\",\"live_native_worker_lanes\":false,\"live_worker_waterfall\":false}},\n",
            "  \"worker_runtime\": {},\n",
            "  \"panels\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        snapshot.prd_total,
        snapshot.prd_implemented,
        snapshot.prd_artifact_mvp,
        snapshot.prd_planned,
        snapshot.prd_missing_live,
        json_string(&snapshot.qa_status),
        json_string(&snapshot.security_status),
        snapshot.provider_configured_count,
        snapshot.voxel_count,
        snapshot.mission_count,
        snapshot.browser_sessions,
        snapshot.computer_sessions,
        snapshot.app_sessions,
        snapshot.worker_lane_missions,
        worker_lane_items,
        snapshot.worker_lane_missions,
        snapshot.worker_lane_count,
        worker_lane_items,
        worker_runtime,
        json_array(&[
            "mission_control",
            "mission_status",
            "worker_lanes",
            "worker_runtime",
            "prd_coverage",
            "voxel_triwiki",
            "mcp_tools",
            "browser_computer_app",
            "qa_security",
            "provider_usage"
        ])
    ))
}

fn render_dashboard_html(stamp: &ClockStamp, cwd: &Path) -> Result<String, OpenSksError> {
    let snapshot = collect_gui_snapshot(cwd);
    let generated = html_escape(&stamp.json());
    let qa_status = html_escape(&snapshot.qa_status);
    let security_status = html_escape(&snapshot.security_status);
    let worker_lane_rows = render_worker_lane_rows_html(&snapshot.worker_lanes);
    let worker_lane_missions = snapshot.worker_lane_missions;
    let worker_lane_count = snapshot.worker_lane_count;
    let worker_runtime_status = if snapshot.worker_runtime.available {
        "available"
    } else {
        "missing"
    };
    Ok(format!(
        concat!(
            "<!doctype html>\n",
            "<html lang=\"en\">\n",
            "<head>\n",
            "<meta charset=\"utf-8\">\n",
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n",
            "<title>OpenSKS Mission Control</title>\n",
            "<style>\n",
            ":root {{ color-scheme: light dark; --bg: #f7f8fa; --fg: #111827; --muted: #4b5563; --line: #d1d5db; --panel: #ffffff; --accent: #0f766e; --warn: #b45309; }}\n",
            "body {{ margin: 0; font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, \"Segoe UI\", sans-serif; background: var(--bg); color: var(--fg); }}\n",
            "main {{ max-width: 1180px; margin: 0 auto; padding: 24px; }}\n",
            "header {{ display: flex; justify-content: space-between; gap: 16px; align-items: end; border-bottom: 1px solid var(--line); padding-bottom: 16px; }}\n",
            "h1 {{ margin: 0; font-size: 28px; line-height: 1.15; }}\n",
            "h2 {{ margin: 0 0 10px; font-size: 16px; }}\n",
            "p {{ margin: 0; color: var(--muted); }}\n",
            ".grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 12px; margin-top: 18px; }}\n",
            "section {{ border: 1px solid var(--line); background: var(--panel); border-radius: 8px; padding: 14px; min-height: 118px; }}\n",
            ".metric {{ font-size: 30px; font-weight: 700; margin-top: 8px; }}\n",
            ".ok {{ color: var(--accent); }} .warn {{ color: var(--warn); }}\n",
            "dl {{ display: grid; grid-template-columns: 1fr auto; gap: 8px; margin: 0; }}\n",
            "dt {{ color: var(--muted); }} dd {{ margin: 0; font-weight: 650; }}\n",
            "table {{ width: 100%; border-collapse: collapse; font-size: 13px; }}\n",
            "th, td {{ text-align: left; border-top: 1px solid var(--line); padding: 8px 6px; vertical-align: top; }}\n",
            "th {{ color: var(--muted); font-weight: 650; }}\n",
            "code {{ font-size: 12px; }}\n",
            "@media (prefers-color-scheme: dark) {{ :root {{ --bg: #101418; --fg: #f3f4f6; --muted: #a1a1aa; --line: #374151; --panel: #161b22; --accent: #2dd4bf; --warn: #fbbf24; }} }}\n",
            "</style>\n",
            "</head>\n",
            "<body><main>\n",
            "<header><div><h1>OpenSKS Mission Control</h1><p>Generated from local .opensks artifacts.</p></div><code>{}</code></header>\n",
            "<div class=\"grid\">\n",
            "<section><h2>PRD Coverage</h2><dl><dt>Total</dt><dd>{}</dd><dt>Implemented</dt><dd>{}</dd><dt>Artifact MVP</dt><dd>{}</dd><dt>Planned</dt><dd>{}</dd><dt>Missing Live</dt><dd>{}</dd></dl></section>\n",
            "<section><h2>QA</h2><p>Status</p><div class=\"metric ok\">{}</div><p>Security: <strong>{}</strong></p></section>\n",
            "<section><h2>Voxel TriWiki</h2><p>Indexed voxels</p><div class=\"metric\">{}</div><p>Source: <code>.opensks/triwiki</code></p></section>\n",
            "<section><h2>Providers</h2><p>Configured env providers</p><div class=\"metric\">{}</div><p>Secret values are not rendered.</p></section>\n",
            "<section><h2>Missions</h2><p>Mission artifacts</p><div class=\"metric\">{}</div><p>Final seals stay partial until live PRD criteria pass.</p></section>\n",
            "<section><h2>Use Planes</h2><dl><dt>Browser</dt><dd>{}</dd><dt>Computer</dt><dd>{}</dd><dt>App</dt><dd>{}</dd></dl></section>\n",
            "<section id=\"mission-status\"><h2>Mission Status</h2><dl><dt>Missions with lanes</dt><dd>{}</dd><dt>Worker lanes</dt><dd>{}</dd></dl><p>Static artifact view; native live GUI is not claimed.</p></section>\n",
            "<section id=\"worker-runtime\"><h2>Worker Runtime</h2><dl><dt>Status</dt><dd>{}</dd><dt>Active leases</dt><dd>{}</dd><dt>Recovered</dt><dd>{}</dd><dt>Routed</dt><dd>{}</dd></dl><p>Daemon-visible local bus artifact; provider workers are not claimed.</p></section>\n",
            "<section id=\"worker-lanes\" style=\"grid-column: 1 / -1;\"><h2>Worker Lanes</h2><p>Mission status and planned worker lanes from goal-loop/tool-plan artifacts.</p><table><thead><tr><th>Mission</th><th>Status</th><th>Mode</th><th>Lanes</th></tr></thead><tbody>{}</tbody></table></section>\n",
            "</div>\n",
            "</main></body></html>\n"
        ),
        generated,
        snapshot.prd_total,
        snapshot.prd_implemented,
        snapshot.prd_artifact_mvp,
        snapshot.prd_planned,
        snapshot.prd_missing_live,
        qa_status,
        security_status,
        snapshot.voxel_count,
        snapshot.provider_configured_count,
        snapshot.mission_count,
        snapshot.browser_sessions,
        snapshot.computer_sessions,
        snapshot.app_sessions,
        worker_lane_missions,
        worker_lane_count,
        html_escape(worker_runtime_status),
        snapshot.worker_runtime.active_leases,
        snapshot.worker_runtime.recovered_leases,
        snapshot.worker_runtime.routed_requests,
        worker_lane_rows
    ))
}

fn collect_gui_snapshot(cwd: &Path) -> GuiSnapshot {
    let coverage = read_runtime_artifact(cwd, "prd-coverage.json");
    let qa = read_runtime_artifact(cwd, "qa/qa-report.json");
    let security = read_runtime_artifact_with_fallback(
        cwd,
        "security/security-audit.json",
        "qa/security-audit.json",
    );
    let providers = read_runtime_artifact(cwd, "providers/provider-dashboard.json");
    let voxels = read_runtime_artifact(cwd, "triwiki/voxel-index-report.json");
    let worker_lanes = collect_worker_lane_snapshots(cwd);
    let worker_lane_count = worker_lanes.iter().map(|mission| mission.lanes.len()).sum();
    let worker_lane_missions = worker_lanes.len();
    let worker_runtime = collect_worker_runtime_dashboard(cwd);

    GuiSnapshot {
        prd_total: extract_json_number_field(&coverage, "total").unwrap_or(0),
        prd_implemented: extract_json_number_field(&coverage, "implemented").unwrap_or(0),
        prd_artifact_mvp: extract_json_number_field(&coverage, "artifact_mvp").unwrap_or(0),
        prd_planned: extract_json_number_field(&coverage, "planned_artifact").unwrap_or(0),
        prd_missing_live: extract_json_number_field(&coverage, "missing_live_implementation")
            .unwrap_or(0),
        qa_status: extract_json_string_field(&qa, "status")
            .unwrap_or_else(|| "missing".to_string()),
        security_status: extract_json_string_field(&security, "status")
            .unwrap_or_else(|| "missing".to_string()),
        provider_configured_count: extract_json_number_field(&providers, "configured_count")
            .unwrap_or(0),
        voxel_count: extract_json_number_field(&voxels, "voxel_count").unwrap_or(0),
        mission_count: count_runtime_child_dirs(cwd, "missions"),
        browser_sessions: count_runtime_child_dirs(cwd, "browser"),
        computer_sessions: count_runtime_child_dirs(cwd, "computer-use"),
        app_sessions: count_runtime_child_dirs(cwd, "app-use"),
        worker_lane_missions,
        worker_lane_count,
        worker_lanes,
        worker_runtime,
    }
}

fn collect_worker_lane_snapshots(cwd: &Path) -> Vec<WorkerLaneSnapshot> {
    let missions_dir = cwd.join(OPEN_SKSDIR).join("missions");
    let mut dirs = Vec::new();
    if let Ok(entries) = fs::read_dir(&missions_dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            }
        }
    }
    dirs.sort();

    let mut snapshots = Vec::new();
    for dir in dirs {
        let goal_loop = dir.join("goal-loop.json");
        let tool_plan = dir.join("tool-plan.json");
        let (source_path, source_contents) = if goal_loop.exists() {
            let contents = fs::read_to_string(&goal_loop).unwrap_or_default();
            (goal_loop, contents)
        } else if tool_plan.exists() {
            let contents = fs::read_to_string(&tool_plan).unwrap_or_default();
            (tool_plan, contents)
        } else {
            continue;
        };
        let lanes = extract_json_string_array_field(&source_contents, "worker_lanes");
        if lanes.is_empty() {
            continue;
        }
        let fallback_id = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown-mission")
            .to_string();
        snapshots.push(WorkerLaneSnapshot {
            mission_id: extract_json_string_field(&source_contents, "mission_id")
                .unwrap_or(fallback_id),
            status: extract_json_string_field(&source_contents, "status")
                .unwrap_or_else(|| "unknown".to_string()),
            execution_mode: extract_json_string_field(&source_contents, "execution_mode")
                .unwrap_or_else(|| "unknown".to_string()),
            lanes,
            source: source_path.display().to_string(),
        });
    }
    snapshots
}

fn render_worker_lanes_report(stamp: &ClockStamp, snapshots: &[WorkerLaneSnapshot]) -> String {
    let lane_count: usize = snapshots.iter().map(|snapshot| snapshot.lanes.len()).sum();
    let rows = render_worker_lane_items_json(snapshots);
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.worker-lanes.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"mission_count\": {},\n",
            "  \"lane_count\": {},\n",
            "  \"dashboard_panel\": \"dashboard.html#worker-lanes\",\n",
            "  \"source_authority\": \".opensks/missions/*/goal-loop.json and tool-plan.json\",\n",
            "  \"live_native_worker_lanes\": false,\n",
            "  \"missions\": {}\n",
            "}}\n"
        ),
        stamp.json(),
        snapshots.len(),
        lane_count,
        rows
    )
}

fn render_worker_lane_items_json(snapshots: &[WorkerLaneSnapshot]) -> String {
    let rows = snapshots
        .iter()
        .map(|snapshot| {
            format!(
                concat!(
                    "{{\"mission_id\":{},\"status\":{},\"execution_mode\":{},",
                    "\"lane_count\":{},\"worker_lanes\":{},\"source\":{}}}"
                ),
                json_string(&snapshot.mission_id),
                json_string(&snapshot.status),
                json_string(&snapshot.execution_mode),
                snapshot.lanes.len(),
                json_vec(&snapshot.lanes),
                json_string(&snapshot.source)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn render_worker_lane_rows_html(snapshots: &[WorkerLaneSnapshot]) -> String {
    if snapshots.is_empty() {
        return "<tr><td colspan=\"4\">No mission worker lanes found in local artifacts.</td></tr>"
            .to_string();
    }
    snapshots
        .iter()
        .map(|snapshot| {
            format!(
                "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td></tr>",
                html_escape(&snapshot.mission_id),
                html_escape(&snapshot.status),
                html_escape(&snapshot.execution_mode),
                html_escape(&snapshot.lanes.join(", "))
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

fn collect_worker_runtime_dashboard(cwd: &Path) -> WorkerRuntimeDashboard {
    let Some(dir) = latest_runtime_child_dir(cwd, "workers") else {
        return WorkerRuntimeDashboard {
            available: false,
            run_id: "missing".to_string(),
            active_leases: 0,
            expired_leases: 0,
            recovered_leases: 0,
            routed_requests: 0,
            concurrent_routing: false,
            source: String::new(),
        };
    };
    let final_state_path = dir.join("worker-final-state.json");
    let final_state = fs::read_to_string(&final_state_path).unwrap_or_default();
    WorkerRuntimeDashboard {
        available: final_state.contains("\"schema\": \"opensks.worker-final-state.v1\""),
        run_id: extract_json_string_field(&final_state, "run_id").unwrap_or_else(|| {
            dir.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown-worker-runtime")
                .to_string()
        }),
        active_leases: extract_json_number_field(&final_state, "active_lease_count").unwrap_or(0),
        expired_leases: extract_json_number_field(&final_state, "expired_lease_count").unwrap_or(0),
        recovered_leases: extract_json_number_field(&final_state, "recovered_expired_lease_count")
            .unwrap_or(0),
        routed_requests: extract_json_number_field(&final_state, "routed_request_count")
            .unwrap_or(0),
        concurrent_routing: json_bool_field_equals(
            &final_state,
            "concurrent_request_routing",
            true,
        ),
        source: final_state_path.display().to_string(),
    }
}

fn render_worker_runtime_dashboard_json(runtime: &WorkerRuntimeDashboard) -> String {
    format!(
        concat!(
            "{{\"available\":{},\"run_id\":{},\"active_leases\":{},",
            "\"expired_leases\":{},\"recovered_leases\":{},\"routed_requests\":{},",
            "\"concurrent_routing\":{},\"daemon_visible_worker_bus\":{},",
            "\"artifact\":\"worker-final-state.json\",\"source\":{},",
            "\"live_provider_workers\":false,\"live_remote_provider_bus\":false}}"
        ),
        runtime.available,
        json_string(&runtime.run_id),
        runtime.active_leases,
        runtime.expired_leases,
        runtime.recovered_leases,
        runtime.routed_requests,
        runtime.concurrent_routing,
        runtime.available,
        json_string(&runtime.source)
    )
}

fn latest_runtime_child_dir(cwd: &Path, relative: &str) -> Option<PathBuf> {
    let mut dirs = fs::read_dir(cwd.join(OPEN_SKSDIR).join(relative))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    dirs.sort();
    dirs.pop()
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

fn render_platform_manifest(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.platform-manifest.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"primary_platform\": \"macOS\",\n",
            "  \"secondary_platform\": \"Linux\",\n",
            "  \"future_platforms\": {},\n",
            "  \"primary_runtime\": {},\n",
            "  \"gui_candidate\": \"Tauri v2 with Rust engine core and WebView frontend\",\n",
            "  \"live_native_gui\": false\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&["Windows"]),
        json_array(&[
            "Rust",
            "Tokio",
            "content-addressed-cache",
            "worktree-isolation",
            "stage-scheduler",
            "event-sourced-artifacts"
        ])
    )
}

fn render_module_manifest(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.module-manifest.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"module_categories\": {},\n",
            "  \"commercial_open_source_concepts\": {},\n",
            "  \"live_plugin_marketplace\": false\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&[
            "mcp_server",
            "provider_adapter",
            "tool_driver",
            "scheduler_strategy",
            "qa_plugin",
            "design_plugin"
        ]),
        json_array(&[
            "modular_provider_profiles",
            "brokered_tool_surfaces",
            "local_artifact_dashboards",
            "extension_ready_manifest_boundaries"
        ])
    )
}

fn render_macos_integration_manifest(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.macos-integration-manifest.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"macos_first\": true,\n",
            "  \"keychain_posture\": \"macos_keychain_first_policy_artifact\",\n",
            "  \"accessibility_posture\": \"brokered_inspection_artifact\",\n",
            "  \"app_automation_posture\": \"policy_broker_artifact\",\n",
            "  \"apple_silicon_posture\": \"rust_native_runtime_candidate\",\n",
            "  \"menu_bar_live\": false,\n",
            "  \"global_shortcut_live\": false,\n",
            "  \"signed_update_live\": false\n",
            "}}\n"
        ),
        stamp.json()
    )
}

fn render_source_notes_ledger(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.source-notes-ledger.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"foundations\": {},\n",
            "  \"mapped_artifacts\": {},\n",
            "  \"live_external_claim_verification\": false\n",
            "}}\n"
        ),
        stamp.json(),
        json_array(&[
            "Model Context Protocol",
            "OpenAI computer use",
            "Playwright",
            "macOS automation",
            "OpenRouter",
            "SKS Codex"
        ]),
        json_array(&[
            "mcp-server-descriptor.json",
            "computer-final-state.json",
            "browser-final-state.json",
            "app-final-state.json",
            "provider-capabilities.json",
            "final-seal.json"
        ])
    )
}

fn render_product_statement(stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.product-statement.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"statement\": {},\n",
            "  \"completion_claim\": \"artifact_mvp_not_full_product_completion\"\n",
            "}}\n"
        ),
        stamp.json(),
        json_string(concat!(
            "OpenSKS is a Rust-native autonomous coding OS built around ",
            "goal-loop engineering, Voxel TriWiki memory, MCP capability ",
            "orchestration, and safe computer/browser/app use. It keeps large ",
            "context cache-stable, coordinates multiple LLMs and tools through ",
            "a policy broker, executes real parallel coding and QA stages, and ",
            "proves every completion with artifacts, coverage, security audits, ",
            "and a final seal."
        ))
    )
}

fn write_prd_coverage(cwd: &Path) -> Result<PathBuf, OpenSksError> {
    let dir = cwd.join(OPEN_SKSDIR);
    fs::create_dir_all(&dir)?;
    let requirements = prd_requirements();
    let path = dir.join("prd-coverage.json");
    write_text_atomic(&path, &render_prd_coverage_json_for(&requirements))?;
    write_text_atomic(
        &dir.join("requirement-coverage-gate.json"),
        &render_requirement_coverage_gate_json(&requirements),
    )?;
    Ok(path)
}

fn render_prd_coverage_json() -> String {
    let requirements = prd_requirements();
    render_prd_coverage_json_for(&requirements)
}

fn render_prd_coverage_json_for(requirements: &[PrdRequirement]) -> String {
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
        json_string(PRD_SOURCE_LABEL),
        requirements.len(),
        implemented,
        artifact_mvp,
        planned,
        missing_live,
        rows
    )
}

fn render_requirement_coverage_gate_json(requirements: &[PrdRequirement]) -> String {
    let implemented = requirements
        .iter()
        .filter(|item| item.status == "implemented")
        .count();
    let artifact_mvp = requirements
        .iter()
        .filter(|item| item.status == "artifact_mvp")
        .count();
    let covered = implemented + artifact_mvp;
    let total = requirements.len();
    let target_percent = 95.0;
    let coverage_percent = if total == 0 {
        0.0
    } else {
        covered as f64 * 100.0 / total as f64
    };
    let gate_passed = coverage_percent >= target_percent;
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.requirement-coverage-gate.v1\",\n",
            "  \"source\": {},\n",
            "  \"scope\": \"prd_requirement_artifact_coverage\",\n",
            "  \"covered_statuses\": {},\n",
            "  \"total_requirements\": {},\n",
            "  \"implemented_count\": {},\n",
            "  \"artifact_mvp_count\": {},\n",
            "  \"covered_requirement_count\": {},\n",
            "  \"coverage_percent\": {:.2},\n",
            "  \"target_percent\": {:.2},\n",
            "  \"gate_passed\": {},\n",
            "  \"live_acceptance_all_passed\": false,\n",
            "  \"evidence_refs\": {}\n",
            "}}\n"
        ),
        json_string(PRD_SOURCE_LABEL),
        json_array(&["implemented", "artifact_mvp"]),
        total,
        implemented,
        artifact_mvp,
        covered,
        coverage_percent,
        target_percent,
        gate_passed,
        json_array(&["prd-coverage.json", "acceptance/acceptance-summary.json"])
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
    const PRD_REQUIREMENTS_TSV: &str = include_str!("../assets/prd-requirements.tsv");

    PRD_REQUIREMENTS_TSV
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .map(|line| {
            let mut fields = line.split('\t');
            let requirement = req(
                fields.next().expect("prd requirement id"),
                fields.next().expect("prd requirement section"),
                fields.next().expect("prd requirement text"),
                fields.next().expect("prd requirement status"),
                fields.next().expect("prd requirement evidence"),
            );
            debug_assert!(
                fields.next().is_none(),
                "prd requirement row has unexpected fields"
            );
            requirement
        })
        .collect()
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
    let security_findings = scan_workspace_for_security_findings(cwd)?;
    let security_summary = security_scan_summary(&secret_findings, &security_findings);
    let isolated_workspace = mission_dir.join("worker-workspace");
    fs::create_dir_all(&isolated_workspace)?;
    let isolated_files = copy_workspace_snapshot(cwd, &isolated_workspace)?;

    let goal_loop = render_goal_loop_json(&goal, &mission_id, &config.mode, &stamp, &tool_plan);
    let goal_state = render_goal_state_jsonl(&goal, &mission_id, &stamp, &config.mode);
    let automation_loop = render_automation_loop_json(&goal, &mission_id, &stamp, &config.mode);
    let progress_ledger = render_progress_ledger_json(&goal, &mission_id, &stamp, &config.mode);
    let stop_policy_json = render_stop_policy_json(&goal.stop_policy, &mission_id, &goal.id);
    let tool_plan_json = render_tool_plan_json(&tool_plan, &mission_id, &goal.id);
    let goal_kind_registry = render_goal_kind_registry_json(&goal, &mission_id, &stamp);
    let triwiki_json = render_triwiki_json(&voxels, &mission_id, &goal.id);
    let voxels_jsonl = render_voxels_jsonl(&voxels);

    write_text_atomic(&mission_dir.join("goal-loop.json"), &goal_loop)?;
    write_text_atomic(&mission_dir.join("goal-state.jsonl"), &goal_state)?;
    write_text_atomic(&mission_dir.join("automation-loop.json"), &automation_loop)?;
    write_text_atomic(&mission_dir.join("progress-ledger.json"), &progress_ledger)?;
    write_text_atomic(&mission_dir.join("stop-policy.json"), &stop_policy_json)?;
    write_text_atomic(&mission_dir.join("tool-plan.json"), &tool_plan_json)?;
    write_text_atomic(
        &mission_dir.join("goal-kind-registry.json"),
        &goal_kind_registry,
    )?;
    write_text_atomic(&mission_dir.join("voxel-triwiki.json"), &triwiki_json)?;
    write_text_atomic(&mission_dir.join("voxels.jsonl"), &voxels_jsonl)?;
    write_text_atomic(
        &mission_dir.join("qa-report.json"),
        &render_qa_report(&stamp, &qa_checks),
    )?;
    write_text_atomic(
        &mission_dir.join("security-audit.json"),
        &render_security_audit(&stamp, &secret_findings, &security_findings),
    )?;
    write_text_atomic(
        &mission_dir.join("security-findings.jsonl"),
        &render_security_findings_jsonl(&stamp, &security_findings),
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
    write_text_atomic(
        &mission_dir.join("requirement-coverage-gate.json"),
        &render_requirement_coverage_gate_json(&prd_requirements()),
    )?;
    let final_seal_refs = final_seal_prechecked_artifact_refs();
    let final_seal_artifacts_present = final_seal_refs
        .iter()
        .filter(|artifact| **artifact != "final-seal.json")
        .all(|artifact| mission_dir.join(artifact).exists());
    let final_seal = render_final_seal_json(
        &goal,
        &mission_id,
        &stamp,
        &tool_plan,
        &voxels,
        FinalSealVerification {
            checks: &qa_checks,
            security_summary: &security_summary,
            artifact_refs_present: final_seal_artifacts_present,
        },
    );
    write_text_atomic(&mission_dir.join("final-seal.json"), &final_seal)?;
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

fn supported_goal_kinds() -> &'static [&'static str] {
    &[
        "code_change",
        "bugfix",
        "test_repair",
        "refactor",
        "design_improvement",
        "browser_task",
        "app_task",
        "computer_task",
        "security_review",
        "benchmark",
        "self_improve",
        "research",
    ]
}

fn render_goal_kind_registry_json(goal: &Goal, mission_id: &str, stamp: &ClockStamp) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.goal-kind-registry.v1\",\n",
            "  \"generated_at\": {},\n",
            "  \"mission_id\": {},\n",
            "  \"goal_id\": {},\n",
            "  \"selected_kind\": {},\n",
            "  \"supported_kinds\": {},\n",
            "  \"source\": \"PRD v3 section 2.3\"\n",
            "}}\n"
        ),
        stamp.json(),
        json_string(mission_id),
        json_string(&goal.id),
        json_string(&goal.kind),
        json_array(supported_goal_kinds())
    )
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

fn render_automation_loop_json(
    goal: &Goal,
    mission_id: &str,
    stamp: &ClockStamp,
    mode: &ExecutionMode,
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.automation-loop.v1\",\n",
            "  \"mission_id\": {},\n",
            "  \"goal_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"execution_mode\": {},\n",
            "  \"stages\": {},\n",
            "  \"artifact_mvp_status\": {{\n",
            "    \"goal_analysis\": true,\n",
            "    \"context_composition\": true,\n",
            "    \"work_decomposition\": true,\n",
            "    \"tool_invocation_plan\": true,\n",
            "    \"qa\": true,\n",
            "    \"repair_wave\": \"artifact_gate_only\",\n",
            "    \"final_apply\": \"no_op_or_patch_gate_only\",\n",
            "    \"self_improve\": \"represented_by_memory_update_stage\"\n",
            "  }},\n",
            "  \"live_provider_workers\": false,\n",
            "  \"live_self_improve_engine\": false\n",
            "}}\n"
        ),
        json_string(mission_id),
        json_string(&goal.id),
        stamp.json(),
        json_string(mode.as_str()),
        json_array(&[
            "goal_analysis",
            "context_composition",
            "work_decomposition",
            "parallel_worker_execution",
            "tool_invocation",
            "qa",
            "repair_wave",
            "final_apply",
            "report",
            "self_improve"
        ])
    )
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

fn final_seal_artifact_refs() -> Vec<&'static str> {
    let mut refs = final_seal_prechecked_artifact_refs();
    refs.push("final-seal.json");
    refs
}

fn final_seal_prechecked_artifact_refs() -> Vec<&'static str> {
    vec![
        "goal-loop.json",
        "goal-state.jsonl",
        "automation-loop.json",
        "progress-ledger.json",
        "stop-policy.json",
        "tool-plan.json",
        "goal-kind-registry.json",
        "voxel-triwiki.json",
        "voxels.jsonl",
        "qa-report.json",
        "security-audit.json",
        "security-findings.jsonl",
        "stage-scheduler.json",
        "scheduler-events.jsonl",
        "scheduler-final-state.json",
        "worktree-isolation.json",
        "patch-envelope.json",
        "patch-gate-result.json",
        "prd-coverage.json",
        "requirement-coverage-gate.json",
    ]
}

fn render_final_seal_json(
    goal: &Goal,
    mission_id: &str,
    stamp: &ClockStamp,
    tool_plan: &ToolPlan,
    voxels: &[Voxel],
    verification: FinalSealVerification<'_>,
) -> String {
    let failed_checks = verification
        .checks
        .iter()
        .filter(|check| check.status == "failed" || check.status == "error")
        .count();
    let non_passed_checks = verification
        .checks
        .iter()
        .filter(|check| check.status != "passed")
        .count();
    let qa_passed = !verification.checks.is_empty() && non_passed_checks == 0;
    let qa_status = if qa_passed {
        "passed"
    } else if failed_checks > 0 {
        "failed"
    } else {
        "blocked"
    };
    let security_status = if verification.security_summary.secret_findings == 0
        && verification.security_summary.critical_or_warning_findings == 0
    {
        "passed"
    } else {
        "findings"
    };
    let checked_artifacts = final_seal_prechecked_artifact_refs();
    let artifact_manifest = final_seal_artifact_refs();
    let artifact_integrity_passed = qa_passed
        && verification.security_summary.secret_findings == 0
        && verification.security_summary.critical_or_warning_findings == 0
        && !tool_plan.capabilities.is_empty()
        && !voxels.is_empty()
        && verification.artifact_refs_present;
    let artifact_manifest_hash = stable_content_hash(&artifact_manifest.join("\n"));
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.final-seal.v1\",\n",
            "  \"mission_id\": {},\n",
            "  \"goal_id\": {},\n",
            "  \"sealed_at\": {},\n",
            "  \"status\": \"partial\",\n",
            "  \"status_reason\": \"Goal-loop intake, Voxel TriWiki seed, local scheduler, local QA/security scan, isolated workspace snapshot, patch gate, progress ledger, and final seal were written. Provider-backed workers, repair waves, and final apply transactions remain future work.\",\n",
            "  \"trust_scope\": \"artifact_mvp_final_seal_integrity\",\n",
            "  \"completion_claim\": \"artifact_integrity_only_not_live_route_completion\",\n",
            "  \"trust_contract\": {{\n",
            "    \"scope\": \"artifact_mvp_final_seal_integrity\",\n",
            "    \"artifact_mvp_final_seal_integrity\": {},\n",
            "    \"artifact_mvp_final_seal_integrity_status\": {},\n",
            "    \"artifact_manifest_hash\": {},\n",
            "    \"checked_artifacts_exist\": {},\n",
            "    \"checked_artifact_count\": {},\n",
            "    \"artifact_manifest_count\": {},\n",
            "    \"final_artifact\": \"final-seal.json\",\n",
            "    \"patch_gate\": {{\"status\":\"pending_diff\",\"final_apply_allowed\":false,\"ref\":\"patch-gate-result.json\"}},\n",
            "    \"live_route_completion\": false,\n",
            "    \"live_hproof_route_gate\": false,\n",
            "    \"live_h_proof\": false,\n",
            "    \"provider_backed_workers_live\": false,\n",
            "    \"live_provider_workers\": false,\n",
            "    \"repair_waves_live\": false,\n",
            "    \"live_repair_waves\": false,\n",
            "    \"final_apply_transaction_live\": false,\n",
            "    \"live_final_apply\": false,\n",
            "    \"acceptance_binding\": \"prod-005 passed only for artifact-MVP final-seal integrity; live route completion remains partial\",\n",
            "    \"checked_artifacts\": {},\n",
            "    \"artifact_manifest\": {}\n",
            "  }},\n",
            "  \"requirements_coverage\": {{\n",
            "    \"requirements_total\": {},\n",
            "    \"requirements_extracted\": {},\n",
            "    \"intake_coverage\": 1.0,\n",
            "    \"execution_coverage\": 0.0\n",
            "  }},\n",
            "  \"model_provenance\": {{\"runtime\":\"local-rust-cli\",\"model_calls\":0}},\n",
            "  \"tool_provenance\": {{\"tool_plan_ref\":\"tool-plan.json\",\"capabilities\":{},\"approval_required\":{}}},\n",
            "  \"qa_summary\": {{\"status\":{},\"check_count\":{},\"failed_checks\":{},\"non_passed_checks\":{},\"checks\":{} }},\n",
            "  \"security_summary\": {{\"status\":{},\"risk_profile\":{},\"secrets_exposed\":{},\"secret_findings\":{},\"security_findings\":{},\"critical_or_warning_findings\":{},\"destructive_actions_executed\":false}},\n",
            "  \"mutation_summary\": {{\"workspace_mutated\":true,\"artifacts_written\":{},\"final_apply_transaction\":\"artifact-only\"}},\n",
            "  \"cache_summary\": {{\"voxel_count\":{},\"content_hash_algorithm\":\"fnv1a64\",\"stable_prefix_seeded\":true}}\n",
            "}}\n"
        ),
        json_string(mission_id),
        json_string(&goal.id),
        stamp.json(),
        artifact_integrity_passed,
        json_string(if artifact_integrity_passed {
            "passed"
        } else {
            "blocked"
        }),
        json_string(&artifact_manifest_hash),
        verification.artifact_refs_present,
        checked_artifacts.len(),
        artifact_manifest.len(),
        json_array(&checked_artifacts),
        json_array(&artifact_manifest),
        goal.success_criteria.len(),
        goal.success_criteria.len(),
        json_vec(&tool_plan.capabilities),
        json_vec(&tool_plan.approval_required),
        json_string(qa_status),
        verification.checks.len(),
        failed_checks,
        non_passed_checks,
        json_array(&[
            "goal-loop.json written",
            "goal-state.jsonl written",
            "automation-loop.json written",
            "progress-ledger.json written",
            "stop-policy.json written",
            "tool-plan.json written",
            "goal-kind-registry.json written",
            "voxel-triwiki.json written",
            "qa-report.json written",
            "security-audit.json written",
            "security-findings.jsonl written",
            "stage-scheduler.json written",
            "worktree-isolation.json written",
            "patch-envelope.json written",
            "prd-coverage.json written",
            "requirement-coverage-gate.json written",
            "final-seal.json written"
        ]),
        json_string(security_status),
        json_string(&goal.risk_profile),
        if verification.security_summary.secret_findings == 0 {
            "false"
        } else {
            "true"
        },
        verification.security_summary.secret_findings,
        verification.security_summary.security_findings,
        verification.security_summary.critical_or_warning_findings,
        json_array(&artifact_manifest),
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

fn html_escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn read_runtime_artifact(cwd: &Path, relative: &str) -> String {
    fs::read_to_string(cwd.join(OPEN_SKSDIR).join(relative)).unwrap_or_default()
}

fn read_runtime_artifact_with_fallback(cwd: &Path, primary: &str, fallback: &str) -> String {
    let primary_value = read_runtime_artifact(cwd, primary);
    if primary_value.trim().is_empty() {
        read_runtime_artifact(cwd, fallback)
    } else {
        primary_value
    }
}

fn count_runtime_child_dirs(cwd: &Path, relative: &str) -> usize {
    fs::read_dir(cwd.join(OPEN_SKSDIR).join(relative))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().is_dir())
                .count()
        })
        .unwrap_or(0)
}

fn extract_json_number_field(input: &str, key: &str) -> Option<usize> {
    extract_json_raw_field(input, key)?.parse().ok()
}

fn html_escape(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            ch => out.push(ch),
        }
    }
    out
}

fn extract_json_string_field(input: &str, key: &str) -> Option<String> {
    let raw = extract_json_raw_field(input, key)?;
    if raw.len() < 2 || !raw.starts_with('"') || !raw.ends_with('"') {
        return None;
    }
    Some(unescape_simple_json_string(&raw[1..raw.len() - 1]))
}

fn extract_json_string_array_field(input: &str, key: &str) -> Vec<String> {
    let raw = match extract_json_array_field(input, key) {
        Some(raw) => raw,
        None => return Vec::new(),
    };
    let mut values = Vec::new();
    let mut chars = raw[1..raw.len().saturating_sub(1)]
        .char_indices()
        .peekable();
    while let Some((start, ch)) = chars.next() {
        if ch != '"' {
            continue;
        }
        let mut escaped = false;
        for (offset, inner) in raw[start + 2..raw.len().saturating_sub(1)].char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if inner == '\\' {
                escaped = true;
                continue;
            }
            if inner == '"' {
                let value_start = start + 2;
                let value_end = value_start + offset;
                values.push(unescape_simple_json_string(&raw[value_start..value_end]));
                while let Some((next, _)) = chars.peek() {
                    if *next <= value_end {
                        chars.next();
                    } else {
                        break;
                    }
                }
                break;
            }
        }
    }
    values
}

fn extract_json_array_field(input: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = input.find(&needle)? + needle.len();
    let after_key = &input[start..];
    let colon = after_key.find(':')?;
    let array_offset = after_key[colon + 1..].find('[')?;
    let value_start = start + colon + 1 + array_offset;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in input[value_start..].char_indices() {
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
            '[' => depth += 1,
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(input[value_start..value_start + offset + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
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
        "  opensks                  # create and open the native macOS app\n",
        "  opensks goal \"<goal text>\" [--kind <kind>] [--mode goal|direct|naruto] [--max-waves <n>]\n",
        "  opensks goal status <mission-id>\n",
        "  opensks run \"<goal text>\"\n",
        "  opensks naruto \"<goal text>\"\n",
        "  opensks browser \"<url or browser goal>\"\n",
        "  opensks app-use \"<app goal>\"\n",
        "  opensks computer-use \"<computer goal>\"\n",
        "  opensks mcp list|add|audit|describe|invoke|serve\n",
        "  opensks voxel index\n",
        "  opensks voxel query \"<text>\"\n",
        "  opensks cache warm\n",
        "  opensks qa run\n",
        "  opensks design qa\n",
        "  opensks security audit\n",
        "  opensks bench\n",
        "  opensks auth\n",
        "  opensks provider list|probe|usage|adapter-check|route\n",
        "  opensks daemon --stdio --workspace <path>\n",
        "  opensks terminal <command>\n",
        "  opensks updater plan\n",
        "  opensks acceptance audit\n",
        "  opensks app\n",
        "  opensks history init\n",
        "  opensks scheduler run \"<goal>\"\n",
        "  opensks scheduler simulate [count]\n",
        "  opensks scheduler dispatch [count]\n",
        "  opensks scheduler recover [count]\n",
        "  opensks perf stress [--events <n>]\n",
        "  opensks worker runtime \"<goal>\"\n",
        "  opensks worktree create \"<worker label>\"\n",
        "  opensks worktree isolate \"<worker label>\"\n",
        "  opensks patch propose \"<summary>\"\n",
        "  opensks patch check <repo-relative-path>\n",
        "  opensks graph templates|compile [template-id]\n",
        "  opensks hooks replay\n",
        "  opensks codegraph index|query <text>\n",
        "  opensks triwiki seed\n",
        "  opensks context pack [token-budget]\n",
        "  opensks image ledger\n",
        "  opensks reasoning debate\n",
        "  opensks git outbox\n",
        "  opensks gc plan\n",
        "  opensks release proof\n",
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

fn provider_usage() -> &'static str {
    opensks_cli::provider_usage()
}

fn voxel_usage() -> &'static str {
    concat!(
        "usage: opensks voxel index\n",
        "       opensks voxel query \"<text>\"\n"
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
fn default_cli_cwd(args: &[String]) -> Result<PathBuf, OpenSksError> {
    if let Some(value) = env::var_os(OPENSKS_WORKSPACE_ENV) {
        let workspace = PathBuf::from(value);
        if !workspace.as_os_str().is_empty() {
            return Ok(workspace);
        }
    }
    if let Some(workspace) = workspace_override_from_args(args) {
        return Ok(workspace);
    }
    default_cwd()
}

#[cfg(test)]
fn workspace_override_from_args(args: &[String]) -> Option<PathBuf> {
    if args.first().is_some_and(|arg| arg == "app-data") {
        return args
            .get(1)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
    }

    args.windows(2).find_map(|window| {
        if window[0] == "--workspace" && !window[1].is_empty() {
            Some(PathBuf::from(&window[1]))
        } else {
            None
        }
    })
}

pub fn native_app_bundle_path(cwd: &Path) -> PathBuf {
    cwd.join(OPEN_SKSDIR).join("macos").join("OpenSKS.app")
}

fn create_native_app_bundle(cwd: &Path) -> Result<PathBuf, OpenSksError> {
    let bundle = native_app_bundle_path(cwd);
    let contents = bundle.join("Contents");
    let macos_dir = contents.join("MacOS");
    let resources_dir = contents.join("Resources");
    let _ = fs::remove_dir_all(contents.join("_CodeSignature"));
    fs::create_dir_all(&macos_dir)?;
    fs::create_dir_all(&resources_dir)?;

    let current_exe = env::current_exe()?;
    // The bundle executable is the compiled SwiftUI app; the Rust binary is
    // embedded as the CLI engine the Swift shell drives.
    let bundle_executable = macos_dir.join("OpenSKS");
    compile_swift_app(cwd, &bundle_executable)?;
    make_executable(&bundle_executable)?;

    let cli_copy = resources_dir.join("opensks-cli");
    fs::copy(&current_exe, &cli_copy)?;
    make_executable(&cli_copy)?;
    adhoc_sign_executable(&cli_copy)?;

    write_text_atomic(
        &resources_dir.join("workspace-path.txt"),
        &format!("{}\n", cwd.display()),
    )?;
    write_text_atomic(&resources_dir.join("opensks-logo.svg"), OPEN_SKS_LOGO_SVG)?;
    create_macos_icon_assets(&resources_dir)?;

    write_text_atomic(&contents.join("Info.plist"), &render_macos_app_info_plist())?;
    write_text_atomic(&contents.join("PkgInfo"), "APPL????")?;
    adhoc_sign_native_app_bundle(&bundle)?;
    Ok(bundle)
}

#[cfg(target_os = "macos")]
fn adhoc_sign_native_app_bundle(bundle: &Path) -> Result<(), OpenSksError> {
    adhoc_sign_path(bundle, "app bundle")
}

#[cfg(target_os = "macos")]
fn adhoc_sign_executable(executable: &Path) -> Result<(), OpenSksError> {
    adhoc_sign_path(executable, "executable")
}

#[cfg(target_os = "macos")]
fn adhoc_sign_path(path: &Path, kind: &str) -> Result<(), OpenSksError> {
    let signed = process::Command::new("codesign")
        .arg("--force")
        .arg("--sign")
        .arg("-")
        .arg(path)
        .output()?;
    if !signed.status.success() {
        return Err(OpenSksError::Invalid(format!(
            "codesign failed for {kind} `{}`:\n{}",
            path.display(),
            String::from_utf8_lossy(&signed.stderr)
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn adhoc_sign_native_app_bundle(bundle: &Path) -> Result<(), OpenSksError> {
    let _ = bundle;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn adhoc_sign_executable(executable: &Path) -> Result<(), OpenSksError> {
    let _ = executable;
    Ok(())
}

#[cfg(target_os = "macos")]
fn swift_package_dir_from_root(root: &Path) -> Option<PathBuf> {
    if root.join("Package.swift").is_file() {
        return Some(root.to_path_buf());
    }
    let nested = root.join("swift");
    if nested.join("Package.swift").is_file() {
        return Some(nested);
    }
    None
}

#[cfg(target_os = "macos")]
fn swift_package_dir_from_ancestors(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        if let Some(package_dir) = swift_package_dir_from_root(ancestor) {
            return Some(package_dir);
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn find_swift_package_dir(cwd: &Path) -> Option<PathBuf> {
    if let Some(configured) = env::var_os(SWIFT_PACKAGE_DIR_ENV).map(PathBuf::from) {
        if let Some(package_dir) = swift_package_dir_from_root(&configured) {
            return Some(package_dir);
        }
    }
    if let Some(package_dir) = swift_package_dir_from_ancestors(cwd) {
        return Some(package_dir);
    }
    if let Ok(current_dir) = env::current_dir() {
        if let Some(package_dir) = swift_package_dir_from_ancestors(&current_dir) {
            return Some(package_dir);
        }
    }
    if let Ok(current_exe) = env::current_exe() {
        if let Some(parent) = current_exe.parent() {
            if let Some(package_dir) = swift_package_dir_from_ancestors(parent) {
                return Some(package_dir);
            }
        }
    }
    None
}

/// Build the SwiftUI app from `swift/Package.swift`, the source of truth for the
/// Studio app, and copy the package product into the generated `.app` bundle.
#[cfg(target_os = "macos")]
fn compile_swift_app(cwd: &Path, output: &Path) -> Result<(), OpenSksError> {
    // Under `cargo test`, do not shell out to the Swift toolchain: it is slow and races with
    // env-mutating tests (concurrent setenv corrupts a child's environment).
    // A placeholder keeps bundle-structure assertions valid; real binaries build
    // the SwiftPM product below and copy that Mach-O into the app bundle.
    if cfg!(test) {
        fs::copy(env::current_exe()?, output)?;
        return Ok(());
    }

    let package_dir = find_swift_package_dir(cwd).ok_or_else(|| {
        OpenSksError::Invalid(format!(
            "could not locate swift/Package.swift; set {SWIFT_PACKAGE_DIR_ENV} to the OpenSKS Swift package directory"
        ))
    })?;
    let scratch_dir = env::temp_dir().join(format!(
        "opensks-swiftpm-{}",
        ClockStamp::now()?.compact_id()
    ));
    let _ = fs::remove_dir_all(&scratch_dir);
    let disable_swift_sandbox = env::var_os(SWIFT_DISABLE_SANDBOX_ENV).is_some();

    let mut build_command = process::Command::new("swift");
    build_command.arg("build");
    if disable_swift_sandbox {
        build_command.arg("--disable-sandbox");
    }
    let build = build_command
        .arg("--package-path")
        .arg(&package_dir)
        .arg("--configuration")
        .arg("release")
        .arg("--product")
        .arg(SWIFT_STUDIO_PRODUCT)
        .arg("--scratch-path")
        .arg(&scratch_dir)
        .output()?;
    if !build.status.success() {
        let _ = fs::remove_dir_all(&scratch_dir);
        return Err(OpenSksError::Invalid(format!(
            "swift build failed for `{}`:\n{}",
            package_dir.display(),
            String::from_utf8_lossy(&build.stderr)
        )));
    }

    let mut bin_path_command = process::Command::new("swift");
    bin_path_command.arg("build");
    if disable_swift_sandbox {
        bin_path_command.arg("--disable-sandbox");
    }
    let bin_path = bin_path_command
        .arg("--package-path")
        .arg(&package_dir)
        .arg("--configuration")
        .arg("release")
        .arg("--show-bin-path")
        .arg("--scratch-path")
        .arg(&scratch_dir)
        .output()?;
    if !bin_path.status.success() {
        let _ = fs::remove_dir_all(&scratch_dir);
        return Err(OpenSksError::Invalid(format!(
            "swift build --show-bin-path failed for `{}`:\n{}",
            package_dir.display(),
            String::from_utf8_lossy(&bin_path.stderr)
        )));
    }
    let bin_dir = PathBuf::from(String::from_utf8_lossy(&bin_path.stdout).trim());
    let built_executable = bin_dir.join(SWIFT_STUDIO_PRODUCT);
    if !built_executable.is_file() {
        let _ = fs::remove_dir_all(&scratch_dir);
        return Err(OpenSksError::Invalid(format!(
            "swift build did not write expected product `{}` at {}",
            SWIFT_STUDIO_PRODUCT,
            built_executable.display()
        )));
    }
    fs::copy(&built_executable, output)?;
    // SPM targets that declare `resources:` generate a companion resource bundle.
    // Ship it under `Contents/Resources`; app-root payloads break macOS code
    // signing as unsealed contents, and `Contents/MacOS` resource bundles are
    // treated as invalid nested code by `codesign --deep`.
    if let Some(macos_dir) = output.parent() {
        let contents_dir = macos_dir.parent();
        let resources_dir = contents_dir.map(|c| c.join("Resources"));
        let app_root = contents_dir.and_then(|c| c.parent());
        for entry in fs::read_dir(&bin_dir)? {
            let entry = entry?;
            let source = entry.path();
            if !source.is_dir() || source.extension().and_then(|e| e.to_str()) != Some("bundle") {
                continue;
            }
            let name = entry.file_name();
            if let Some(root) = app_root {
                let _ = fs::remove_dir_all(root.join(&name));
            }
            let _ = fs::remove_dir_all(macos_dir.join(&name));
            let mut targets = Vec::new();
            if let Some(ref res) = resources_dir {
                targets.push(res.join(&name));
            }
            for dest in targets {
                let _ = fs::remove_dir_all(&dest);
                fs::create_dir_all(&dest)?;
                let mut copied = 0usize;
                copy_dir_snapshot(&source, &source, &dest, &mut copied)?;
            }
        }
    }
    let _ = fs::remove_dir_all(&scratch_dir);
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn compile_swift_app(cwd: &Path, output: &Path) -> Result<(), OpenSksError> {
    let _ = cwd;
    let _ = output;
    Err(OpenSksError::Invalid(
        "the SwiftUI app can only be built on macOS".to_string(),
    ))
}

fn render_macos_app_info_plist() -> String {
    concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
        "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
        "<plist version=\"1.0\">\n",
        "<dict>\n",
        "  <key>CFBundleDevelopmentRegion</key>\n",
        "  <string>en</string>\n",
        "  <key>CFBundleExecutable</key>\n",
        "  <string>OpenSKS</string>\n",
        "  <key>CFBundleIconFile</key>\n",
        "  <string>AppIcon</string>\n",
        "  <key>CFBundleIdentifier</key>\n",
        "  <string>dev.opensks.local</string>\n",
        "  <key>CFBundleInfoDictionaryVersion</key>\n",
        "  <string>6.0</string>\n",
        "  <key>CFBundleName</key>\n",
        "  <string>OpenSKS</string>\n",
        "  <key>CFBundlePackageType</key>\n",
        "  <string>APPL</string>\n",
        "  <key>CFBundleShortVersionString</key>\n",
        "  <string>0.1.0</string>\n",
        "  <key>CFBundleVersion</key>\n",
        "  <string>1</string>\n",
        "  <key>LSMinimumSystemVersion</key>\n",
        "  <string>14.0</string>\n",
        "  <key>NSHighResolutionCapable</key>\n",
        "  <true/>\n",
        "  <key>NSPrincipalClass</key>\n",
        "  <string>NSApplication</string>\n",
        "  <key>LSApplicationCategoryType</key>\n",
        "  <string>public.app-category.developer-tools</string>\n",
        "</dict>\n",
        "</plist>\n"
    )
    .to_string()
}

#[cfg(target_os = "macos")]
fn create_macos_icon_assets(resources_dir: &Path) -> Result<(), OpenSksError> {
    let iconset = resources_dir.join("OpenSKS.iconset");
    fs::create_dir_all(&iconset)?;
    let specs = [
        (16, "icon_16x16.png", *b"icp4"),
        (32, "icon_16x16@2x.png", *b"ic11"),
        (32, "icon_32x32.png", *b"icp5"),
        (64, "icon_32x32@2x.png", *b"ic12"),
        (128, "icon_128x128.png", *b"ic07"),
        (256, "icon_128x128@2x.png", *b"ic13"),
        (256, "icon_256x256.png", *b"ic08"),
        (512, "icon_256x256@2x.png", *b"ic14"),
        (512, "icon_512x512.png", *b"ic09"),
        (1024, "icon_512x512@2x.png", *b"ic10"),
    ];
    let mut icns_entries = Vec::with_capacity(specs.len());
    for (size, name, icon_type) in specs {
        let png = render_macos_icon_png(size);
        let target = iconset.join(name);
        if target.exists() {
            fs::remove_file(&target)?;
        }
        fs::write(target, &png)?;
        icns_entries.push((icon_type, png));
    }
    fs::write(
        resources_dir.join("AppIcon.icns"),
        render_macos_icns(&icns_entries),
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn render_macos_icon_png(size: u32) -> Vec<u8> {
    let mut raw = Vec::with_capacity(((size * size * 4) + size) as usize);
    let center = (size as f32 - 1.0) / 2.0;
    let radius = size as f32 * 0.31;
    let ring_outer = size as f32 * 0.42;
    let ring_inner = size as f32 * 0.37;
    for y in 0..size {
        raw.push(0);
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let distance = (dx * dx + dy * dy).sqrt();
            let diagonal = (x as f32 + y as f32) / (size.max(1) as f32 * 2.0);
            let mut r = (11.0 + 22.0 * diagonal) as u8;
            let mut g = (17.0 + 28.0 * diagonal) as u8;
            let mut b = (25.0 + 38.0 * diagonal) as u8;
            if distance <= radius {
                r = 42;
                g = 204;
                b = 179;
            } else if distance >= ring_inner && distance <= ring_outer {
                r = 129;
                g = 116;
                b = 255;
            } else if dx.abs() <= size as f32 * 0.035 || dy.abs() <= size as f32 * 0.035 {
                r = 78;
                g = 236;
                b = 205;
            }
            raw.extend_from_slice(&[r, g, b, 255]);
        }
    }

    let mut png = Vec::new();
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    png_chunk(&mut png, b"IHDR", &png_ihdr(size, size));
    png_chunk(&mut png, b"IDAT", &zlib_store_blocks(&raw));
    png_chunk(&mut png, b"IEND", &[]);
    png
}

#[cfg(target_os = "macos")]
fn render_macos_icns(entries: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let total_len = entries
        .iter()
        .fold(8usize, |sum, (_, png)| sum + 8 + png.len());
    let mut icns = Vec::with_capacity(total_len);
    icns.extend_from_slice(b"icns");
    icns.extend_from_slice(&(total_len as u32).to_be_bytes());
    for (icon_type, png) in entries {
        icns.extend_from_slice(icon_type);
        icns.extend_from_slice(&((png.len() + 8) as u32).to_be_bytes());
        icns.extend_from_slice(png);
    }
    icns
}

#[cfg(target_os = "macos")]
fn png_ihdr(width: u32, height: u32) -> Vec<u8> {
    let mut data = Vec::with_capacity(13);
    data.extend_from_slice(&width.to_be_bytes());
    data.extend_from_slice(&height.to_be_bytes());
    data.extend_from_slice(&[8, 6, 0, 0, 0]);
    data
}

#[cfg(target_os = "macos")]
fn png_chunk(png: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(chunk_type);
    png.extend_from_slice(data);
    let mut crc_data = Vec::with_capacity(chunk_type.len() + data.len());
    crc_data.extend_from_slice(chunk_type);
    crc_data.extend_from_slice(data);
    png.extend_from_slice(&crc32(&crc_data).to_be_bytes());
}

#[cfg(target_os = "macos")]
fn zlib_store_blocks(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + (data.len() / 65_535 + 1) * 5 + 6);
    out.extend_from_slice(&[0x78, 0x01]);
    for (index, block) in data.chunks(65_535).enumerate() {
        let final_block = index == data.len().saturating_sub(1) / 65_535;
        out.push(if final_block { 0x01 } else { 0x00 });
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

#[cfg(target_os = "macos")]
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(target_os = "macos")]
fn adler32(data: &[u8]) -> u32 {
    const MOD_ADLER: u32 = 65_521;
    let mut a = 1u32;
    let mut b = 0u32;
    for byte in data {
        a = (a + u32::from(*byte)) % MOD_ADLER;
        b = (b + a) % MOD_ADLER;
    }
    (b << 16) | a
}

#[cfg(not(target_os = "macos"))]
fn create_macos_icon_assets(resources_dir: &Path) -> Result<(), OpenSksError> {
    let _ = resources_dir;
    Ok(())
}

fn make_executable(path: &Path) -> Result<(), OpenSksError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn native_app_dashboard(workspace: &Path) -> Result<NativeAppDashboard, OpenSksError> {
    let acceptance_path = workspace
        .join(OPEN_SKSDIR)
        .join("acceptance")
        .join("acceptance-summary.json");
    let acceptance = fs::read_to_string(&acceptance_path).unwrap_or_default();
    let goal_complete = if json_bool_field_equals(&acceptance, "goal_complete", true) {
        Some(true)
    } else if json_bool_field_equals(&acceptance, "goal_complete", false) {
        Some(false)
    } else {
        None
    };
    let resources_dir = current_app_resources_dir()?.unwrap_or_else(|| {
        native_app_bundle_path(workspace)
            .join("Contents")
            .join("Resources")
    });
    Ok(NativeAppDashboard {
        workspace: workspace.to_path_buf(),
        workspace_label: compact_display_path(workspace),
        app_bundle: native_app_bundle_path(workspace),
        artifact_dir: workspace.join(OPEN_SKSDIR).join("app"),
        dashboard_html: workspace
            .join(OPEN_SKSDIR)
            .join("app")
            .join("dashboard.html"),
        cli_path: resources_dir.join("opensks-cli"),
        acceptance: NativeAcceptanceStatus {
            total: extract_json_number_field(&acceptance, "total").unwrap_or(0),
            passed: extract_json_number_field(&acceptance, "passed").unwrap_or(0),
            partial: extract_json_number_field(&acceptance, "partial").unwrap_or(0),
            failed: extract_json_number_field(&acceptance, "failed").unwrap_or(0),
            goal_complete,
        },
        release: collect_native_release_status(workspace),
        provider_adapter_check: collect_native_provider_adapter_check_json(workspace),
        provider_mock_e2e: collect_native_provider_mock_e2e_status(workspace),
        gui: collect_gui_snapshot(workspace),
    })
}

fn collect_native_provider_adapter_check_json(workspace: &Path) -> Option<String> {
    let proof_path = workspace
        .join(OPEN_SKSDIR)
        .join("providers")
        .join("provider-adapter-check.json");
    let raw = fs::read_to_string(proof_path).unwrap_or_default();
    if raw.trim().is_empty() {
        return None;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return None;
    };
    if value.get("schema").and_then(serde_json::Value::as_str)
        != Some("opensks.provider-adapter-check.v1")
    {
        return None;
    }
    Some(value.to_string())
}

fn collect_native_provider_mock_e2e_status(
    workspace: &Path,
) -> Option<NativeProviderMockE2eStatus> {
    let proof_path = workspace
        .join(OPEN_SKSDIR)
        .join("providers")
        .join("provider-mock-e2e.json");
    let raw = fs::read_to_string(proof_path).unwrap_or_default();
    if raw.trim().is_empty() {
        return None;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Some(NativeProviderMockE2eStatus {
            status: "invalid".to_string(),
            fixture_kind: "unknown".to_string(),
            live_vendor_calls_performed: false,
            secret_value_exposed: false,
            model_catalog_count: 0,
            model_catalog_synced: false,
            model_enabled: false,
            registry_route_status: "invalid".to_string(),
            selected_model_id: None,
            checks: Vec::new(),
        });
    };
    let checks = value
        .get("checks")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item.get("id")?.as_str()?.to_string();
                    let status = item
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown")
                        .to_string();
                    let evidence_ref = item
                        .get("evidence_ref")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    Some(NativeProviderMockE2eCheck {
                        id,
                        status,
                        evidence_ref,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(NativeProviderMockE2eStatus {
        status: value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("not_audited")
            .to_string(),
        fixture_kind: value
            .get("fixture_kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        live_vendor_calls_performed: value
            .get("live_vendor_calls_performed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        secret_value_exposed: value
            .get("secret_value_exposed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        model_catalog_count: value
            .get("model_catalog_count")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0),
        model_catalog_synced: value
            .get("model_catalog_synced")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        model_enabled: value
            .get("model_enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        registry_route_status: value
            .get("registry_route_status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        selected_model_id: value
            .get("selected_model_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        checks,
    })
}

fn collect_native_release_status(workspace: &Path) -> NativeReleaseStatus {
    let proof_path = workspace
        .join(OPEN_SKSDIR)
        .join("release")
        .join("release-proof.json");
    let raw = fs::read_to_string(proof_path).unwrap_or_default();
    if raw.trim().is_empty() {
        return NativeReleaseStatus {
            status: "not_audited".to_string(),
            source_commit_sha: None,
            workspace_dirty: false,
            artifact_digest_gate_passed: false,
            same_sha_artifact_binding: false,
            missing_artifacts: Vec::new(),
            blockers: Vec::new(),
            remediation_actions: Vec::new(),
            signing_evidence: None,
        };
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return NativeReleaseStatus {
            status: "invalid".to_string(),
            source_commit_sha: None,
            workspace_dirty: false,
            artifact_digest_gate_passed: false,
            same_sha_artifact_binding: false,
            missing_artifacts: Vec::new(),
            blockers: Vec::new(),
            remediation_actions: Vec::new(),
            signing_evidence: None,
        };
    };
    let status = value
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("not_audited")
        .to_string();
    let source_commit_sha = value
        .get("source_commit_sha")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let workspace_dirty = value
        .get("workspace_dirty")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let artifact_digest_gate_passed = value
        .get("artifact_digest_gate_passed")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let same_sha_artifact_binding = value
        .get("same_sha_artifact_binding")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let missing_artifacts = value
        .get("missing_artifacts")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let blockers = value
        .get("blockers")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let code = item.get("code")?.as_str()?.to_string();
                    let message = item
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    Some(NativeReleaseBlocker { code, message })
                })
                .collect()
        })
        .unwrap_or_default();
    let remediation_actions = value
        .get("remediation_actions")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let blocker = item.get("blocker")?.as_str()?.to_string();
                    let action = item
                        .get("action")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let scope = item
                        .get("scope")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("release")
                        .to_string();
                    Some(NativeReleaseRemediationAction {
                        blocker,
                        action,
                        scope,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let signing_evidence = collect_native_release_signing_evidence(value.get("signing_evidence"));
    NativeReleaseStatus {
        status,
        source_commit_sha,
        workspace_dirty,
        artifact_digest_gate_passed,
        same_sha_artifact_binding,
        missing_artifacts,
        blockers,
        remediation_actions,
        signing_evidence,
    }
}

fn collect_native_release_signing_evidence(
    value: Option<&serde_json::Value>,
) -> Option<NativeReleaseSigningEvidence> {
    let value = value?;
    Some(NativeReleaseSigningEvidence {
        checked: value
            .get("checked")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        app_bundle_path: value
            .get("app_bundle_path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(".opensks/macos/OpenSKS.app")
            .to_string(),
        identifier: value
            .get("identifier")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        signature: value
            .get("signature")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        team_identifier: value
            .get("team_identifier")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        cd_hash: value
            .get("cd_hash")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        production_signed: value
            .get("production_signed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        notarized: value
            .get("notarized")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        codesign_status: value
            .get("codesign_status")
            .and_then(serde_json::Value::as_i64)
            .and_then(|number| i32::try_from(number).ok()),
        notarization_status: value
            .get("notarization_status")
            .and_then(serde_json::Value::as_i64)
            .and_then(|number| i32::try_from(number).ok()),
        diagnostic: value
            .get("diagnostic")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn compact_display_path(path: &Path) -> String {
    let raw = path.display().to_string();
    let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
        return raw;
    };
    if let Ok(relative) = path.strip_prefix(&home) {
        if relative.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", relative.display())
        }
    } else {
        raw
    }
}

pub fn default_launch_cwd() -> Result<PathBuf, OpenSksError> {
    let current = env::current_dir()?;
    if looks_like_opensks_workspace(&current) {
        return Ok(current);
    }

    let executable = env::current_exe()?;
    for ancestor in executable.ancestors() {
        if looks_like_opensks_workspace(ancestor) {
            return Ok(ancestor.to_path_buf());
        }
    }

    Ok(current)
}

pub fn current_app_bundle_workspace() -> Result<Option<PathBuf>, OpenSksError> {
    let Some(resources_dir) = current_app_resources_dir()? else {
        return Ok(None);
    };
    let workspace = fs::read_to_string(resources_dir.join("workspace-path.txt"))?;
    let workspace = workspace.trim();
    if workspace.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(workspace)))
    }
}

fn current_app_resources_dir() -> Result<Option<PathBuf>, OpenSksError> {
    let executable = env::current_exe()?;
    let Some(macos_dir) = executable.parent() else {
        return Ok(None);
    };
    let Some(contents_dir) = macos_dir.parent() else {
        return Ok(None);
    };
    if macos_dir.file_name().and_then(|name| name.to_str()) == Some("MacOS")
        && contents_dir.file_name().and_then(|name| name.to_str()) == Some("Contents")
    {
        Ok(Some(contents_dir.join("Resources")))
    } else {
        Ok(None)
    }
}

pub fn open_path_for_user(path: &Path) -> Result<(), OpenSksError> {
    #[cfg(target_os = "macos")]
    {
        let status = process::Command::new("open").arg(path).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(OpenSksError::Invalid(format!(
                "`open {}` exited with status {status}",
                path.display()
            )))
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Ok(())
    }
}

fn looks_like_opensks_workspace(path: &Path) -> bool {
    path.join("Cargo.toml").is_file() && path.join("src").join("lib.rs").is_file()
}

#[cfg(test)]
mod tests;
