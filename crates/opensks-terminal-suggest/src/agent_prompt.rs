use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::looks_secret_like;

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
        user_prompt: redact_if_secret_like(user_prompt.into()),
        recent_blocks: recent_blocks.iter().map(redact_block).collect(),
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

fn redact_if_secret_like(value: String) -> String {
    if looks_secret_like(&value) {
        "[redacted: possible secret]".to_string()
    } else {
        value
    }
}

fn redact_block(block: &TerminalCommandBlockContext) -> TerminalCommandBlockContext {
    TerminalCommandBlockContext {
        command: redact_if_secret_like(block.command.clone()),
        exit_code: block.exit_code,
        package: block.package.clone(),
        output_summary: block.output_summary.clone().map(redact_if_secret_like),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secret_like_user_prompt() {
        let envelope = build_agent_prompt_envelope(
            Path::new("/tmp/project"),
            "what is my password for the db?",
            &[],
            &[],
        );
        assert_eq!(envelope.user_prompt, "[redacted: possible secret]");
    }

    #[test]
    fn redacts_secret_like_recent_block_command_and_output() {
        let blocks = [TerminalCommandBlockContext {
            command: "cat .env".to_string(),
            exit_code: Some(0),
            package: Some("opensks-terminal-suggest".to_string()),
            output_summary: Some("cat ~/.ssh/id_rsa".to_string()),
        }];
        let envelope =
            build_agent_prompt_envelope(Path::new("/tmp/project"), "help me debug", &blocks, &[]);
        assert_eq!(envelope.recent_blocks.len(), 1);
        let block = &envelope.recent_blocks[0];
        assert_eq!(block.command, "[redacted: possible secret]");
        assert_eq!(
            block.output_summary.as_deref(),
            Some("[redacted: possible secret]")
        );
        assert_eq!(block.exit_code, Some(0));
        assert_eq!(block.package.as_deref(), Some("opensks-terminal-suggest"));
    }

    #[test]
    fn leaves_non_secret_content_untouched() {
        let blocks = [TerminalCommandBlockContext {
            command: "cargo test".to_string(),
            exit_code: Some(101),
            package: Some("opensks-terminal-suggest".to_string()),
            output_summary: Some("test failed".to_string()),
        }];
        let envelope =
            build_agent_prompt_envelope(Path::new("/tmp/project"), "why did this fail?", &blocks, &[]);
        assert_eq!(envelope.user_prompt, "why did this fail?");
        assert_eq!(envelope.recent_blocks[0].command, "cargo test");
        assert_eq!(
            envelope.recent_blocks[0].output_summary.as_deref(),
            Some("test failed")
        );
    }
}
