pub mod agent_prompt;
pub mod catalog;
pub mod engine;
pub mod history;
pub mod natural_language;
pub mod path_complete;
pub mod project;

pub use agent_prompt::{TerminalAgentPromptEnvelope, TerminalCommandBlockContext};
pub use engine::{
    TerminalProviderCommandProposal, TerminalSuggestionContext, TerminalSuggestionEngine,
    TerminalSuggestionEngineConfig, TerminalSuggestionError,
};
pub use natural_language::TerminalInputIntent;

use opensks_contracts::TerminalRiskLevel;

pub(crate) const SOURCE_AGENT_PROMPT: &str = "agent_prompt";
pub(crate) const SOURCE_ALIAS_CATALOG: &str = "alias_catalog";
pub(crate) const SOURCE_PROJECT_CATALOG: &str = "project_catalog";
pub(crate) const SOURCE_SHELL_PREFIX: &str = "shell_prefix";
pub(crate) const SOURCE_PATH_COMPLETION: &str = "path_completion";
pub(crate) const SOURCE_HISTORY: &str = "history";
pub(crate) const SOURCE_CONTEXT_HINTS: &str = "context_hints";
pub(crate) const SOURCE_PROVIDER: &str = "provider";
pub(crate) const SOURCE_FALLBACK: &str = "fallback";

#[derive(Debug, Clone)]
pub(crate) struct SuggestionCandidate {
    pub replacement: String,
    pub display: String,
    pub description: Option<String>,
    pub source: &'static str,
    pub confidence: f32,
    pub risk: TerminalRiskLevel,
}

impl SuggestionCandidate {
    pub(crate) fn new(
        replacement: impl Into<String>,
        display: impl Into<String>,
        source: &'static str,
        confidence: f32,
    ) -> Self {
        let replacement = replacement.into();
        let risk = classify_command_risk(&replacement);
        Self {
            replacement,
            display: display.into(),
            description: None,
            source,
            confidence: confidence.clamp(0.0, 1.0),
            risk,
        }
    }

    pub(crate) fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub(crate) fn with_risk(mut self, risk: TerminalRiskLevel) -> Self {
        self.risk = risk;
        self
    }
}

pub(crate) fn source_priority(source: &str) -> usize {
    match source {
        SOURCE_AGENT_PROMPT => 0,
        SOURCE_ALIAS_CATALOG => 1,
        SOURCE_PROJECT_CATALOG => 2,
        SOURCE_SHELL_PREFIX => 3,
        SOURCE_PATH_COMPLETION => 4,
        SOURCE_HISTORY => 5,
        SOURCE_CONTEXT_HINTS => 6,
        SOURCE_PROVIDER => 7,
        SOURCE_FALLBACK => 8,
        _ => 9,
    }
}

pub(crate) fn classify_command_risk(command: &str) -> TerminalRiskLevel {
    if looks_secret_like(command) {
        return TerminalRiskLevel::SecretExposure;
    }
    if looks_destructive_like(command) {
        return TerminalRiskLevel::Destructive;
    }
    TerminalRiskLevel::Safe
}

pub(crate) fn max_risk(a: TerminalRiskLevel, b: TerminalRiskLevel) -> TerminalRiskLevel {
    if risk_rank(&a) >= risk_rank(&b) { a } else { b }
}

fn risk_rank(risk: &TerminalRiskLevel) -> u8 {
    match risk {
        TerminalRiskLevel::Safe => 0,
        TerminalRiskLevel::Caution => 1,
        TerminalRiskLevel::Destructive => 2,
        TerminalRiskLevel::Privileged => 2,
        TerminalRiskLevel::NetworkMutation => 2,
        TerminalRiskLevel::SecretExposure => 3,
        TerminalRiskLevel::Unknown => 4,
    }
}

pub(crate) fn looks_secret_like(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    let secret_tokens = [
        ".env",
        "id_rsa",
        "id_dsa",
        "id_ed25519",
        "private_key",
        "private-key",
        "secret",
        "credential",
        "authorization:",
        "bearer ",
        "token",
        "apikey",
        "api_key",
        "password",
        ".key",
        ".pem",
        ".p12",
        ".pfx",
        "known_hosts",
    ];
    secret_tokens.iter().any(|token| lower.contains(token))
}

pub(crate) fn looks_destructive_like(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    lower.contains("rm -rf")
        || lower.contains("rm -fr")
        || lower.contains("git reset --hard")
        || lower.contains("git clean -fd")
        || lower.contains("chmod 777")
        || lower.contains("chown -r")
        || lower.starts_with("sudo ")
        || lower.contains(" mkfs")
        || lower.starts_with("mkfs")
        || lower.contains(" dd if=")
        || lower.starts_with("dd if=")
        || lower.contains("shutdown")
        || lower.contains("reboot")
        || ((lower.contains("curl ") || lower.contains("wget "))
            && (lower.contains("| sh") || lower.contains("| bash")))
}
