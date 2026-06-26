use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCommandBlockContext {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAgentPromptEnvelope {
    pub schema: String,
    pub cwd_redacted: String,
    pub user_prompt: String,
    pub recent_blocks: Vec<TerminalCommandBlockContext>,
    pub project_hints: Vec<String>,
    pub safety_rules: Vec<String>,
}

pub fn build_agent_prompt_envelope(
    cwd: &Path,
    user_prompt: impl Into<String>,
    recent_blocks: &[TerminalCommandBlockContext],
    project_hints: &[String],
) -> TerminalAgentPromptEnvelope {
    TerminalAgentPromptEnvelope {
        schema: "opensks.terminal-agent-prompt-envelope.v1".to_string(),
        cwd_redacted: redact_cwd(cwd),
        user_prompt: user_prompt.into(),
        recent_blocks: recent_blocks.to_vec(),
        project_hints: project_hints.to_vec(),
        safety_rules: default_safety_rules(),
    }
}

pub fn default_safety_rules() -> Vec<String> {
    [
        "Do not auto-execute commands.",
        "Return command proposals, not hidden actions.",
        "Destructive commands require explicit approval.",
        "Never ask to print secrets or read private key files.",
        "Prefer minimal, reversible commands first.",
        "Prefer diagnostic commands before mutation commands.",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn redact_cwd(cwd: &Path) -> String {
    let cwd = cwd.display().to_string();
    let Some(home) = std::env::var_os("HOME") else {
        return cwd;
    };
    let home = home.to_string_lossy();
    cwd.strip_prefix(home.as_ref())
        .map(|suffix| format!("~{suffix}"))
        .unwrap_or(cwd)
}
