use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{CliError, CliOutput, write_text_atomic};

const SMOKE_TEXT: &str = "opensks-terminal-ok";
const MCP_TOOLS_REL: &str = ".opensks/runtime/terminal/mcp-tools.json";
const HISTORY_REL: &str = ".opensks/runtime/terminal/history.jsonl";

#[derive(Debug, Clone)]
struct TerminalSuggestion {
    id: String,
    replacement: String,
    display: String,
    description: String,
    source: String,
    confidence: f64,
    risk: String,
    requires_approval: bool,
    evidence_refs: Vec<String>,
}

pub fn terminal_usage() -> &'static str {
    concat!(
        "usage: opensks terminal <command>\n\n",
        "commands:\n",
        "  smoke                         run a local terminal smoke test\n",
        "  start [--cwd <path>] [--shell <path>] [--cols <n>] [--rows <n>]\n",
        "  suggest --input <text> [--cursor <n>] [--cwd <path>] [--ai]\n",
        "  agent <prompt> [--cwd <path>]\n",
        "  explain --last [--session <id>]\n",
        "  history [--limit <n>]\n"
    )
}

pub fn run_terminal_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(CliOutput {
            stdout: terminal_usage().to_string(),
        });
    }

    match args[0].as_str() {
        "smoke" => run_smoke(&args[1..], cwd),
        "start" => run_start(&args[1..], cwd),
        "suggest" => run_suggest(&args[1..], cwd),
        "agent" => run_agent(&args[1..], cwd),
        "explain" => run_explain(&args[1..], cwd),
        "history" => run_history(&args[1..], cwd),
        other => Err(CliError::Usage(format!(
            "unknown terminal subcommand `{other}`\n\n{}",
            terminal_usage()
        ))),
    }
}

fn run_smoke(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    reject_extra_args(args, "terminal smoke")?;
    ensure_mcp_descriptor(cwd)?;
    let session_id = format!("term-smoke-{}", now_millis()?);
    let session_dir = terminal_sessions_dir(cwd).join(&session_id);
    let artifact = session_dir.join("session.json");
    let command = "printf 'opensks-terminal-ok\\n'";
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let ok = output.status.success() && stdout.lines().any(|line| line == SMOKE_TEXT);
    let status = if ok { "ok" } else { "failed" };
    let artifact_body = serde_json::json!({
        "schema": "opensks.terminal-session.v1",
        "session_id": session_id,
        "mode": "smoke",
        "runtime": "headless_safe_command",
        "persistent": false,
        "command_redacted": command,
        "expected_output": SMOKE_TEXT,
        "observed_output": stdout,
        "stderr_redacted": stderr,
        "status": status,
        "exit_code": output.status.code(),
        "stopped": true
    });
    write_json_pretty(&artifact, &artifact_body)?;
    append_history(cwd, &session_id, command, "terminal:smoke")?;
    if !ok {
        return Err(CliError::Invalid(format!(
            "terminal smoke failed\nsession: {session_id}\nartifact: {}\n",
            relative_terminal_path(&artifact, cwd).display()
        )));
    }
    Ok(CliOutput {
        stdout: format!(
            "terminal smoke ok\nsession: {session_id}\nartifact: {}\n",
            relative_terminal_path(&artifact, cwd).display()
        ),
    })
}

fn run_start(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    ensure_mcp_descriptor(cwd)?;
    let options = parse_start_options(args, cwd)?;
    let session_id = format!("term-headless-{}", now_millis()?);
    let artifact = terminal_sessions_dir(cwd)
        .join(&session_id)
        .join("session.json");
    let artifact_body = serde_json::json!({
        "schema": "opensks.terminal-session.v1",
        "session_id": session_id,
        "mode": "headless_only",
        "runtime": "runtime_not_yet_connected",
        "persistent": false,
        "cwd_kind": cwd_kind(cwd, &options.cwd),
        "shell": options.shell,
        "cols": options.cols,
        "rows": options.rows,
        "status": "prepared",
        "truth_note": "persistent PTY registry is not connected in this checkout; smoke executes a bounded safe command"
    });
    write_json_pretty(&artifact, &artifact_body)?;
    Ok(CliOutput {
        stdout: format!(
            "terminal session prepared\nsession: {session_id}\nruntime: not_connected\nartifact: {}\n",
            relative_terminal_path(&artifact, cwd).display()
        ),
    })
}

