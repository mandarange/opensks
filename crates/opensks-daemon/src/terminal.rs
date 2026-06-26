use std::path::{Path, PathBuf};

use crate::{DaemonError, DaemonOptions};

const MCP_TOOLS_REL: &str = ".opensks/runtime/terminal/mcp-tools.json";

pub(crate) fn terminal_request_lines(
    raw_request: &serde_json::Value,
    options: &DaemonOptions,
) -> Result<Option<(String, Vec<String>)>, DaemonError> {
    let Some(kind) = raw_request.get("kind").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    if !matches!(
        kind,
        "terminal_session_start"
            | "terminal_session_input"
            | "terminal_session_resize"
            | "terminal_session_stop"
            | "terminal_suggestion_request"
            | "terminal_agent_turn_start"
    ) {
        return Ok(None);
    }
    let request_id = raw_request
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("terminal-request")
        .to_string();
    ensure_mcp_descriptor(&options.workspace)?;
    let lines = match kind {
        "terminal_session_start" => session_start_lines(&request_id, raw_request, options)?,
        "terminal_session_input" => session_input_lines(&request_id, raw_request, options)?,
        "terminal_session_resize" => session_resize_lines(&request_id, raw_request, options)?,
        "terminal_session_stop" => session_stop_lines(&request_id, raw_request, options)?,
        "terminal_suggestion_request" => suggestion_lines(&request_id, raw_request, options)?,
        "terminal_agent_turn_start" => agent_turn_lines(&request_id, raw_request, options)?,
        _ => Vec::new(),
    };
    Ok(Some((request_id, lines)))
}

pub(crate) fn terminal_engine_request_lines(
    request: &opensks_contracts::EngineRequest,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let raw_request = serde_json::to_value(request)?;
    Ok(terminal_request_lines(&raw_request, options)?
        .map(|(_, lines)| lines)
        .unwrap_or_default())
}

