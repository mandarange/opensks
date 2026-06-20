use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

const OPEN_SKSDIR: &str = ".opensks";
const DEFAULT_MAX_WAVES: u32 = 3;
const DEFAULT_MAX_WALL_CLOCK_SECONDS: u64 = 60 * 60;
const DEFAULT_MAX_TOKENS: u64 = 200_000;
const DEFAULT_MAX_COST_USD: f64 = 25.0;
const DEFAULT_MAX_TOOL_CALLS: u32 = 100;
const DEFAULT_MAX_NO_PROGRESS: u32 = 2;
const DEFAULT_MAX_REPEATED_OUTPUT: u32 = 2;
const DEFAULT_REQUIRED_COVERAGE_THRESHOLD: f64 = 1.0;

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

    let goal_loop = render_goal_loop_json(&goal, &mission_id, &config.mode, &stamp, &tool_plan);
    let goal_state = render_goal_state_jsonl(&goal, &mission_id, &stamp, &config.mode);
    let progress_ledger = render_progress_ledger_json(&goal, &mission_id, &stamp, &config.mode);
    let stop_policy_json = render_stop_policy_json(&goal.stop_policy, &mission_id, &goal.id);
    let tool_plan_json = render_tool_plan_json(&tool_plan, &mission_id, &goal.id);
    let triwiki_json = render_triwiki_json(&voxels, &mission_id, &goal.id);
    let final_seal = render_final_seal_json(&goal, &mission_id, &stamp, &tool_plan, &voxels);
    let voxels_jsonl = render_voxels_jsonl(&voxels);

    write_text_atomic(&mission_dir.join("goal-loop.json"), &goal_loop)?;
    write_text_atomic(&mission_dir.join("goal-state.jsonl"), &goal_state)?;
    write_text_atomic(&mission_dir.join("progress-ledger.json"), &progress_ledger)?;
    write_text_atomic(&mission_dir.join("stop-policy.json"), &stop_policy_json)?;
    write_text_atomic(&mission_dir.join("tool-plan.json"), &tool_plan_json)?;
    write_text_atomic(&mission_dir.join("voxel-triwiki.json"), &triwiki_json)?;
    write_text_atomic(&mission_dir.join("voxels.jsonl"), &voxels_jsonl)?;
    write_text_atomic(&mission_dir.join("final-seal.json"), &final_seal)?;
    append_text(&triwiki_dir.join("voxels.jsonl"), &voxels_jsonl)?;

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
            "  \"status_reason\": \"MVP creates and seals goal-loop artifacts; execution workers are planned but not yet run.\"\n",
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
            "artifact_write",
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
        "{\"id\":\"progress-002\",\"kind\":\"final_seal_advanced\",\"detail\":\"final seal artifact written\",\"count\":1}".to_string(),
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
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.final-seal.v1\",\n",
            "  \"mission_id\": {},\n",
            "  \"goal_id\": {},\n",
            "  \"sealed_at\": {},\n",
            "  \"status\": \"partial\",\n",
            "  \"status_reason\": \"Goal-loop intake, Voxel TriWiki seed, stop policy, tool plan, progress ledger, and final seal were written. Execution workers and repair waves are planned future work.\",\n",
            "  \"requirements_coverage\": {{\n",
            "    \"requirements_total\": {},\n",
            "    \"requirements_extracted\": {},\n",
            "    \"intake_coverage\": 1.0,\n",
            "    \"execution_coverage\": 0.0\n",
            "  }},\n",
            "  \"model_provenance\": {{\"runtime\":\"local-rust-cli\",\"model_calls\":0}},\n",
            "  \"tool_provenance\": {{\"tool_plan_ref\":\"tool-plan.json\",\"capabilities\":{},\"approval_required\":{}}},\n",
            "  \"qa_summary\": {{\"status\":\"passed\",\"checks\":{} }},\n",
            "  \"security_summary\": {{\"risk_profile\":{},\"secrets_exposed\":false,\"destructive_actions_executed\":false}},\n",
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
        json_array(&[
            "goal-loop.json written",
            "goal-state.jsonl written",
            "progress-ledger.json written",
            "stop-policy.json written",
            "tool-plan.json written",
            "voxel-triwiki.json written",
            "final-seal.json written"
        ]),
        json_string(&goal.risk_profile),
        json_array(&[
            "goal-loop.json",
            "goal-state.jsonl",
            "progress-ledger.json",
            "stop-policy.json",
            "tool-plan.json",
            "voxel-triwiki.json",
            "voxels.jsonl",
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
    let tmp_path = path.with_extension("tmp");
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

fn usage() -> &'static str {
    concat!(
        "OpenSKS\n\n",
        "Usage:\n",
        "  opensks goal \"<goal text>\" [--kind <kind>] [--mode goal|direct|naruto] [--max-waves <n>]\n",
        "  opensks goal status <mission-id>\n",
        "  opensks run \"<goal text>\"\n",
        "  opensks naruto \"<goal text>\"\n\n",
        "The current MVP writes proof-first goal-loop artifacts under .opensks/missions/<mission-id>/.\n"
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
            "final-seal.json",
        ] {
            assert!(
                mission_dir.join(artifact).exists(),
                "expected artifact {artifact}"
            );
        }

        let goal_loop = fs::read_to_string(mission_dir.join("goal-loop.json")).expect("goal loop");
        assert!(goal_loop.contains("\"schema\": \"opensks.goal-loop.v1\""));
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
}