fn run_suggest(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    ensure_mcp_descriptor(cwd)?;
    let options = parse_suggest_options(args, cwd)?;
    let suggestions = suggestions_for(&options.input, options.include_ai, "terminal:suggest");
    let artifact = cwd
        .join(".opensks")
        .join("runtime")
        .join("terminal")
        .join("suggestions")
        .join(format!("suggest-{}.json", now_millis()?));
    let artifact_body = serde_json::json!({
        "schema": "opensks.terminal-suggestions.v1",
        "input": options.input,
        "cursor": options.cursor,
        "cwd_kind": cwd_kind(cwd, &options.cwd),
        "include_ai": options.include_ai,
        "provider": if options.include_ai { "not_connected" } else { "not_requested" },
        "suggestions": suggestions_json(&suggestions)
    });
    write_json_pretty(&artifact, &artifact_body)?;
    if options.json {
        return Ok(CliOutput {
            stdout: serde_json::to_string_pretty(&artifact_body).map_err(|error| {
                CliError::Invalid(format!("serialize terminal suggestions: {error}"))
            })? + "\n",
        });
    }
    Ok(CliOutput {
        stdout: render_suggestions_human(&suggestions),
    })
}

fn run_agent(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    ensure_mcp_descriptor(cwd)?;
    let options = parse_agent_options(args, cwd)?;
    let suggestions = agent_suggestions(&options.prompt);
    let turn_id = format!("terminal-agent-{}", now_millis()?);
    let artifact = cwd
        .join(".opensks")
        .join("runtime")
        .join("terminal")
        .join("agent-turns")
        .join(format!("{turn_id}.json"));
    let artifact_body = serde_json::json!({
        "schema": "opensks.terminal-agent-envelope.v1",
        "turn_id": turn_id,
        "prompt_redacted": options.prompt,
        "cwd_kind": cwd_kind(cwd, &options.cwd),
        "provider": "not_connected",
        "execution": "proposal_only",
        "recent_terminal_blocks_max": 5,
        "suggestions": suggestions_json(&suggestions)
    });
    write_json_pretty(&artifact, &artifact_body)?;
    if options.json {
        return Ok(CliOutput {
            stdout: serde_json::to_string_pretty(&artifact_body).map_err(|error| {
                CliError::Invalid(format!("serialize terminal agent turn: {error}"))
            })? + "\n",
        });
    }
    let mut stdout =
        String::from("terminal agent prepared\nprovider: not_connected\nsuggestions:\n");
    for (idx, suggestion) in suggestions.iter().enumerate() {
        stdout.push_str(&format!("{}. {}\n", idx + 1, suggestion.display));
    }
    Ok(CliOutput { stdout })
}

fn run_explain(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    ensure_mcp_descriptor(cwd)?;
    let options = parse_explain_options(args)?;
    let blocks = read_history_blocks(cwd, 1)?;
    let Some(last) = blocks.last() else {
        return Ok(CliOutput {
            stdout: "terminal explain\nstatus: no_terminal_history\n".to_string(),
        });
    };
    Ok(CliOutput {
        stdout: format!(
            "terminal explain\nsession: {}\nsource: {}\nsummary: last recorded OpenSKS terminal command block was `{}`\n",
            options.session.unwrap_or_else(|| {
                last.get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string()
            }),
            last.get("source")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("terminal:history"),
            last.get("command_redacted")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("redacted")
        ),
    })
}