fn session_start_lines(
    request_id: &str,
    raw_request: &serde_json::Value,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let params = terminal_param(raw_request, "terminal_session_start");
    let cwd = params
        .and_then(|value| value.get("cwd"))
        .and_then(serde_json::Value::as_str)
        .map(|value| normalize_cwd(&options.workspace, value))
        .unwrap_or_else(|| options.workspace.clone());
    let session_id = params
        .and_then(|value| value.get("session_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("term-daemon-{}", now_millis()));
    let shell = params
        .and_then(|value| value.get("shell"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("/bin/sh");
    let cols = params
        .and_then(|value| value.get("cols"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(80);
    let rows = params
        .and_then(|value| value.get("rows"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(24);
    let artifact_ref = format!(".opensks/runtime/terminal/sessions/{session_id}/session.json");
    let artifact = options.workspace.join(&artifact_ref);
    let receipt = serde_json::json!({
        "schema": "opensks.terminal-session.v1",
        "request_id": request_id,
        "session_id": session_id,
        "mode": "headless_only",
        "runtime": "runtime_not_yet_connected",
        "persistent": false,
        "cwd_kind": cwd_kind(&options.workspace, &cwd),
        "shell": shell,
        "cols": cols,
        "rows": rows,
        "status": "prepared",
        "truth_note": "persistent terminal registry is not connected in this checkout"
    });
    write_json_pretty(&artifact, &receipt)?;
    Ok(vec![serde_json::to_string(&serde_json::json!({
        "schema": "opensks.terminal-session-started.v1",
        "request_id": request_id,
        "session_id": session_id,
        "status": "prepared",
        "runtime": "not_connected",
        "artifact_ref": artifact_ref
    }))?])
}

fn session_input_lines(
    request_id: &str,
    raw_request: &serde_json::Value,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let params = terminal_param(raw_request, "terminal_session_input");
    let session_id = params
        .and_then(|value| value.get("session_id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let input = params
        .and_then(|value| value.get("input"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    append_history(options, session_id, input, "daemon:terminal-session-input")?;
    Ok(vec![serde_json::to_string(&serde_json::json!({
        "schema": "opensks.terminal-session-input.v1",
        "request_id": request_id,
        "session_id": session_id,
        "status": "not_connected",
        "accepted": false,
        "message": "persistent terminal registry is not connected; input was recorded as a redacted proposal only"
    }))?])
}

fn session_resize_lines(
    request_id: &str,
    raw_request: &serde_json::Value,
    _options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let params = terminal_param(raw_request, "terminal_session_resize");
    let session_id = params
        .and_then(|value| value.get("session_id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    Ok(vec![serde_json::to_string(&serde_json::json!({
        "schema": "opensks.terminal-session-resize.v1",
        "request_id": request_id,
        "session_id": session_id,
        "status": "not_connected",
        "message": "persistent terminal registry is not connected"
    }))?])
}

fn session_stop_lines(
    request_id: &str,
    raw_request: &serde_json::Value,
    _options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let params = terminal_param(raw_request, "terminal_session_stop");
    let session_id = params
        .and_then(|value| value.get("session_id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    Ok(vec![serde_json::to_string(&serde_json::json!({
        "schema": "opensks.terminal-session-stopped.v1",
        "request_id": request_id,
        "session_id": session_id,
        "status": "stopped",
        "runtime": "not_connected"
    }))?])
}

fn suggestion_lines(
    request_id: &str,
    raw_request: &serde_json::Value,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let Some(params) = terminal_param(raw_request, "terminal_suggestion_request") else {
        return terminal_error_lines(request_id, "missing params.terminal_suggestion_request");
    };
    let input = params
        .get("input")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let cwd = params
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .map(|value| normalize_cwd(&options.workspace, value))
        .unwrap_or_else(|| options.workspace.clone());
    let include_ai = params
        .get("include_ai")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let max_suggestions = params
        .get("max_suggestions")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(5) as usize;
    let mut suggestions = suggestions_for(input, include_ai);
    suggestions.truncate(max_suggestions);
    let artifact_ref = format!(".opensks/runtime/terminal/suggestions/{request_id}.json");
    let artifact = options.workspace.join(&artifact_ref);
    let cache = serde_json::json!({
        "schema": "opensks.terminal-suggestion-cache.v1",
        "request_id": request_id,
        "input": input,
        "cwd_kind": cwd_kind(&options.workspace, &cwd),
        "include_ai": include_ai,
        "provider": if include_ai { "not_connected" } else { "not_requested" },
        "suggestions": suggestions.clone()
    });
    write_json_pretty(&artifact, &cache)?;
    let mut lines = Vec::new();
    for suggestion in suggestions {
        let mut suggestion = suggestion;
        suggestion["request_id"] = serde_json::Value::String(request_id.to_string());
        suggestion["artifact_ref"] = serde_json::Value::String(artifact_ref.clone());
        lines.push(serde_json::to_string(&suggestion)?);
    }
    Ok(lines)
}

fn agent_turn_lines(
    request_id: &str,
    raw_request: &serde_json::Value,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let params = terminal_param(raw_request, "terminal_agent_turn_start");
    let prompt = params
        .and_then(|value| value.get("prompt"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            raw_request
                .get("params")
                .and_then(|value| value.get("message"))
                .and_then(serde_json::Value::as_str)
        })
        .unwrap_or("");
    let suggestions = agent_suggestions(prompt);
    let artifact_ref = format!(".opensks/runtime/terminal/agent-turns/{request_id}.json");
    let artifact = options.workspace.join(&artifact_ref);
    let envelope = serde_json::json!({
        "schema": "opensks.terminal-agent-envelope.v1",
        "request_id": request_id,
        "prompt_redacted": prompt,
        "provider": "not_connected",
        "execution": "proposal_only",
        "recent_terminal_blocks_max": 5,
        "suggestions": suggestions.clone()
    });
    write_json_pretty(&artifact, &envelope)?;
    let mut lines = Vec::new();
    for suggestion in suggestions {
        let mut suggestion = suggestion;
        suggestion["request_id"] = serde_json::Value::String(request_id.to_string());
        suggestion["artifact_ref"] = serde_json::Value::String(artifact_ref.clone());
        lines.push(serde_json::to_string(&suggestion)?);
    }
    Ok(lines)
}

fn terminal_error_lines(request_id: &str, message: &str) -> Result<Vec<String>, DaemonError> {
    Ok(vec![serde_json::to_string(&serde_json::json!({
        "schema": "opensks.engine-event.v1",
        "event_id": format!("terminal-error-{request_id}"),
        "request_id": request_id,
        "event_type": "error",
        "severity": "error",
        "message": message,
        "protocol_version": "opensks.contracts.v1",
        "timestamp_ms": now_millis(),
        "evidence_refs": ["daemon:terminal-router"],
        "redacted": true
    }))?])
}

fn terminal_param<'a>(
    raw_request: &'a serde_json::Value,
    name: &str,
) -> Option<&'a serde_json::Value> {
    raw_request
        .get("params")
        .and_then(|params| params.get(name))
}

fn suggestions_for(input: &str, include_ai: bool) -> Vec<serde_json::Value> {
    let lower = input.trim().to_ascii_lowercase();
    let mut suggestions = if lower == "git st" || lower.starts_with("git st ") {
        vec![suggestion(
            "sug-git-status",
            "git status",
            "Complete common Git status shorthand.",
            0.95,
            "terminal:suggestion-request",
        )]
    } else if lower.starts_with("cargo t") || lower.contains("cargo test") {
        vec![
            suggestion(
                "sug-cargo-test-nocapture",
                "cargo test -- --nocapture",
                "Run tests with captured output disabled for diagnosis.",
                0.72,
                "terminal:suggestion-request",
            ),
            suggestion(
                "sug-cargo-check",
                "cargo check",
                "Run a fast Rust compile/type check.",
                0.66,
                "terminal:suggestion-request",
            ),
        ]
    } else {
        vec![suggestion(
            "sug-pwd",
            "pwd",
            "Inspect the current working directory without side effects.",
            0.55,
            "terminal:suggestion-request",
        )]
    };
    if include_ai {
        for suggestion in &mut suggestions {
            suggestion["source"] =
                serde_json::Value::String("fallback_provider_not_connected".to_string());
            if let Some(refs) = suggestion
                .get_mut("evidence_refs")
                .and_then(serde_json::Value::as_array_mut)
            {
                refs.push(serde_json::Value::String(
                    "provider:not-connected".to_string(),
                ));
            }
        }
    }
    suggestions
}

fn agent_suggestions(prompt: &str) -> Vec<serde_json::Value> {
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
    evidence_ref: &str,
) -> serde_json::Value {
    serde_json::json!({
        "schema": "opensks.terminal-suggestion.v1",
        "id": id,
        "replacement": command,
        "display": command,
        "description": description,
        "source": "fallback",
        "confidence": confidence,
        "risk": "safe",
        "requires_approval": false,
        "evidence_refs": [evidence_ref]
    })
}

fn ensure_mcp_descriptor(workspace: &Path) -> Result<(), DaemonError> {
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
    write_json_pretty(&workspace.join(MCP_TOOLS_REL), &descriptor)
}

fn append_history(
    options: &DaemonOptions,
    session_id: &str,
    command: &str,
    source: &str,
) -> Result<(), DaemonError> {
    let path = options
        .workspace
        .join(".opensks")
        .join("runtime")
        .join("terminal")
        .join("history.jsonl");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let record = serde_json::json!({
        "schema": "opensks.terminal-history-block.v1",
        "id": format!("terminal-history-{}", now_millis()),
        "session_id": session_id,
        "command_redacted": command,
        "source": source,
        "redacted": true
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    use std::io::Write;
    writeln!(file, "{}", serde_json::to_string(&record)?)?;
    Ok(())
}

fn write_json_pretty(path: &Path, value: &serde_json::Value) -> Result<(), DaemonError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(value)?;
    std::fs::write(path, [&body[..], b"\n"].concat())?;
    Ok(())
}

fn normalize_cwd(workspace: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    }
}

fn cwd_kind(workspace: &Path, candidate: &Path) -> &'static str {
    if candidate.starts_with(workspace) {
        "workspace"
    } else {
        "external"
    }
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
