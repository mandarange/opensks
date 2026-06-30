use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use opensks_contracts::{
    TERMINAL_SUGGESTION_REQUEST_SCHEMA, TerminalRiskLevel, TerminalSuggestionRequest,
    TerminalSuggestionSource,
};
use opensks_terminal_suggest::{
    TerminalCommandBlockContext, TerminalProviderCommandProposal, TerminalSuggestionContext,
    TerminalSuggestionEngine, TerminalSuggestionEngineConfig,
};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[test]
fn expands_common_aliases() {
    let fixture = Fixture::new();
    let suggestions = fixture.suggest("git st");
    assert_eq!(suggestions[0].replacement, "git status");

    let suggestions = fixture.suggest("cargo t");
    assert_eq!(suggestions[0].replacement, "cargo test");
}

#[test]
fn completes_cargo_workspace_packages() {
    let fixture = Fixture::new();
    let suggestions = fixture.suggest("cargo test -p ");
    assert!(
        suggestions
            .iter()
            .any(|suggestion| suggestion.replacement == "cargo test -p opensks-terminal-suggest")
    );
}

#[test]
fn completes_workspace_paths_with_trailing_slash() {
    let fixture = Fixture::new();
    let suggestions = fixture.suggest("cd crates/op");
    assert!(
        suggestions
            .iter()
            .any(|suggestion| suggestion.replacement == "cd crates/opensks-terminal-suggest/")
    );
}

#[test]
fn recent_cargo_test_failure_adds_diagnostic_candidate() {
    let fixture = Fixture::new();
    let mut context = fixture.context();
    context.recent_failure = Some(TerminalCommandBlockContext {
        command: "cargo test".to_string(),
        exit_code: Some(101),
        package: Some("opensks-terminal-suggest".to_string()),
        output_summary: Some("test failed".to_string()),
    });
    let suggestions = fixture.suggest_with_context("cargo test", context);
    assert!(suggestions.iter().any(|suggestion| {
        suggestion.replacement == "cargo test -p opensks-terminal-suggest -- --nocapture"
    }));
}

#[test]
fn natural_language_is_routed_to_agent_suggestion() {
    let fixture = Fixture::new();
    let suggestions = fixture.suggest("왜 cargo test가 실패해?");
    assert_eq!(suggestions[0].replacement, "/agent 왜 cargo test가 실패해?");
    assert_eq!(
        suggestions[0].source,
        TerminalSuggestionSource::OpenSksContext
    );
}

#[test]
fn secret_path_completion_is_approval_gated() {
    let fixture = Fixture::new();
    let env_fixture = ["OPENAI", "_API_KEY=redacted"].concat();
    fs::write(fixture.root.join(".env"), env_fixture).unwrap();
    let suggestions = fixture.suggest("cat .");
    let secret = suggestions
        .iter()
        .find(|suggestion| suggestion.replacement == "cat .env")
        .expect(".env should be visible only when prefix starts with dot");
    assert_eq!(secret.risk, TerminalRiskLevel::SecretExposure);
    assert!(secret.requires_approval);
}

#[test]
fn secret_history_commands_are_excluded() {
    let fixture = Fixture::new();
    fixture.write_history(&[
        r#"{"command":"cargo test --locked"}"#,
        r#"{"command":"cat .env"}"#,
        "not-json",
    ]);
    let suggestions = fixture.suggest("cat");
    assert!(
        suggestions
            .iter()
            .all(|suggestion| !suggestion.replacement.contains(".env"))
    );
}

#[test]
fn duplicate_replacements_keep_best_source_but_preserve_high_risk() {
    let fixture = Fixture::new();
    let mut context = fixture.context();
    context
        .provider_proposals
        .push(TerminalProviderCommandProposal {
            replacement: "git status".to_string(),
            display: None,
            description: Some("provider duplicate".to_string()),
            confidence: Some(0.99),
            risk: Some(TerminalRiskLevel::Destructive),
            requires_approval: Some(true),
        });
    let mut config = fixture.config();
    config.provider_proposals_enabled = true;
    let engine = TerminalSuggestionEngine::new(config);
    let suggestions = engine
        .suggest(&request_with_ai("git st", None, true), &context)
        .unwrap();
    let status = suggestions
        .iter()
        .find(|suggestion| suggestion.replacement == "git status")
        .unwrap();
    assert_eq!(status.source, TerminalSuggestionSource::Completion);
    assert_eq!(status.risk, TerminalRiskLevel::Destructive);
    assert!(status.requires_approval);
}