fn run_history(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    ensure_mcp_descriptor(cwd)?;
    let limit = parse_history_limit(args)?;
    let blocks = read_history_blocks(cwd, limit)?;
    if blocks.is_empty() {
        return Ok(CliOutput {
            stdout: "terminal history\nstatus: empty\n".to_string(),
        });
    }
    let mut stdout = String::from("terminal history\n");
    for (idx, block) in blocks.iter().enumerate() {
        stdout.push_str(&format!(
            "{}. {}  {}\n",
            idx + 1,
            block
                .get("command_redacted")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("redacted"),
            block
                .get("source")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("terminal:history")
        ));
    }
    Ok(CliOutput { stdout })
}

struct StartOptions {
    cwd: PathBuf,
    shell: Option<String>,
    cols: u16,
    rows: u16,
}

struct SuggestOptions {
    input: String,
    cursor: usize,
    cwd: PathBuf,
    include_ai: bool,
    json: bool,
}

struct AgentOptions {
    prompt: String,
    cwd: PathBuf,
    json: bool,
}

struct ExplainOptions {
    session: Option<String>,
}

fn parse_start_options(args: &[String], cwd: &Path) -> Result<StartOptions, CliError> {
    let mut options = StartOptions {
        cwd: cwd.to_path_buf(),
        shell: None,
        cols: 80,
        rows: 24,
    };
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--cwd" => {
                let value = flag_value(args, idx, "--cwd", terminal_usage())?;
                options.cwd = normalize_cwd(cwd, value);
                idx += 2;
            }
            "--shell" => {
                options.shell =
                    Some(flag_value(args, idx, "--shell", terminal_usage())?.to_string());
                idx += 2;
            }
            "--cols" => {
                options.cols =
                    parse_u16(flag_value(args, idx, "--cols", terminal_usage())?, "--cols")?;
                idx += 2;
            }
            "--rows" => {
                options.rows =
                    parse_u16(flag_value(args, idx, "--rows", terminal_usage())?, "--rows")?;
                idx += 2;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown terminal start argument `{other}`\n\n{}",
                    terminal_usage()
                )));
            }
        }
    }
    Ok(options)
}

fn parse_suggest_options(args: &[String], cwd: &Path) -> Result<SuggestOptions, CliError> {
    let mut input = None;
    let mut cursor = None;
    let mut request_cwd = cwd.to_path_buf();
    let mut include_ai = false;
    let mut json = false;
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--input" => {
                input = Some(flag_value(args, idx, "--input", terminal_usage())?.to_string());
                idx += 2;
            }
            "--cursor" => {
                cursor = Some(parse_usize(
                    flag_value(args, idx, "--cursor", terminal_usage())?,
                    "--cursor",
                )?);
                idx += 2;
            }
            "--cwd" => {
                request_cwd = normalize_cwd(cwd, flag_value(args, idx, "--cwd", terminal_usage())?);
                idx += 2;
            }
            "--ai" => {
                include_ai = true;
                idx += 1;
            }
            "--json" => {
                json = true;
                idx += 1;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown terminal suggest argument `{other}`\n\n{}",
                    terminal_usage()
                )));
            }
        }
    }
    let input = input.ok_or_else(|| CliError::Usage(terminal_usage().to_string()))?;
    let cursor = cursor.unwrap_or_else(|| input.chars().count());
    Ok(SuggestOptions {
        input,
        cursor,
        cwd: request_cwd,
        include_ai,
        json,
    })
}

