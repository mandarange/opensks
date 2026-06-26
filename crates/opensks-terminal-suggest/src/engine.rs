use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use opensks_contracts::{
    TERMINAL_SUGGESTION_SCHEMA, TerminalRiskLevel, TerminalSuggestion, TerminalSuggestionRequest,
    TerminalSuggestionSource,
};
use thiserror::Error;

use crate::agent_prompt::TerminalCommandBlockContext;
use crate::natural_language::{TerminalInputIntent, classify_input_intent};
use crate::{
    SOURCE_AGENT_PROMPT, SOURCE_CONTEXT_HINTS, SOURCE_FALLBACK, SOURCE_PROVIDER,
    SuggestionCandidate, catalog, classify_command_risk, history, max_risk, path_complete, project,
    source_priority,
};

#[derive(Debug, Clone)]
pub struct TerminalSuggestionEngineConfig {
    pub workspace_root: PathBuf,
    pub catalog_path: Option<PathBuf>,
    pub max_suggestions: usize,
    pub provider_proposals_enabled: bool,
}

impl TerminalSuggestionEngineConfig {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            catalog_path: None,
            max_suggestions: 8,
            provider_proposals_enabled: false,
        }
    }
}

impl Default for TerminalSuggestionEngineConfig {
    fn default() -> Self {
        Self::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }
}

#[derive(Debug, Clone)]
pub struct TerminalSuggestionContext {
    pub cwd: PathBuf,
    pub recent_blocks: Vec<TerminalCommandBlockContext>,
    pub recent_failure: Option<TerminalCommandBlockContext>,
    pub project_hints: Vec<String>,
    pub provider_proposals: Vec<TerminalProviderCommandProposal>,
}

impl TerminalSuggestionContext {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            recent_blocks: Vec::new(),
            recent_failure: None,
            project_hints: Vec::new(),
            provider_proposals: Vec::new(),
        }
    }
}

impl Default for TerminalSuggestionContext {
    fn default() -> Self {
        Self::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }
}

#[derive(Debug, Clone)]
pub struct TerminalProviderCommandProposal {
    pub replacement: String,
    pub display: Option<String>,
    pub description: Option<String>,
    pub confidence: Option<f32>,
    pub risk: Option<TerminalRiskLevel>,
    pub requires_approval: Option<bool>,
}

#[derive(Debug, Error, Clone)]
#[error("{message}")]
pub struct TerminalSuggestionError {
    message: String,
}

impl TerminalSuggestionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TerminalSuggestionEngine {
    config: TerminalSuggestionEngineConfig,
}

impl TerminalSuggestionEngine {
    pub fn new(config: TerminalSuggestionEngineConfig) -> Self {
        Self { config }
    }

    pub fn suggest(
        &self,
        request: &TerminalSuggestionRequest,
        context: &TerminalSuggestionContext,
    ) -> Result<Vec<TerminalSuggestion>, TerminalSuggestionError> {
        let input = input_at_cursor(&request.input, request.cursor);
        let cwd = if request.cwd.trim().is_empty() {
            context.cwd.clone()
        } else {
            PathBuf::from(&request.cwd)
        };
        let max_suggestions = usize::from(request.normalized_max_suggestions())
            .min(self.config.max_suggestions.max(1));
        let intent = self.classify_input_intent(input);
        let mut candidates = Vec::new();

        match intent {
            TerminalInputIntent::Empty => {}
            TerminalInputIntent::AgentPrompt => {
                candidates.push(agent_prompt_candidate(input));
            }
            TerminalInputIntent::ShellCommand | TerminalInputIntent::Ambiguous => {
                candidates.extend(catalog::catalog_suggestions(
                    &self.config.workspace_root,
                    self.config.catalog_path.as_deref(),
                    input,
                ));
                candidates.extend(project::project_suggestions(
                    &self.config.workspace_root,
                    input,
                ));
                candidates.extend(catalog::shell_prefix_suggestions(input));
                candidates.extend(path_complete::path_suggestions(
                    &self.config.workspace_root,
                    &cwd,
                    input,
                ));
                candidates.extend(history::history_suggestions(
                    &self.config.workspace_root,
                    input,
                ));
                candidates.extend(context_hint_suggestions(context, input));
                candidates.extend(recent_failure_suggestions(context, input));
            }
        }

        candidates.extend(provider_candidates(
            self.config.provider_proposals_enabled && request.include_ai,
            &context.provider_proposals,
            input,
        ));
        Ok(dedupe_and_sort(candidates, max_suggestions))
    }

    pub fn classify_input_intent(&self, input: &str) -> TerminalInputIntent {
        classify_input_intent(input)
    }
}

fn input_at_cursor(input: &str, cursor: usize) -> &str {
    let mut cursor = cursor.min(input.len());
    while cursor > 0 && !input.is_char_boundary(cursor) {
        cursor -= 1;
    }
    &input[..cursor]
}

fn agent_prompt_candidate(input: &str) -> SuggestionCandidate {
    let prompt = input
        .trim()
        .strip_prefix("/agent")
        .map(str::trim)
        .unwrap_or_else(|| input.trim());
    let replacement = format!("/agent {prompt}");
    SuggestionCandidate::new(replacement.clone(), replacement, SOURCE_AGENT_PROMPT, 0.95)
        .with_description("Route natural-language input to the OpenSKS agent")
}

fn context_hint_suggestions(
    context: &TerminalSuggestionContext,
    input: &str,
) -> Vec<SuggestionCandidate> {
    context
        .project_hints
        .iter()
        .filter(|hint| hint.starts_with(input) && hint.as_str() != input)
        .map(|hint| {
            SuggestionCandidate::new(hint.clone(), hint.clone(), SOURCE_CONTEXT_HINTS, 0.70)
                .with_description("OpenSKS context hint")
        })
        .collect()
}