#[test]
fn max_suggestions_limit_is_applied() {
    let fixture = Fixture::new();
    let suggestions = fixture.suggest_request(request("cargo", Some(1)));
    assert_eq!(suggestions.len(), 1);
}

#[test]
fn provider_absence_returns_honest_fallback() {
    let fixture = Fixture::new();
    let suggestions = fixture.suggest("zz");
    let fallback = suggestions
        .iter()
        .find(|suggestion| suggestion.source == TerminalSuggestionSource::Fallback)
        .expect("fallback should be returned when provider proposals are absent");
    assert!(
        fallback
            .description
            .contains("Provider-backed AI suggestions are not connected")
    );
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "opensks-terminal-suggest-test-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("crates/opensks-terminal-suggest/src")).unwrap();
        fs::create_dir_all(root.join("crates/opensks-terminal/src")).unwrap();
        fs::create_dir_all(root.join("xtask/src")).unwrap();
        fs::create_dir_all(root.join(".opensks/wiki/records")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            r#"
[workspace]
members = [
  ".",
  "crates/opensks-terminal-suggest",
  "crates/opensks-terminal",
  "xtask",
]

[package]
name = "opensks"
version = "0.1.0"
edition = "2024"
"#,
        )
        .unwrap();
        write_manifest(
            &root.join("crates/opensks-terminal-suggest/Cargo.toml"),
            "opensks-terminal-suggest",
        );
        write_manifest(
            &root.join("crates/opensks-terminal/Cargo.toml"),
            "opensks-terminal",
        );
        write_manifest(&root.join("xtask/Cargo.toml"), "xtask");
        fs::write(
            root.join(".opensks/wiki/records/terminal-command-catalog.md"),
            "- git status\n- cargo test\n- cargo test -p <package>\n",
        )
        .unwrap();
        Self { root }
    }

    fn config(&self) -> TerminalSuggestionEngineConfig {
        let mut config = TerminalSuggestionEngineConfig::new(&self.root);
        config.max_suggestions = 8;
        config
    }

    fn context(&self) -> TerminalSuggestionContext {
        TerminalSuggestionContext::new(&self.root)
    }

    fn suggest(&self, input: &str) -> Vec<opensks_contracts::TerminalSuggestion> {
        self.suggest_request(request(input, None))
    }

    fn suggest_request(
        &self,
        request: TerminalSuggestionRequest,
    ) -> Vec<opensks_contracts::TerminalSuggestion> {
        self.suggest_with_context_request(request, self.context())
    }

    fn suggest_with_context(
        &self,
        input: &str,
        context: TerminalSuggestionContext,
    ) -> Vec<opensks_contracts::TerminalSuggestion> {
        self.suggest_with_context_request(request(input, None), context)
    }

    fn suggest_with_context_request(
        &self,
        request: TerminalSuggestionRequest,
        context: TerminalSuggestionContext,
    ) -> Vec<opensks_contracts::TerminalSuggestion> {
        TerminalSuggestionEngine::new(self.config())
            .suggest(&request, &context)
            .unwrap()
    }

    fn write_history(&self, lines: &[&str]) {
        let dir = self.root.join(".opensks/runtime/terminal/suggestions");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("cache.jsonl"), lines.join("\n")).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn request(input: &str, max_suggestions: Option<usize>) -> TerminalSuggestionRequest {
    request_with_ai(input, max_suggestions, false)
}

fn request_with_ai(
    input: &str,
    max_suggestions: Option<usize>,
    include_ai: bool,
) -> TerminalSuggestionRequest {
    TerminalSuggestionRequest {
        schema: TERMINAL_SUGGESTION_REQUEST_SCHEMA.to_string(),
        request_id: format!("request-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed)),
        cwd: String::new(),
        input: input.to_string(),
        cursor: input.len(),
        shell: None,
        last_exit_code: None,
        max_suggestions: max_suggestions.unwrap_or(8) as u8,
        include_ai,
        context_refs: Vec::new(),
    }
}

fn write_manifest(path: &Path, name: &str) {
    fs::write(
        path,
        format!(
            r#"
[package]
name = "{name}"
version = "0.1.0"
edition = "2024"
"#
        ),
    )
    .unwrap();
}