fn parse_agent_options(args: &[String], cwd: &Path) -> Result<AgentOptions, CliError> {
    let mut prompt_parts = Vec::new();
    let mut request_cwd = cwd.to_path_buf();
    let mut json = false;
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--cwd" => {
                request_cwd = normalize_cwd(cwd, flag_value(args, idx, "--cwd", terminal_usage())?);
                idx += 2;
            }
            "--json" => {
                json = true;
                idx += 1;
            }
            value if value.starts_with("--") => {
                return Err(CliError::Usage(format!(
                    "unknown terminal agent argument `{value}`\n\n{}",
                    terminal_usage()
                )));
            }
            value => {
                prompt_parts.push(value.to_string());
                idx += 1;
            }
        }
    }
    if prompt_parts.is_empty() {
        return Err(CliError::Usage(terminal_usage().to_string()));
    }
    Ok(AgentOptions {
        prompt: prompt_parts.join(" "),
        cwd: request_cwd,
        json,
    })
}

fn parse_explain_options(args: &[String]) -> Result<ExplainOptions, CliError> {
    let mut last = false;
    let mut session = None;
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--last" => {
                last = true;
                idx += 1;
            }
            "--session" => {
                session = Some(flag_value(args, idx, "--session", terminal_usage())?.to_string());
                idx += 2;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown terminal explain argument `{other}`\n\n{}",
                    terminal_usage()
                )));
            }
        }
    }
    if !last && session.is_none() {
        return Err(CliError::Usage(terminal_usage().to_string()));
    }
    Ok(ExplainOptions { session })
}

fn parse_history_limit(args: &[String]) -> Result<usize, CliError> {
    let mut limit = 20;
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--limit" => {
                limit = parse_usize(
                    flag_value(args, idx, "--limit", terminal_usage())?,
                    "--limit",
                )?;
                idx += 2;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown terminal history argument `{other}`\n\n{}",
                    terminal_usage()
                )));
            }
        }
    }
    Ok(limit)
}

fn suggestions_for(
    input: &str,
    include_ai: bool,
    evidence_prefix: &str,
) -> Vec<TerminalSuggestion> {
    let normalized = input.trim();
    let lower = normalized.to_ascii_lowercase();
    let mut suggestions = if lower == "git st" || lower.starts_with("git st ") {
        vec![suggestion(
            "sug-git-status",
            "git status",
            "Complete common Git status shorthand.",
            0.95,
            evidence_prefix,
        )]
    } else if lower.starts_with("cargo t") || lower.contains("cargo test") {
        vec![
            suggestion(
                "sug-cargo-test-nocapture",
                "cargo test -- --nocapture",
                "Run Rust tests with captured output disabled for diagnosis.",
                0.72,
                evidence_prefix,
            ),
            suggestion(
                "sug-cargo-check",
                "cargo check",
                "Run a fast Rust compile/type check.",
                0.66,
                evidence_prefix,
            ),
        ]
    } else if lower.starts_with("git ") {
        vec![suggestion(
            "sug-git-status",
            "git status",
            "Inspect repository state before choosing a Git action.",
            0.62,
            evidence_prefix,
        )]
    } else {
        vec![suggestion(
            "sug-pwd",
            "pwd",
            "Inspect the current working directory without side effects.",
            0.55,
            evidence_prefix,
        )]
    };
    if include_ai {
        for suggestion in &mut suggestions {
            suggestion.source = "fallback_provider_not_connected".to_string();
            suggestion
                .evidence_refs
                .push("provider:not-connected".to_string());
        }
    }
    suggestions
}

fn agent_suggestions(prompt: &str) -> Vec<TerminalSuggestion> {
    let lower = prompt.to_ascii_lowercase();
    if lower.contains("cargo test") || lower.contains("test") || lower.contains("실패") {
        vec![
            suggestion(
                "sug-agent-cargo-test-nocapture",
                "cargo test -- --nocapture",
                "Run tests with captured output disabled for diagnosis.",
                0.65,
                "terminal:agent-turn",
            ),
            suggestion(
                "sug-agent-cargo-check",
                "cargo check",
                "Run a fast Rust compile/type check before deeper investigation.",
                0.6,
                "terminal:agent-turn",
            ),
        ]
    } else {
        vec![suggestion(
            "sug-agent-cargo-check",
            "cargo check",
            "Start with a non-mutating Rust compile/type check.",
            0.58,
            "terminal:agent-turn",
        )]
    }
}