fn recent_failure_suggestions(
    context: &TerminalSuggestionContext,
    input: &str,
) -> Vec<SuggestionCandidate> {
    let Some(block) = &context.recent_failure else {
        return Vec::new();
    };
    if !block.command.starts_with("cargo test") {
        return Vec::new();
    }
    if !input.is_empty() && !input.starts_with("cargo test") && !input.starts_with("cargo t") {
        return Vec::new();
    }
    let replacement = block
        .package
        .as_ref()
        .map(|package| format!("cargo test -p {package} -- --nocapture"))
        .unwrap_or_else(|| "cargo test -- --nocapture".to_string());
    vec![
        SuggestionCandidate::new(replacement.clone(), replacement, SOURCE_CONTEXT_HINTS, 0.70)
            .with_description("Diagnostic follow-up for recent cargo test failure"),
    ]
}

fn provider_candidates(
    provider_enabled: bool,
    proposals: &[TerminalProviderCommandProposal],
    input: &str,
) -> Vec<SuggestionCandidate> {
    if provider_enabled && !proposals.is_empty() {
        return proposals
            .iter()
            .map(|proposal| {
                let display = proposal
                    .display
                    .clone()
                    .unwrap_or_else(|| proposal.replacement.clone());
                let risk = proposal
                    .risk
                    .clone()
                    .unwrap_or_else(|| classify_command_risk(&proposal.replacement));
                let mut candidate = SuggestionCandidate::new(
                    proposal.replacement.clone(),
                    display,
                    SOURCE_PROVIDER,
                    proposal.confidence.unwrap_or(0.65).clamp(0.0, 1.0),
                )
                .with_risk(risk);
                if proposal.requires_approval == Some(true)
                    && !candidate.risk.requires_approval_by_default()
                {
                    candidate.risk = TerminalRiskLevel::Unknown;
                }
                if let Some(description) = &proposal.description {
                    candidate.description = Some(description.clone());
                }
                candidate
            })
            .collect();
    }

    vec![
        SuggestionCandidate::new(
            input.to_string(),
            "Deterministic suggestions only",
            SOURCE_FALLBACK,
            0.30,
        )
        .with_risk(TerminalRiskLevel::Unknown)
        .with_description(
            "Provider-backed AI suggestions are not connected; deterministic suggestions only.",
        ),
    ]
}

fn dedupe_and_sort(
    candidates: Vec<SuggestionCandidate>,
    max_suggestions: usize,
) -> Vec<TerminalSuggestion> {
    let mut by_replacement: BTreeMap<String, SuggestionCandidate> = BTreeMap::new();
    for mut candidate in candidates {
        candidate.risk = max_risk(
            candidate.risk.clone(),
            classify_command_risk(&candidate.replacement),
        );
        match by_replacement.remove(&candidate.replacement) {
            None => {
                by_replacement.insert(candidate.replacement.clone(), candidate);
            }
            Some(existing) => {
                let merged_risk = max_risk(existing.risk.clone(), candidate.risk.clone());
                let keep_new = source_priority(candidate.source) < source_priority(existing.source)
                    || (source_priority(candidate.source) == source_priority(existing.source)
                        && candidate.confidence > existing.confidence);
                let mut chosen = if keep_new { candidate } else { existing };
                chosen.risk = merged_risk;
                by_replacement.insert(chosen.replacement.clone(), chosen);
            }
        }
    }

    let mut candidates: Vec<_> = by_replacement.into_values().collect();
    candidates.sort_by(|left, right| {
        source_priority(left.source)
            .cmp(&source_priority(right.source))
            .then_with(|| right.confidence.total_cmp(&left.confidence))
            .then_with(|| left.replacement.cmp(&right.replacement))
    });
    candidates
        .into_iter()
        .take(max_suggestions)
        .enumerate()
        .map(|(index, candidate)| contract_suggestion(index, candidate))
        .collect()
}

fn contract_suggestion(index: usize, candidate: SuggestionCandidate) -> TerminalSuggestion {
    let risk = candidate.risk.clone();
    let requires_approval = risk.requires_approval_by_default();
    TerminalSuggestion {
        schema: TERMINAL_SUGGESTION_SCHEMA.to_string(),
        id: format!("terminal-suggestion-{}", index + 1),
        replacement: candidate.replacement,
        display: candidate.display,
        description: candidate.description.unwrap_or_default(),
        source: contract_source(candidate.source),
        confidence: candidate.confidence,
        risk,
        requires_approval,
        evidence_refs: Vec::new(),
    }
}

fn contract_source(source: &str) -> TerminalSuggestionSource {
    match source {
        crate::SOURCE_HISTORY => TerminalSuggestionSource::ShellHistory,
        crate::SOURCE_PROJECT_CATALOG => TerminalSuggestionSource::ProjectCatalog,
        crate::SOURCE_PATH_COMPLETION => TerminalSuggestionSource::FilePath,
        crate::SOURCE_CONTEXT_HINTS | crate::SOURCE_AGENT_PROMPT => {
            TerminalSuggestionSource::OpenSksContext
        }
        crate::SOURCE_PROVIDER => TerminalSuggestionSource::Provider,
        crate::SOURCE_FALLBACK => TerminalSuggestionSource::Fallback,
        crate::SOURCE_ALIAS_CATALOG | crate::SOURCE_SHELL_PREFIX => {
            TerminalSuggestionSource::Completion
        }
        _ => TerminalSuggestionSource::Unknown,
    }
}

#[allow(dead_code)]
fn _path_is_workspace_child(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}
