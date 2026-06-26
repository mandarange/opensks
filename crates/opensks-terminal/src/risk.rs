use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use opensks_contracts::{
    TERMINAL_RISK_DECISION_SCHEMA, TerminalExecutionDecision, TerminalRiskDecision,
    TerminalRiskLevel,
};

use crate::redaction::redact_command;

static RISK_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct TerminalRiskPolicy {
    pub workspace: std::path::PathBuf,
}

impl TerminalRiskPolicy {
    pub fn new(workspace: impl AsRef<Path>) -> Self {
        Self {
            workspace: workspace.as_ref().to_path_buf(),
        }
    }

    pub fn classify(&self, command: &str) -> TerminalRiskDecision {
        classify_command_risk_with_workspace(command, &self.workspace)
    }
}

pub fn classify_command_risk(command: &str) -> TerminalRiskDecision {
    let workspace = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    classify_command_risk_with_workspace(command, &workspace)
}

pub fn classify_command_risk_with_workspace(
    command: &str,
    workspace: &Path,
) -> TerminalRiskDecision {
    let trimmed = command.trim();
    let lower = trimmed.to_ascii_lowercase();
    let (risk, decision, reason_code) = if trimmed.is_empty() {
        (
            TerminalRiskLevel::Unknown,
            TerminalExecutionDecision::RequireApproval,
            "empty_or_unknown",
        )
    } else if looks_secret_exposure(&lower) {
        (
            TerminalRiskLevel::SecretExposure,
            TerminalExecutionDecision::RequireApproval,
            "secret_exposure",
        )
    } else if looks_privileged(&lower) {
        (
            TerminalRiskLevel::Privileged,
            TerminalExecutionDecision::RequireApproval,
            "privileged_command",
        )
    } else if looks_destructive(&lower) {
        (
            TerminalRiskLevel::Destructive,
            TerminalExecutionDecision::RequireApproval,
            "destructive_command",
        )
    } else if looks_network_mutation(&lower) {
        (
            TerminalRiskLevel::NetworkMutation,
            TerminalExecutionDecision::RequireApproval,
            "network_mutation",
        )
    } else if looks_caution(&lower) {
        (
            TerminalRiskLevel::Caution,
            TerminalExecutionDecision::Warn,
            "mutation_or_state_change",
        )
    } else if looks_safe(&lower) {
        (
            TerminalRiskLevel::Safe,
            TerminalExecutionDecision::Allow,
            "safe_known_command",
        )
    } else {
        (
            TerminalRiskLevel::Unknown,
            TerminalExecutionDecision::RequireApproval,
            "unknown_command",
        )
    };

    TerminalRiskDecision {
        schema: TERMINAL_RISK_DECISION_SCHEMA.to_string(),
        id: next_risk_id(),
        command_redacted: redact_command(trimmed, workspace),
        risk,
        decision: decision.clone(),
        reason_code: reason_code.to_string(),
        requires_approval: matches!(decision, TerminalExecutionDecision::RequireApproval),
        evidence_refs: Vec::new(),
    }
}

fn looks_safe(command: &str) -> bool {
    let first = command.split_whitespace().next().unwrap_or_default();
    matches!(
        first,
        "ls" | "pwd" | "cat" | "git" | "cargo" | "rg" | "grep" | "sed" | "head" | "tail"
    ) && (command == "git status"
        || command.starts_with("git status")
        || command == "git diff"
        || command.starts_with("git diff")
        || command == "cargo test"
        || command.starts_with("cargo test")
        || command == "cargo check"
        || command.starts_with("cargo check")
        || command == "cargo fmt --check"
        || command.starts_with("cargo fmt --check")
        || command.starts_with("cat ")
        || command == "ls"
        || command.starts_with("ls ")
        || command == "pwd"
        || command.starts_with("rg ")
        || command.starts_with("grep ")
        || command.starts_with("sed -n ")
        || command.starts_with("head ")
        || command.starts_with("tail "))
}

fn looks_caution(command: &str) -> bool {
    command == "cargo fix"
        || command.starts_with("cargo fix ")
        || command == "cargo fmt"
        || command.starts_with("cargo fmt ")
        || command == "npm install"
        || command.starts_with("npm install ")
        || command == "pnpm install"
        || command.starts_with("pnpm install ")
        || command.starts_with("git checkout")
        || command.starts_with("git reset --soft")
}

fn looks_destructive(command: &str) -> bool {
    command.contains("rm -rf")
        || command.contains("rm -fr")
        || command.contains("find . -delete")
        || command.contains("git reset --hard")
        || command.contains("git clean -fdx")
        || command.starts_with("truncate ")
        || command.contains(" truncate ")
}

fn looks_privileged(command: &str) -> bool {
    command.starts_with("sudo ")
        || command == "su"
        || command.starts_with("su ")
        || command.contains("chmod -r 777")
        || command.contains("chmod 777 -r")
        || command.starts_with("chown -r")
        || command.contains(" chown -r")
}

fn looks_secret_exposure(command: &str) -> bool {
    command == "env"
        || command == "printenv"
        || command.starts_with("env ")
        || command.starts_with("printenv ")
        || command.contains("cat .env")
        || command.contains("cat ~/.ssh/id_rsa")
        || command.contains("cat ~/.ssh/id_ed25519")
        || command == "pbpaste"
        || command.starts_with("pbpaste ")
        || command.contains("security find-generic-password")
}

fn looks_network_mutation(command: &str) -> bool {
    command.contains("curl -x post")
        || command.contains("curl -x delete")
        || command.starts_with("http delete")
        || command.contains(" terraform apply")
        || command.starts_with("terraform apply")
        || command.contains(" kubectl apply")
        || command.starts_with("kubectl apply")
        || command.contains(" gh pr merge")
        || command.starts_with("gh pr merge")
        || command == "git push"
        || command.starts_with("git push ")
}

fn next_risk_id() -> String {
    let count = RISK_COUNTER.fetch_add(1, Ordering::Relaxed);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("term-risk-{millis}-{count}")
}