fn suggestion(
    id: &str,
    command: &str,
    description: &str,
    confidence: f64,
    evidence_prefix: &str,
) -> TerminalSuggestion {
    TerminalSuggestion {
        id: id.to_string(),
        replacement: command.to_string(),
        display: command.to_string(),
        description: description.to_string(),
        source: "fallback".to_string(),
        confidence,
        risk: "safe".to_string(),
        requires_approval: false,
        evidence_refs: vec![evidence_prefix.to_string()],
    }
}

fn render_suggestions_human(suggestions: &[TerminalSuggestion]) -> String {
    let mut stdout = String::from("suggestions:\n");
    for (idx, suggestion) in suggestions.iter().enumerate() {
        stdout.push_str(&format!(
            "{}. {}     completion   {}   {:.2}\n",
            idx + 1,
            suggestion.display,
            suggestion.risk,
            suggestion.confidence
        ));
    }
    stdout
}

fn suggestions_json(suggestions: &[TerminalSuggestion]) -> serde_json::Value {
    serde_json::Value::Array(
        suggestions
            .iter()
            .map(|suggestion| {
                serde_json::json!({
                    "schema": "opensks.terminal-suggestion.v1",
                    "id": suggestion.id,
                    "replacement": suggestion.replacement,
                    "display": suggestion.display,
                    "description": suggestion.description,
                    "source": suggestion.source,
                    "confidence": suggestion.confidence,
                    "risk": suggestion.risk,
                    "requires_approval": suggestion.requires_approval,
                    "evidence_refs": suggestion.evidence_refs
                })
            })
            .collect(),
    )
}

fn ensure_mcp_descriptor(cwd: &Path) -> Result<(), CliError> {
    let path = cwd.join(MCP_TOOLS_REL);
    let descriptor = serde_json::json!({
        "schema": "opensks.terminal-mcp-tool-descriptor.v1",
        "tools": [
            {"name": "opensks.terminal.start", "risk": "caution"},
            {"name": "opensks.terminal.write", "risk": "caution"},
            {"name": "opensks.terminal.read", "risk": "safe"},
            {"name": "opensks.terminal.resize", "risk": "safe"},
            {"name": "opensks.terminal.stop", "risk": "safe"},
            {"name": "opensks.terminal.suggest", "risk": "safe"},
            {"name": "opensks.terminal.agent", "risk": "caution"},
            {"name": "opensks.terminal.explain", "risk": "safe"}
        ]
    });
    write_json_pretty(&path, &descriptor)
}

fn append_history(
    cwd: &Path,
    session_id: &str,
    command: &str,
    source: &str,
) -> Result<(), CliError> {
    let path = cwd.join(HISTORY_REL);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let record = serde_json::json!({
        "schema": "opensks.terminal-history-block.v1",
        "id": format!("terminal-history-{}", now_millis()?),
        "session_id": session_id,
        "command_redacted": command,
        "source": source,
        "cwd_kind": "workspace",
        "redacted": true
    });
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(
        file,
        "{}",
        serde_json::to_string(&record)
            .map_err(|error| CliError::Invalid(format!("serialize terminal history: {error}")))?
    )?;
    Ok(())
}

fn read_history_blocks(cwd: &Path, limit: usize) -> Result<Vec<serde_json::Value>, CliError> {
    let path = cwd.join(HISTORY_REL);
    let Ok(contents) = fs::read_to_string(path) else {
        return Ok(Vec::new());
    };
    let mut blocks = contents
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>();
    if blocks.len() > limit {
        blocks = blocks.split_off(blocks.len() - limit);
    }
    Ok(blocks)
}

fn terminal_sessions_dir(cwd: &Path) -> PathBuf {
    cwd.join(".opensks")
        .join("runtime")
        .join("terminal")
        .join("sessions")
}

fn write_json_pretty(path: &Path, value: &serde_json::Value) -> Result<(), CliError> {
    let body = serde_json::to_string_pretty(value)
        .map_err(|error| CliError::Invalid(format!("serialize terminal artifact: {error}")))?
        + "\n";
    write_text_atomic(path, &body)
}

fn flag_value<'a>(
    args: &'a [String],
    idx: usize,
    flag: &str,
    usage: &str,
) -> Result<&'a str, CliError> {
    args.get(idx + 1)
        .map(String::as_str)
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| CliError::Usage(format!("flag `{flag}` requires a value\n\n{usage}")))
}

fn parse_u16(value: &str, flag: &str) -> Result<u16, CliError> {
    value.parse::<u16>().map_err(|_| {
        CliError::Usage(format!(
            "flag `{flag}` must be a number\n\n{}",
            terminal_usage()
        ))
    })
}

fn parse_usize(value: &str, flag: &str) -> Result<usize, CliError> {
    value.parse::<usize>().map_err(|_| {
        CliError::Usage(format!(
            "flag `{flag}` must be a number\n\n{}",
            terminal_usage()
        ))
    })
}

fn normalize_cwd(base: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn cwd_kind(workspace: &Path, candidate: &Path) -> &'static str {
    if candidate.starts_with(workspace) {
        "workspace"
    } else {
        "external"
    }
}

fn relative_terminal_path(path: &Path, cwd: &Path) -> PathBuf {
    path.strip_prefix(cwd).unwrap_or(path).to_path_buf()
}

fn reject_extra_args(args: &[String], command: &str) -> Result<(), CliError> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(CliError::Usage(format!(
            "{command} does not accept arguments\n\n{}",
            terminal_usage()
        )))
    }
}

fn now_millis() -> Result<u128, CliError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CliError::Invalid("system clock is before UNIX_EPOCH".to_string()))?
        .as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("workspace");
        root
    }

    #[test]
    fn terminal_help_matches_work_order_shape() {
        let help = run_terminal_command(&["--help".to_string()], Path::new(".")).expect("help");
        assert!(help.stdout.contains("usage: opensks terminal <command>"));
        assert!(help.stdout.contains("smoke"));
        assert!(help.stdout.contains("suggest --input <text>"));
    }

    #[test]
    fn terminal_suggest_completes_git_status() {
        let root = temp_workspace("opensks-cli-terminal-suggest");
        let output = run_terminal_command(
            &[
                "suggest".to_string(),
                "--input".to_string(),
                "git st".to_string(),
                "--cwd".to_string(),
                ".".to_string(),
            ],
            &root,
        )
        .expect("suggest");
        assert!(output.stdout.contains("suggestions:"));
        assert!(output.stdout.contains("git status"));
        assert!(root.join(MCP_TOOLS_REL).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_smoke_runs_safe_command_and_writes_artifact() {
        let root = temp_workspace("opensks-cli-terminal-smoke");
        let output = run_terminal_command(&["smoke".to_string()], &root).expect("smoke");
        assert!(output.stdout.contains("terminal smoke ok"));
        assert!(
            output
                .stdout
                .contains(".opensks/runtime/terminal/sessions/")
        );
        assert!(root.join(HISTORY_REL).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_history_reads_internal_history_only() {
        let root = temp_workspace("opensks-cli-terminal-history");
        run_terminal_command(&["smoke".to_string()], &root).expect("smoke");
        let output = run_terminal_command(
            &[
                "history".to_string(),
                "--limit".to_string(),
                "20".to_string(),
            ],
            &root,
        )
        .expect("history");
        assert!(output.stdout.contains("terminal history"));
        assert!(output.stdout.contains("printf 'opensks-terminal-ok\\n'"));
        let _ = fs::remove_dir_all(root);
    }
}
