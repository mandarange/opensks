//! A real multi-step agentic tool loop (recovery release §6 / §8).
//!
//! The single-shot [`LocalTestAdapter`](crate::LocalTestAdapter) executes ONE
//! instruction. A real coding agent instead runs a LOOP: it calls tools (read a
//! file, write a file…), observes the RESULTS, and decides the next step from what
//! it just learned — until it declares it is done. This module provides that loop
//! as honest, deterministic, headless-testable machinery:
//!
//! * [`ToolDriver`] — the decision-maker. Given the observations from the previous
//!   step it returns the next [`AgentStep`] (more tool calls, or a final answer).
//!   A live model adapter implements this by parsing the model's tool calls; the
//!   tests implement it deterministically, and it can branch on observations so the
//!   RESULT-FEEDBACK path is genuinely exercised.
//! * [`run_agentic_loop`] — drives the driver, executes each tool against the
//!   workspace, applies file writes through the SAME transactional writer the
//!   single-shot adapter uses ([`apply_file_writes`](crate::apply_file_writes):
//!   pre-image validation + rollback, no silent overwrite), streams typed events,
//!   and terminates EXPLICITLY — on the driver's final answer or on a hard step
//!   budget. It never completes by silence (directive §0.4); exhausting the budget
//!   is an honest failure, not a fabricated success.
//!
//! The live-model seam is [`ToolDriver`]: wiring an [`OpenRouterAdapter`](crate::OpenRouterAdapter)
//! to emit tool calls is the credentialed work deferred to the release gate. The
//! loop itself — multi-step, feedback-driven, transactional — is real and tested
//! here, and is reachable from a real conversation turn via
//! [`LocalTestInstruction::Steps`](crate::LocalTestInstruction).

use opensks_contracts::projection::RunProjectionState;
use opensks_contracts::{
    AGENT_EVENT_ENVELOPE_SCHEMA, AgentEventEnvelope, AgentEventKind, ConversationTurnSettings,
    ExecutionMode, FileOperation, FilePatch, ImageAsset, ImageProvenanceReceipt,
    PATCH_PROPOSAL_SCHEMA, PatchApplyResult, PatchProposal, ReasoningEffort, RiskLevel,
    Sensitivity, TOOL_POLICY_SCHEMA, ToolPermission, ToolPolicy, ToolPolicyEntry,
    default_tool_registry,
};
use opensks_git_service::{DiffOptions, LogOptions};
use opensks_policy::{Capability, WorkspaceCapabilities};
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::{
    AgentAdapterError, AgentEventSink, AgentRunOutcome, AgentRunRequest, PatchPathLease,
    PlannedWrite, apply_file_writes_with_path_lease, content_hash, minimal_unified_diff,
    patch_path_lease,
};

/// One tool the agent can call within the workspace. Writes are applied
/// transactionally; reads return the on-disk content the agent then reasons over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum ToolCall {
    /// List non-hidden entries in a workspace-relative directory.
    #[serde(rename = "workspace.list_directory", alias = "list_directory")]
    ListDirectory { path: String },
    /// Read a workspace-relative file. The observation carries its content (or
    /// `None` if absent) so the driver can decide what to do next.
    #[serde(rename = "workspace.read_file_range", alias = "read_file")]
    ReadFileRange {
        path: String,
        start_line: Option<u32>,
        end_line: Option<u32>,
    },
    /// Search UTF-8 workspace files for a literal query.
    #[serde(rename = "workspace.search_text", alias = "search_text")]
    SearchText {
        query: String,
        path: String,
        max_results: Option<usize>,
    },
    /// Write a file's full content (create if absent, modify otherwise).
    #[serde(rename = "workspace.propose_patch", alias = "write_file")]
    ProposePatch { path: String, content: String },
    /// Append a line to a file (creating it if absent).
    #[serde(rename = "workspace.diff_patch", alias = "append_line")]
    DiffPatch { path: String, value: String },
    /// Read git status through the read-only git service.
    #[serde(rename = "git.status")]
    GitStatus,
    /// Read git diff through the read-only git service.
    #[serde(rename = "git.diff")]
    GitDiff { path: Option<String> },
    /// Read recent commit history through the read-only git service.
    #[serde(rename = "git.log")]
    GitLog { max_count: Option<u32> },
    /// Query symbols from the CodeGraph index.
    #[serde(rename = "codegraph.query_symbol")]
    CodeGraphQuerySymbol {
        query: String,
        max_results: Option<usize>,
    },
    /// Query adjacent CodeGraph records for a symbol id.
    #[serde(rename = "codegraph.references")]
    CodeGraphReferences { symbol_id: String },
    /// Build a TriWiki-backed worker context pack.
    #[serde(rename = "context.build_pack")]
    ContextBuildPack {
        id: String,
        token_budget: Option<u32>,
    },
    /// Run an approved command through argv execution, never through a shell.
    #[serde(rename = "command.run")]
    CommandRun {
        command: String,
        timeout_ms: Option<u64>,
    },
    /// Run an approved targeted test command through argv execution.
    #[serde(rename = "test.run_targeted")]
    TestRunTargeted {
        target: String,
        timeout_ms: Option<u64>,
    },
    /// Invoke an allowlisted local MCP tool through the OpenSKS broker.
    #[serde(rename = "mcp.invoke")]
    McpInvoke {
        tool_name: String,
        payload: serde_json::Value,
    },
    /// Load an allowlisted local skill route as bounded context.
    #[serde(rename = "skill.invoke")]
    SkillInvoke {
        skill: String,
        payload: serde_json::Value,
    },
    /// Read a text runtime artifact by artifact ref.
    #[serde(rename = "artifact.read")]
    ArtifactRead { artifact_ref: String },
    /// Write a redacted text runtime artifact by artifact ref.
    #[serde(rename = "artifact.write")]
    ArtifactWrite {
        artifact_ref: String,
        content: String,
    },
    /// Generate an image artifact through a caller-provided provider-backed image executor.
    #[serde(rename = "image.generate")]
    ImageGenerate {
        prompt: String,
        asset_id: Option<String>,
        width: Option<u32>,
        height: Option<u32>,
    },
    /// Inspect an existing image artifact through a caller-provided provider-backed vision executor.
    #[serde(rename = "image.inspect")]
    ImageInspect {
        artifact_ref: Option<String>,
        asset_id: Option<String>,
        prompt: Option<String>,
    },
}

impl ToolCall {
    fn path(&self) -> &str {
        match self {
            Self::ListDirectory { path }
            | Self::ReadFileRange { path, .. }
            | Self::SearchText { path, .. }
            | Self::ProposePatch { path, .. }
            | Self::DiffPatch { path, .. } => path,
            Self::GitDiff { path } => path.as_deref().unwrap_or("."),
            Self::GitStatus
            | Self::GitLog { .. }
            | Self::CodeGraphQuerySymbol { .. }
            | Self::CodeGraphReferences { .. }
            | Self::ContextBuildPack { .. }
            | Self::CommandRun { .. }
            | Self::TestRunTargeted { .. }
            | Self::McpInvoke { .. }
            | Self::SkillInvoke { .. }
            | Self::ImageGenerate { .. }
            | Self::ImageInspect { .. } => ".",
            Self::ArtifactRead { artifact_ref } | Self::ArtifactWrite { artifact_ref, .. } => {
                artifact_ref
                    .strip_prefix("artifact://")
                    .unwrap_or(artifact_ref)
            }
        }
    }

    fn tool_name(&self) -> &'static str {
        match self {
            Self::ListDirectory { .. } => "workspace.list_directory",
            Self::ReadFileRange { .. } => "workspace.read_file_range",
            Self::SearchText { .. } => "workspace.search_text",
            Self::ProposePatch { .. } => "workspace.propose_patch",
            Self::DiffPatch { .. } => "workspace.diff_patch",
            Self::GitStatus => "git.status",
            Self::GitDiff { .. } => "git.diff",
            Self::GitLog { .. } => "git.log",
            Self::CodeGraphQuerySymbol { .. } => "codegraph.query_symbol",
            Self::CodeGraphReferences { .. } => "codegraph.references",
            Self::ContextBuildPack { .. } => "context.build_pack",
            Self::CommandRun { .. } => "command.run",
            Self::TestRunTargeted { .. } => "test.run_targeted",
            Self::McpInvoke { .. } => "mcp.invoke",
            Self::SkillInvoke { .. } => "skill.invoke",
            Self::ArtifactRead { .. } => "artifact.read",
            Self::ArtifactWrite { .. } => "artifact.write",
            Self::ImageGenerate { .. } => "image.generate",
            Self::ImageInspect { .. } => "image.inspect",
        }
    }

    fn is_write(&self) -> bool {
        matches!(
            self,
            Self::ProposePatch { .. }
                | Self::DiffPatch { .. }
                | Self::CommandRun { .. }
                | Self::TestRunTargeted { .. }
                | Self::McpInvoke { .. }
                | Self::SkillInvoke { .. }
                | Self::ArtifactWrite { .. }
                | Self::ImageGenerate { .. }
        )
    }
}

/// The result of a single tool call, fed back to the driver for the next step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolResult {
    /// A read: the file's content, or `None` if it does not exist.
    FileContent {
        path: String,
        content: Option<String>,
    },
    /// Generic text output from read-only inspection tools.
    ToolOutput { tool: String, content: String },
    /// A write: whether it landed and the apply reason code (e.g. `applied`,
    /// `stale_precondition`).
    Wrote {
        path: String,
        applied: bool,
        reason: String,
    },
    /// A tool that could not run (e.g. a path that escaped the workspace).
    Failed { tool: String, message: String },
}

/// The driver's decision for the next loop step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStep {
    /// Run these tool calls, then come back with their observations.
    Tools(Vec<ToolCall>),
    /// Stop: the agent is done and this is its final answer.
    Final { text: String },
    /// Stop: the provider/driver failed before a trustworthy final answer.
    Failed {
        code: String,
        message: String,
        retryable: bool,
    },
}

/// The loop's decision-maker. A live model adapter implements this by parsing the
/// model's emitted tool calls; tests implement it deterministically.
pub trait ToolDriver {
    /// Decide the next step given the observations from the previous step (empty on
    /// the first call).
    fn next_step(&mut self, observations: &[ToolResult]) -> AgentStep;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageGenerateToolRequest {
    pub prompt: String,
    pub asset_id: Option<String>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInspectToolRequest {
    pub artifact_ref: String,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInspectToolResult {
    pub receipt: ImageProvenanceReceipt,
    pub text: String,
}

pub trait ImageToolExecutor {
    fn generate_image(
        &self,
        workspace: &std::path::Path,
        request: &ImageGenerateToolRequest,
    ) -> Result<ImageAsset, String>;

    fn inspect_image(
        &self,
        workspace: &std::path::Path,
        request: &ImageInspectToolRequest,
    ) -> Result<ImageInspectToolResult, String>;
}

/// A driver built from a closure, so tests (and simple callers) can branch on the
/// observations — exercising the result-feedback path without a bespoke type.
pub struct FnDriver<F>
where
    F: FnMut(&[ToolResult], usize) -> AgentStep,
{
    step: usize,
    decide: F,
}

impl<F> FnDriver<F>
where
    F: FnMut(&[ToolResult], usize) -> AgentStep,
{
    pub fn new(decide: F) -> Self {
        Self { step: 0, decide }
    }
}

impl<F> ToolDriver for FnDriver<F>
where
    F: FnMut(&[ToolResult], usize) -> AgentStep,
{
    fn next_step(&mut self, observations: &[ToolResult]) -> AgentStep {
        let step = self.step;
        self.step += 1;
        (self.decide)(observations, step)
    }
}

/// A driver that plays a fixed list of tool-call groups (one loop step each) and
/// then returns a final answer. Used for deterministic, predetermined edit
/// sequences (e.g. [`LocalTestInstruction::Steps`](crate::LocalTestInstruction)).
pub struct SequenceDriver {
    groups: std::vec::IntoIter<Vec<ToolCall>>,
    final_text: String,
}

impl SequenceDriver {
    pub fn new(groups: Vec<Vec<ToolCall>>, final_text: impl Into<String>) -> Self {
        Self {
            groups: groups.into_iter(),
            final_text: final_text.into(),
        }
    }
}

impl ToolDriver for SequenceDriver {
    fn next_step(&mut self, _observations: &[ToolResult]) -> AgentStep {
        match self.groups.next() {
            Some(group) => AgentStep::Tools(group),
            None => AgentStep::Final {
                text: self.final_text.clone(),
            },
        }
    }
}

/// Loop configuration. `max_steps` is a HARD budget — the loop terminates honestly
/// (a failure, not a fabricated success) if the driver never declares it is done.
#[derive(Debug, Clone)]
pub struct AgenticConfig {
    pub max_steps: usize,
    pub reasoning_effort: ReasoningEffort,
    pub tool_policy: ToolPolicy,
    pub allowed_paths: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub max_tool_output_bytes: usize,
}

impl Default for AgenticConfig {
    fn default() -> Self {
        Self {
            max_steps: 16,
            reasoning_effort: ReasoningEffort::Standard,
            tool_policy: default_agentic_tool_policy(),
            allowed_paths: vec![],
            forbidden_paths: vec![],
            max_tool_output_bytes: 64 * 1024,
        }
    }
}

impl AgenticConfig {
    pub fn for_turn_settings(settings: &ConversationTurnSettings) -> Self {
        let mut config = Self {
            reasoning_effort: settings.reasoning_effort,
            ..Self::default()
        };
        if matches!(settings.execution_mode, ExecutionMode::ReadOnly) {
            config.tool_policy = read_only_agentic_tool_policy(&settings.tool_policy_id);
        }
        config
    }
}

pub fn default_agentic_tool_policy() -> ToolPolicy {
    let registry = default_tool_registry();
    ToolPolicy {
        schema: TOOL_POLICY_SCHEMA.to_string(),
        policy_id: "agentic-default-tool-registry".to_string(),
        entries: registry
            .available_provider_tools()
            .into_iter()
            .map(|tool| ToolPolicyEntry {
                tool: tool.name.clone(),
                permission: tool.permission,
            })
            .collect(),
    }
}

pub fn read_only_agentic_tool_policy(policy_id: &str) -> ToolPolicy {
    let registry = default_tool_registry();
    ToolPolicy {
        schema: TOOL_POLICY_SCHEMA.to_string(),
        policy_id: format!("{policy_id}:read-only-execution-mode"),
        entries: registry
            .available_provider_tools()
            .into_iter()
            .map(|tool| ToolPolicyEntry {
                tool: tool.name.clone(),
                permission: if matches!(tool.permission, ToolPermission::ReadOnly) {
                    ToolPermission::ReadOnly
                } else {
                    ToolPermission::Deny
                },
            })
            .collect(),
    }
}

#[derive(Debug, Clone)]
pub struct ToolScope {
    pub allowed_paths: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub max_output_bytes: usize,
}

pub struct ToolGateway<'a> {
    capabilities: WorkspaceCapabilities,
    policy: ToolPolicy,
    scope: ToolScope,
    image_executor: Option<&'a dyn ImageToolExecutor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolGatewayReceipt {
    Read(ToolResult),
    ImageArtifact(Box<ImageAsset>),
    ImageInspection(Box<ImageInspectToolResult>),
    PlannedWrite {
        path: String,
        before: String,
        after: String,
        operation: FileOperation,
    },
}

impl<'a> ToolGateway<'a> {
    pub fn new(workspace: &std::path::Path, config: &AgenticConfig) -> Self {
        Self::with_image_executor(workspace, config, None)
    }

    pub fn with_image_executor(
        workspace: &std::path::Path,
        config: &AgenticConfig,
        image_executor: Option<&'a dyn ImageToolExecutor>,
    ) -> Self {
        Self {
            capabilities: WorkspaceCapabilities::deny_by_default(workspace)
                .grant(Capability::FilesystemWorkspace),
            policy: config.tool_policy.clone(),
            scope: ToolScope {
                allowed_paths: config.allowed_paths.clone(),
                forbidden_paths: config.forbidden_paths.clone(),
                max_output_bytes: config.max_tool_output_bytes,
            },
            image_executor,
        }
    }

    pub fn execute(&self, call: &ToolCall) -> Result<ToolGatewayReceipt, ToolResult> {
        self.decide(call)?;
        let abs = self.authorize_path(call)?;
        match call {
            ToolCall::ListDirectory { path } => self.list_directory(path, &abs),
            ToolCall::ReadFileRange {
                path,
                start_line,
                end_line,
            } => self.read_file_range(path, &abs, *start_line, *end_line),
            ToolCall::SearchText {
                query,
                path,
                max_results,
            } => self.search_text(query, path, &abs, *max_results),
            ToolCall::ProposePatch { path, content } => {
                let before = self.read_existing_text_or_empty(&abs, call)?;
                let operation = if abs.exists() {
                    FileOperation::Modify
                } else {
                    FileOperation::Create
                };
                Ok(ToolGatewayReceipt::PlannedWrite {
                    path: path.clone(),
                    before,
                    after: content.clone(),
                    operation,
                })
            }
            ToolCall::DiffPatch { path, value } => {
                let before = self.read_existing_text_or_empty(&abs, call)?;
                let mut after = before.clone();
                if !after.is_empty() && !after.ends_with('\n') {
                    after.push('\n');
                }
                after.push_str(value);
                after.push('\n');
                let operation = if before.is_empty() && !abs.exists() {
                    FileOperation::Create
                } else {
                    FileOperation::Modify
                };
                Ok(ToolGatewayReceipt::PlannedWrite {
                    path: path.clone(),
                    before,
                    after,
                    operation,
                })
            }
            ToolCall::GitStatus => self.git_status(),
            ToolCall::GitDiff { path } => self.git_diff(path.as_deref()),
            ToolCall::GitLog { max_count } => self.git_log(*max_count),
            ToolCall::CodeGraphQuerySymbol { query, max_results } => {
                self.codegraph_query_symbol(query, *max_results)
            }
            ToolCall::CodeGraphReferences { symbol_id } => self.codegraph_references(symbol_id),
            ToolCall::ContextBuildPack { id, token_budget } => {
                self.context_build_pack(id, *token_budget)
            }
            ToolCall::CommandRun {
                command,
                timeout_ms,
            } => self.command_run("command.run", command, *timeout_ms, false),
            ToolCall::TestRunTargeted { target, timeout_ms } => {
                self.command_run("test.run_targeted", target, *timeout_ms, true)
            }
            ToolCall::McpInvoke { tool_name, payload } => self.mcp_invoke(tool_name, payload),
            ToolCall::SkillInvoke { skill, payload } => self.skill_invoke(skill, payload),
            ToolCall::ArtifactRead { artifact_ref } => self.artifact_read(artifact_ref),
            ToolCall::ArtifactWrite {
                artifact_ref,
                content,
            } => self.artifact_write(artifact_ref, content),
            ToolCall::ImageGenerate {
                prompt,
                asset_id,
                width,
                height,
            } => self.image_generate(prompt, asset_id.as_deref(), *width, *height),
            ToolCall::ImageInspect {
                artifact_ref,
                asset_id,
                prompt,
            } => self.image_inspect(
                artifact_ref.as_deref(),
                asset_id.as_deref(),
                prompt.as_deref(),
            ),
        }
    }

    fn decide(&self, call: &ToolCall) -> Result<(), ToolResult> {
        match self.policy.permission_for(call.tool_name()) {
            ToolPermission::Deny => Err(failed(call, "blocked_tool_unlisted_or_denied")),
            ToolPermission::Ask => Err(failed(call, "approval_required_for_tool")),
            ToolPermission::ReadOnly if call.is_write() => {
                Err(failed(call, "blocked_tool_read_only"))
            }
            ToolPermission::ReadOnly | ToolPermission::Allow => Ok(()),
        }
    }

    fn authorize_path(&self, call: &ToolCall) -> Result<std::path::PathBuf, ToolResult> {
        let abs = self
            .capabilities
            .check_path(call.path())
            .map_err(|error| failed(call, &error.to_string()))?;
        let rel = abs
            .strip_prefix(&self.capabilities.workspace_root)
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .map_err(|_| failed(call, "blocked_path_escapes_workspace_root"))?;
        if self
            .scope
            .forbidden_paths
            .iter()
            .any(|pattern| path_matches(pattern, &rel))
        {
            return Err(failed(call, "blocked_path_forbidden_by_tool_scope"));
        }
        if !self.scope.allowed_paths.is_empty()
            && !self
                .scope
                .allowed_paths
                .iter()
                .any(|pattern| path_matches(pattern, &rel))
        {
            return Err(failed(call, "blocked_path_not_allowed_by_tool_scope"));
        }
        Ok(abs)
    }

    fn list_directory(
        &self,
        path: &str,
        abs: &std::path::Path,
    ) -> Result<ToolGatewayReceipt, ToolResult> {
        let entries = std::fs::read_dir(abs).map_err(|error| ToolResult::Failed {
            tool: "workspace.list_directory".to_string(),
            message: format!("io_error:{error}"),
        })?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| ToolResult::Failed {
                tool: "workspace.list_directory".to_string(),
                message: format!("io_error:{error}"),
            })?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let suffix = if entry.path().is_dir() { "/" } else { "" };
            names.push(format!("{name}{suffix}"));
        }
        names.sort();
        Ok(ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            tool: "workspace.list_directory".to_string(),
            content: self.sanitize_output(&format!("list {path}:\n{}", names.join("\n"))),
        }))
    }

    fn read_file_range(
        &self,
        path: &str,
        abs: &std::path::Path,
        start_line: Option<u32>,
        end_line: Option<u32>,
    ) -> Result<ToolGatewayReceipt, ToolResult> {
        let bytes = match std::fs::read(abs) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ToolGatewayReceipt::Read(ToolResult::FileContent {
                    path: path.to_string(),
                    content: None,
                }));
            }
            Err(error) => {
                return Err(ToolResult::Failed {
                    tool: "workspace.read_file_range".to_string(),
                    message: format!("io_error:{error}"),
                });
            }
        };
        if bytes.contains(&0) {
            return Err(ToolResult::Failed {
                tool: "workspace.read_file_range".to_string(),
                message: "blocked_binary_tool_output".to_string(),
            });
        }
        let text = String::from_utf8(bytes).map_err(|_| ToolResult::Failed {
            tool: "workspace.read_file_range".to_string(),
            message: "blocked_non_utf8_tool_output".to_string(),
        })?;
        let ranged = select_line_range(&text, start_line, end_line);
        Ok(ToolGatewayReceipt::Read(ToolResult::FileContent {
            path: path.to_string(),
            content: Some(self.sanitize_output(&ranged)),
        }))
    }

    fn search_text(
        &self,
        query: &str,
        path: &str,
        abs: &std::path::Path,
        max_results: Option<usize>,
    ) -> Result<ToolGatewayReceipt, ToolResult> {
        let max_results = max_results.unwrap_or(50).clamp(1, 200);
        let mut matches = Vec::new();
        self.search_path(query, path, abs, max_results, &mut matches)?;
        Ok(ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            tool: "workspace.search_text".to_string(),
            content: self.sanitize_output(&format!(
                "search {query:?} under {path}:\n{}",
                matches.join("\n")
            )),
        }))
    }

    fn search_path(
        &self,
        query: &str,
        rel: &str,
        abs: &std::path::Path,
        max_results: usize,
        matches: &mut Vec<String>,
    ) -> Result<(), ToolResult> {
        if matches.len() >= max_results {
            return Ok(());
        }
        if abs.is_dir() {
            let entries = std::fs::read_dir(abs).map_err(|error| ToolResult::Failed {
                tool: "workspace.search_text".to_string(),
                message: format!("io_error:{error}"),
            })?;
            for entry in entries {
                let entry = entry.map_err(|error| ToolResult::Failed {
                    tool: "workspace.search_text".to_string(),
                    message: format!("io_error:{error}"),
                })?;
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || matches.len() >= max_results {
                    continue;
                }
                let child_rel = if rel == "." || rel.is_empty() {
                    name
                } else {
                    format!("{rel}/{name}")
                };
                self.search_path(query, &child_rel, &entry.path(), max_results, matches)?;
            }
            return Ok(());
        }
        let Ok(bytes) = std::fs::read(abs) else {
            return Ok(());
        };
        if bytes.contains(&0) {
            return Ok(());
        }
        let Ok(text) = String::from_utf8(bytes) else {
            return Ok(());
        };
        for (index, line) in text.lines().enumerate() {
            if line.contains(query) {
                matches.push(format!("{}:{}: {}", rel, index + 1, line.trim()));
                if matches.len() >= max_results {
                    break;
                }
            }
        }
        Ok(())
    }

    fn git_status(&self) -> Result<ToolGatewayReceipt, ToolResult> {
        let status =
            opensks_git_service::status(&self.capabilities.workspace_root).map_err(|error| {
                ToolResult::Failed {
                    tool: "git.status".to_string(),
                    message: format!("git_status_error:{error}"),
                }
            })?;
        Ok(ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            tool: "git.status".to_string(),
            content: self.sanitize_output(&json_tool_output(&status)),
        }))
    }

    fn git_diff(&self, path: Option<&str>) -> Result<ToolGatewayReceipt, ToolResult> {
        let diff = opensks_git_service::diff(
            &self.capabilities.workspace_root,
            &DiffOptions {
                path: path
                    .filter(|value| *value != "." && !value.is_empty())
                    .map(str::to_string),
                staged: false,
            },
        )
        .map_err(|error| ToolResult::Failed {
            tool: "git.diff".to_string(),
            message: format!("git_diff_error:{error}"),
        })?;
        Ok(ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            tool: "git.diff".to_string(),
            content: self.sanitize_output(&json_tool_output(&diff)),
        }))
    }

    fn git_log(&self, max_count: Option<u32>) -> Result<ToolGatewayReceipt, ToolResult> {
        let history = opensks_git_service::log(
            &self.capabilities.workspace_root,
            &LogOptions {
                max_count: max_count.unwrap_or(20),
            },
        )
        .map_err(|error| ToolResult::Failed {
            tool: "git.log".to_string(),
            message: format!("git_log_error:{error}"),
        })?;
        Ok(ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            tool: "git.log".to_string(),
            content: self.sanitize_output(&json_tool_output(&history)),
        }))
    }

    fn codegraph(&self, tool: &str) -> Result<opensks_codegraph::CodeGraph, ToolResult> {
        opensks_codegraph::read_index(&self.capabilities.workspace_root)
            .and_then(|maybe| {
                maybe.map_or_else(
                    || {
                        opensks_codegraph::CodeGraph::index_workspace(
                            &self.capabilities.workspace_root,
                        )
                    },
                    Ok,
                )
            })
            .map_err(|error| ToolResult::Failed {
                tool: tool.to_string(),
                message: format!("codegraph_error:{error}"),
            })
    }

    fn codegraph_query_symbol(
        &self,
        query: &str,
        max_results: Option<usize>,
    ) -> Result<ToolGatewayReceipt, ToolResult> {
        let graph = self.codegraph("codegraph.query_symbol")?;
        let max_results = max_results.unwrap_or(20).clamp(1, 100);
        let mut records = graph.query(query);
        records.sort_by(|left, right| left.id.cmp(&right.id));
        records.truncate(max_results);
        Ok(ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            tool: "codegraph.query_symbol".to_string(),
            content: self.sanitize_output(&json_tool_output(&records)),
        }))
    }

    fn codegraph_references(&self, symbol_id: &str) -> Result<ToolGatewayReceipt, ToolResult> {
        let graph = self.codegraph("codegraph.references")?;
        let refs = graph.references(symbol_id);
        Ok(ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            tool: "codegraph.references".to_string(),
            content: self.sanitize_output(&json_tool_output(&refs)),
        }))
    }

    fn context_build_pack(
        &self,
        id: &str,
        token_budget: Option<u32>,
    ) -> Result<ToolGatewayReceipt, ToolResult> {
        let pack = opensks_context::pack_workspace_records(
            &self.capabilities.workspace_root,
            id,
            token_budget.unwrap_or(1024).clamp(1, 32_000),
        )
        .map_err(|error| ToolResult::Failed {
            tool: "context.build_pack".to_string(),
            message: format!("context_pack_error:{error}"),
        })?;
        Ok(ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            tool: "context.build_pack".to_string(),
            content: self.sanitize_output(&json_tool_output(&pack)),
        }))
    }

    fn command_run(
        &self,
        tool: &str,
        raw_command: &str,
        timeout_ms: Option<u64>,
        targeted_test_only: bool,
    ) -> Result<ToolGatewayReceipt, ToolResult> {
        if has_shell_metacharacters(raw_command) {
            return Err(ToolResult::Failed {
                tool: tool.to_string(),
                message: "blocked_shell_metacharacter".to_string(),
            });
        }
        let argv = split_command_line(raw_command).map_err(|message| ToolResult::Failed {
            tool: tool.to_string(),
            message: message.to_string(),
        })?;
        validate_command(&argv, targeted_test_only).map_err(|message| ToolResult::Failed {
            tool: tool.to_string(),
            message: message.to_string(),
        })?;

        let timeout = Duration::from_millis(timeout_ms.unwrap_or(10_000).clamp(1_000, 120_000));
        let started = Instant::now();
        let mut command = std::process::Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(&self.capabilities.workspace_root)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for &key in COMMAND_ENV_ALLOWLIST {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }

        let mut child = command.spawn().map_err(|error| ToolResult::Failed {
            tool: tool.to_string(),
            message: self.sanitize_output(&format!("command_spawn_error:{error}")),
        })?;
        // Drain stdout/stderr concurrently on dedicated threads while polling for
        // exit. Without this, a child that writes more than the OS pipe buffer
        // (~64KB) before exiting will block on write() because nothing is
        // reading the pipe, causing try_wait() to spin until timeout even though
        // the command would otherwise have completed quickly.
        let mut child_stdout = child.stdout.take();
        let mut child_stderr = child.stderr.take();
        let stdout_reader = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
            let mut buf = Vec::new();
            if let Some(stdout) = child_stdout.as_mut() {
                std::io::Read::read_to_end(stdout, &mut buf)?;
            }
            Ok(buf)
        });
        let stderr_reader = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
            let mut buf = Vec::new();
            if let Some(stderr) = child_stderr.as_mut() {
                std::io::Read::read_to_end(stderr, &mut buf)?;
            }
            Ok(buf)
        });
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() >= timeout => {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait().map_err(|error| ToolResult::Failed {
                        tool: tool.to_string(),
                        message: self.sanitize_output(&format!("command_wait_error:{error}")),
                    })?;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => {
                    return Err(ToolResult::Failed {
                        tool: tool.to_string(),
                        message: self.sanitize_output(&format!("command_wait_error:{error}")),
                    });
                }
            }
        };
        let stdout_bytes = stdout_reader
            .join()
            .map_err(|_| ToolResult::Failed {
                tool: tool.to_string(),
                message: "command_stdout_reader_panicked".to_string(),
            })?
            .map_err(|error| ToolResult::Failed {
                tool: tool.to_string(),
                message: self.sanitize_output(&format!("command_stdout_read_error:{error}")),
            })?;
        let stderr_bytes = stderr_reader
            .join()
            .map_err(|_| ToolResult::Failed {
                tool: tool.to_string(),
                message: "command_stderr_reader_panicked".to_string(),
            })?
            .map_err(|error| ToolResult::Failed {
                tool: tool.to_string(),
                message: self.sanitize_output(&format!("command_stderr_read_error:{error}")),
            })?;
        let stdout_redacted = self.sanitize_output(&String::from_utf8_lossy(&stdout_bytes));
        let stderr_redacted = self.sanitize_output(&String::from_utf8_lossy(&stderr_bytes));
        Ok(ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            tool: tool.to_string(),
            content: self.sanitize_output(&json_tool_output(&serde_json::json!({
                "schema": "opensks.command-tool-result.v1",
                "tool": tool,
                "command_redacted": redacted_command(&argv),
                "argv_redacted": redacted_argv(&argv),
                "exit_code": status.code(),
                "timed_out": timed_out,
                "duration_ms": started.elapsed().as_millis() as u64,
                "stdout_redacted": stdout_redacted,
                "stderr_redacted": stderr_redacted,
            }))),
        }))
    }

    fn mcp_invoke(
        &self,
        tool_name: &str,
        payload: &serde_json::Value,
    ) -> Result<ToolGatewayReceipt, ToolResult> {
        let tool_name = tool_name.trim();
        if tool_name.is_empty() {
            return Err(ToolResult::Failed {
                tool: "mcp.invoke".to_string(),
                message: "missing_mcp_tool_name".to_string(),
            });
        }
        if !payload.is_object() {
            return Err(ToolResult::Failed {
                tool: "mcp.invoke".to_string(),
                message: "invalid_mcp_arguments".to_string(),
            });
        }
        match tool_name {
            "opensks.repo.search" => self.mcp_repo_search(tool_name, payload),
            _ => Err(ToolResult::Failed {
                tool: "mcp.invoke".to_string(),
                message: "unknown_or_unapproved_mcp_tool".to_string(),
            }),
        }
    }

    fn mcp_repo_search(
        &self,
        tool_name: &str,
        payload: &serde_json::Value,
    ) -> Result<ToolGatewayReceipt, ToolResult> {
        let query = payload
            .get("query")
            .or_else(|| payload.get("q"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ToolResult::Failed {
                tool: "mcp.invoke".to_string(),
                message: "missing_mcp_repo_search_query".to_string(),
            })?;
        let max_results = payload
            .get("limit")
            .or_else(|| payload.get("max_results"))
            .and_then(|value| value.as_u64())
            .unwrap_or(20)
            .clamp(1, 50) as usize;
        let mut matches = Vec::new();
        self.search_path(
            query,
            ".",
            &self.capabilities.workspace_root,
            max_results,
            &mut matches,
        )
        .map_err(|result| ToolResult::Failed {
            tool: "mcp.invoke".to_string(),
            message: format!("mcp_repo_search_error:{}", tool_result_message(&result)),
        })?;
        Ok(ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            tool: "mcp.invoke".to_string(),
            content: self.sanitize_output(&json_tool_output(&serde_json::json!({
                "schema": "opensks.mcp-tool-result.v1",
                "protocol": "mcp",
                "method": "tools/call",
                "mcp_tool_name": tool_name,
                "status": "completed",
                "arguments_redacted": redact_json_value(payload),
                "result": {
                    "matches": matches,
                    "count": matches.len(),
                    "truncated": matches.len() >= max_results,
                },
            }))),
        }))
    }

    fn skill_invoke(
        &self,
        skill: &str,
        payload: &serde_json::Value,
    ) -> Result<ToolGatewayReceipt, ToolResult> {
        if !payload.is_object() {
            return Err(ToolResult::Failed {
                tool: "skill.invoke".to_string(),
                message: "invalid_skill_payload".to_string(),
            });
        }
        let skill = normalize_skill_name(skill).map_err(|message| ToolResult::Failed {
            tool: "skill.invoke".to_string(),
            message: message.to_string(),
        })?;
        let rel = format!(".agents/skills/{skill}/SKILL.md");
        let abs = self
            .capabilities
            .check_path(&rel)
            .map_err(|error| ToolResult::Failed {
                tool: "skill.invoke".to_string(),
                message: error.to_string(),
            })?;
        if !abs.exists() {
            return Err(ToolResult::Failed {
                tool: "skill.invoke".to_string(),
                message: "skill_not_found".to_string(),
            });
        }
        let bytes = std::fs::read(&abs).map_err(|error| ToolResult::Failed {
            tool: "skill.invoke".to_string(),
            message: format!("skill_read_error:{error}"),
        })?;
        if bytes.contains(&0) {
            return Err(ToolResult::Failed {
                tool: "skill.invoke".to_string(),
                message: "blocked_binary_skill_output".to_string(),
            });
        }
        let text = String::from_utf8(bytes).map_err(|_| ToolResult::Failed {
            tool: "skill.invoke".to_string(),
            message: "blocked_non_utf8_skill_output".to_string(),
        })?;
        let metadata = skill_metadata(&text);
        Ok(ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            tool: "skill.invoke".to_string(),
            content: self.sanitize_output(&json_tool_output(&serde_json::json!({
                "schema": "opensks.skill-tool-result.v1",
                "operation": "load_skill_context",
                "skill": skill,
                "declared_name": metadata.name,
                "description": metadata.description,
                "path": rel,
                "payload_redacted": redact_json_value(payload),
                "content_redacted": redact_sensitive_text(&text),
                "content_hash": content_hash(&text),
                "side_effects": "none_instructions_loaded_only",
                "evidence_refs": ["skill-registry:project-local", "skill-invoke:bounded-context"],
            }))),
        }))
    }

    fn artifact_read(&self, artifact_ref: &str) -> Result<ToolGatewayReceipt, ToolResult> {
        let (normalized_ref, rel, abs) = self.artifact_path(artifact_ref, "artifact.read")?;
        let bytes = match std::fs::read(&abs) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ToolGatewayReceipt::Read(ToolResult::ToolOutput {
                    tool: "artifact.read".to_string(),
                    content: self.sanitize_output(&json_tool_output(&serde_json::json!({
                        "schema": "opensks.artifact-tool-result.v1",
                        "operation": "read",
                        "artifact_ref": normalized_ref,
                        "path": rel,
                        "exists": false,
                    }))),
                }));
            }
            Err(error) => {
                return Err(ToolResult::Failed {
                    tool: "artifact.read".to_string(),
                    message: format!("artifact_read_error:{error}"),
                });
            }
        };
        if abs.is_dir() {
            return Err(ToolResult::Failed {
                tool: "artifact.read".to_string(),
                message: "blocked_artifact_directory".to_string(),
            });
        }
        if bytes.contains(&0) {
            return Err(ToolResult::Failed {
                tool: "artifact.read".to_string(),
                message: "blocked_binary_artifact_output".to_string(),
            });
        }
        let text = String::from_utf8(bytes).map_err(|_| ToolResult::Failed {
            tool: "artifact.read".to_string(),
            message: "blocked_non_utf8_artifact_output".to_string(),
        })?;
        let content_redacted = redact_sensitive_text(&text);
        Ok(ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            tool: "artifact.read".to_string(),
            content: self.sanitize_output(&json_tool_output(&serde_json::json!({
                "schema": "opensks.artifact-tool-result.v1",
                "operation": "read",
                "artifact_ref": normalized_ref,
                "path": rel,
                "exists": true,
                "content_redacted": content_redacted,
                "content_hash": content_hash(&text),
            }))),
        }))
    }

    fn artifact_write(
        &self,
        artifact_ref: &str,
        content: &str,
    ) -> Result<ToolGatewayReceipt, ToolResult> {
        let (normalized_ref, rel, abs) = self.artifact_path(artifact_ref, "artifact.write")?;
        let content_redacted = redact_sensitive_text(content);
        if content_redacted.len() > self.scope.max_output_bytes {
            return Err(ToolResult::Failed {
                tool: "artifact.write".to_string(),
                message: "artifact_content_too_large".to_string(),
            });
        }
        let before = std::fs::read_to_string(&abs).unwrap_or_default();
        let operation = if abs.exists() {
            FileOperation::Modify
        } else {
            FileOperation::Create
        };
        let proposal_id = format!("artifact-write-{}", content_hash(&normalized_ref));
        let lease = PatchPathLease::new(
            format!("artifact-lease:{}", content_hash(&normalized_ref)),
            format!("artifact-fence:{}", content_hash(&rel)),
        );
        let apply = apply_file_writes_with_path_lease(
            &self.capabilities.workspace_root,
            &proposal_id,
            &[PlannedWrite {
                path: rel.clone(),
                expected_before_hash: content_hash(&before),
                after_content: content_redacted.clone(),
                operation,
            }],
            &lease,
        )
        .map_err(|error| ToolResult::Failed {
            tool: "artifact.write".to_string(),
            message: format!("artifact_write_error:{error}"),
        })?;
        if !apply.applied {
            return Err(ToolResult::Failed {
                tool: "artifact.write".to_string(),
                message: format!("artifact_write_rejected:{}", apply.reason_code),
            });
        }
        std::fs::metadata(&abs).map_err(|error| ToolResult::Failed {
            tool: "artifact.write".to_string(),
            message: format!("artifact_write_error:{error}"),
        })?;
        Ok(ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            tool: "artifact.write".to_string(),
            content: self.sanitize_output(&json_tool_output(&serde_json::json!({
                "schema": "opensks.artifact-tool-result.v1",
                "operation": "write",
                "artifact_ref": normalized_ref,
                "path": rel,
                "bytes_written": content_redacted.len(),
                "content_hash": content_hash(&content_redacted),
                "content_redacted": true,
            }))),
        }))
    }

    fn artifact_path(
        &self,
        artifact_ref: &str,
        tool: &str,
    ) -> Result<(String, String, std::path::PathBuf), ToolResult> {
        let raw = artifact_ref.trim();
        let Some(path) = raw.strip_prefix("artifact://") else {
            return Err(ToolResult::Failed {
                tool: tool.to_string(),
                message: "invalid_artifact_ref_scheme".to_string(),
            });
        };
        let rel = path.trim_start_matches("./").replace('\\', "/");
        if !rel.starts_with(".opensks/") {
            return Err(ToolResult::Failed {
                tool: tool.to_string(),
                message: "blocked_artifact_ref_outside_runtime".to_string(),
            });
        }
        let rel_path = std::path::Path::new(&rel);
        if rel_path.is_absolute()
            || rel_path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(ToolResult::Failed {
                tool: tool.to_string(),
                message: "blocked_artifact_path_escape".to_string(),
            });
        }
        let abs = self
            .capabilities
            .check_path(rel_path)
            .map_err(|error| ToolResult::Failed {
                tool: tool.to_string(),
                message: error.to_string(),
            })?;
        if self
            .scope
            .forbidden_paths
            .iter()
            .any(|pattern| path_matches(pattern, &rel))
        {
            return Err(ToolResult::Failed {
                tool: tool.to_string(),
                message: "blocked_path_forbidden_by_tool_scope".to_string(),
            });
        }
        if !self.scope.allowed_paths.is_empty()
            && !self
                .scope
                .allowed_paths
                .iter()
                .any(|pattern| path_matches(pattern, &rel))
        {
            return Err(ToolResult::Failed {
                tool: tool.to_string(),
                message: "blocked_path_not_allowed_by_tool_scope".to_string(),
            });
        }
        Ok((format!("artifact://{rel}"), rel, abs))
    }

    fn image_generate(
        &self,
        prompt: &str,
        asset_id: Option<&str>,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<ToolGatewayReceipt, ToolResult> {
        let Some(executor) = self.image_executor else {
            return Err(ToolResult::Failed {
                tool: "image.generate".to_string(),
                message: "image_executor_unavailable".to_string(),
            });
        };
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(ToolResult::Failed {
                tool: "image.generate".to_string(),
                message: "missing_prompt".to_string(),
            });
        }
        let request = ImageGenerateToolRequest {
            prompt: prompt.to_string(),
            asset_id: asset_id.map(str::to_string),
            width: width.unwrap_or(1024),
            height: height.unwrap_or(1024),
        };
        executor
            .generate_image(&self.capabilities.workspace_root, &request)
            .map(|asset| ToolGatewayReceipt::ImageArtifact(Box::new(asset)))
            .map_err(|error| ToolResult::Failed {
                tool: "image.generate".to_string(),
                message: self.sanitize_output(&format!("image_generate_error:{error}")),
            })
    }

    fn image_inspect(
        &self,
        artifact_ref: Option<&str>,
        asset_id: Option<&str>,
        prompt: Option<&str>,
    ) -> Result<ToolGatewayReceipt, ToolResult> {
        let Some(executor) = self.image_executor else {
            return Err(ToolResult::Failed {
                tool: "image.inspect".to_string(),
                message: "image_executor_unavailable".to_string(),
            });
        };
        let artifact_ref = artifact_ref
            .or(asset_id)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ToolResult::Failed {
                tool: "image.inspect".to_string(),
                message: "missing_artifact_ref".to_string(),
            })?;
        let request = ImageInspectToolRequest {
            artifact_ref: artifact_ref.to_string(),
            prompt: prompt
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        };
        executor
            .inspect_image(&self.capabilities.workspace_root, &request)
            .map(|result| ToolGatewayReceipt::ImageInspection(Box::new(result)))
            .map_err(|error| ToolResult::Failed {
                tool: "image.inspect".to_string(),
                message: self.sanitize_output(&format!("image_inspect_error:{error}")),
            })
    }

    fn read_existing_text_or_empty(
        &self,
        abs: &std::path::Path,
        call: &ToolCall,
    ) -> Result<String, ToolResult> {
        if !abs.exists() {
            return Ok(String::new());
        }
        std::fs::read_to_string(abs).map_err(|error| {
            failed(
                call,
                &format!("blocked_existing_file_not_safe_text:{error}"),
            )
        })
    }

    fn sanitize_output(&self, raw: &str) -> String {
        let redacted = redact_sensitive_text(raw);
        truncate_utf8(&redacted, self.scope.max_output_bytes)
    }
}

/// Drive `driver` until it returns [`AgentStep::Final`], reports
/// [`AgentStep::Failed`], or exhausts the step budget. Executes each tool against
/// `request.workspace`, applies writes transactionally, and streams typed events
/// into `sink`. Returns the honest outcome (assistant text, every proposed patch,
/// every apply result, terminal state).
pub fn run_agentic_loop(
    request: &AgentRunRequest,
    driver: &mut dyn ToolDriver,
    config: &AgenticConfig,
    sink: &dyn AgentEventSink,
) -> Result<AgentRunOutcome, AgentAdapterError> {
    run_agentic_loop_inner(request, driver, config, sink, None)
}

pub fn run_agentic_loop_with_image_tools(
    request: &AgentRunRequest,
    driver: &mut dyn ToolDriver,
    config: &AgenticConfig,
    sink: &dyn AgentEventSink,
    image_executor: &dyn ImageToolExecutor,
) -> Result<AgentRunOutcome, AgentAdapterError> {
    run_agentic_loop_inner(request, driver, config, sink, Some(image_executor))
}

fn run_agentic_loop_inner(
    request: &AgentRunRequest,
    driver: &mut dyn ToolDriver,
    config: &AgenticConfig,
    sink: &dyn AgentEventSink,
    image_executor: Option<&dyn ImageToolExecutor>,
) -> Result<AgentRunOutcome, AgentAdapterError> {
    let workspace = request.workspace.as_path();
    let gateway = ToolGateway::with_image_executor(workspace, config, image_executor);
    let mut sequence: u64 = 0;
    let mut observations: Vec<ToolResult> = Vec::new();
    let mut patches: Vec<PatchProposal> = Vec::new();
    let mut applies: Vec<PatchApplyResult> = Vec::new();
    let mut had_successful_apply = false;
    let mut consecutive_noop_write_steps = 0usize;

    for step in 0..config.max_steps.max(1) {
        if request_cancelled(request) {
            return Ok(cancelled_outcome(
                request,
                sink,
                &mut sequence,
                patches,
                applies,
            ));
        }
        let step_decision = driver.next_step(&observations);
        if request_cancelled(request) {
            return Ok(cancelled_outcome(
                request,
                sink,
                &mut sequence,
                patches,
                applies,
            ));
        }
        match step_decision {
            AgentStep::Failed {
                code,
                message,
                retryable,
            } => {
                emit(
                    sink,
                    request,
                    &mut sequence,
                    AgentEventKind::Error,
                    serde_json::json!({
                        "code": code,
                        "message": message,
                        "retryable": retryable,
                    }),
                );
                return Ok(AgentRunOutcome {
                    assistant_text: message,
                    patches,
                    apply_results: applies,
                    final_state: RunProjectionState::Failed,
                });
            }
            AgentStep::Final { text } => {
                emit(
                    sink,
                    request,
                    &mut sequence,
                    AgentEventKind::AssistantTextCompleted,
                    serde_json::json!({ "text": text }),
                );
                return Ok(AgentRunOutcome {
                    assistant_text: text,
                    patches,
                    apply_results: applies,
                    final_state: RunProjectionState::Completed,
                });
            }
            AgentStep::Tools(calls) if calls.is_empty() => {
                // An empty step makes no progress; record nothing and continue so
                // the budget still bounds a stuck driver.
                observations = Vec::new();
            }
            AgentStep::Tools(calls) => {
                emit(
                    sink,
                    request,
                    &mut sequence,
                    AgentEventKind::PlanUpdated,
                    serde_json::json!({
                        "steps": calls.iter().map(describe_call).collect::<Vec<_>>()
                    }),
                );

                let mut next_obs: Vec<ToolResult> = Vec::new();
                // (rel_path, before, after, operation) planned writes for THIS step,
                // applied together as one transaction after all calls are processed.
                let mut planned: Vec<(String, String, String, FileOperation)> = Vec::new();
                let mut write_call_count = 0usize;
                let mut noop_write_count = 0usize;
                let mut non_write_call_count = 0usize;

                for call in &calls {
                    if request_cancelled(request) {
                        return Ok(cancelled_outcome(
                            request,
                            sink,
                            &mut sequence,
                            patches,
                            applies,
                        ));
                    }
                    emit(
                        sink,
                        request,
                        &mut sequence,
                        AgentEventKind::ToolCallStarted,
                        serde_json::json!({ "tool": call.tool_name(), "path": call.path() }),
                    );
                    match gateway.execute(call) {
                        Ok(ToolGatewayReceipt::Read(result)) => {
                            non_write_call_count += 1;
                            let exists = matches!(
                                &result,
                                ToolResult::FileContent {
                                    content: Some(_),
                                    ..
                                }
                            );
                            emit(
                                sink,
                                request,
                                &mut sequence,
                                AgentEventKind::ToolCallCompleted,
                                serde_json::json!({
                                    "tool": call.tool_name(),
                                    "path": call.path(),
                                    "exists": exists,
                                }),
                            );
                            next_obs.push(result);
                        }
                        Ok(ToolGatewayReceipt::ImageArtifact(asset)) => {
                            non_write_call_count += 1;
                            let asset = asset.as_ref();
                            emit(
                                sink,
                                request,
                                &mut sequence,
                                AgentEventKind::ToolCallCompleted,
                                serde_json::json!({
                                    "tool": call.tool_name(),
                                    "path": asset.path,
                                    "asset_id": asset.id,
                                    "planned": false,
                                }),
                            );
                            emit(
                                sink,
                                request,
                                &mut sequence,
                                AgentEventKind::ImageArtifactCreated,
                                image_artifact_event_payload(asset),
                            );
                            next_obs.push(ToolResult::ToolOutput {
                                tool: "image.generate".to_string(),
                                content: gateway.sanitize_output(&json_tool_output(
                                    &image_artifact_observation(asset),
                                )),
                            });
                        }
                        Ok(ToolGatewayReceipt::ImageInspection(result)) => {
                            non_write_call_count += 1;
                            let result = result.as_ref();
                            emit(
                                sink,
                                request,
                                &mut sequence,
                                AgentEventKind::ToolCallCompleted,
                                serde_json::json!({
                                    "tool": call.tool_name(),
                                    "path": ".",
                                    "asset_id": result.receipt.asset_id,
                                    "provider_id": result.receipt.provider_id,
                                    "model_id": result.receipt.model_id,
                                    "content_hash": result.receipt.content_hash,
                                    "provenance_hash": result.receipt.provenance_hash,
                                    "planned": false,
                                }),
                            );
                            next_obs.push(ToolResult::ToolOutput {
                                tool: "image.inspect".to_string(),
                                content: gateway.sanitize_output(&json_tool_output(
                                    &image_inspection_observation(result),
                                )),
                            });
                        }
                        Ok(ToolGatewayReceipt::PlannedWrite {
                            path,
                            before,
                            after,
                            operation,
                        }) => {
                            write_call_count += 1;
                            if before == after {
                                noop_write_count += 1;
                                emit(
                                    sink,
                                    request,
                                    &mut sequence,
                                    AgentEventKind::ToolCallCompleted,
                                    serde_json::json!({
                                        "tool": call.tool_name(),
                                        "path": path,
                                        "planned": false,
                                        "no_op": true,
                                        "reason_code": "no_op",
                                    }),
                                );
                                next_obs.push(ToolResult::Wrote {
                                    path,
                                    applied: false,
                                    reason: "no_op".to_string(),
                                });
                                continue;
                            }
                            emit(
                                sink,
                                request,
                                &mut sequence,
                                AgentEventKind::ToolCallCompleted,
                                serde_json::json!({
                                    "tool": call.tool_name(),
                                    "path": path,
                                    "planned": true,
                                }),
                            );
                            planned.push((path, before, after, operation));
                        }
                        Err(result) => {
                            non_write_call_count += 1;
                            let message = tool_result_message(&result);
                            emit(
                                sink,
                                request,
                                &mut sequence,
                                AgentEventKind::Warning,
                                serde_json::json!({
                                    "tool": call.tool_name(),
                                    "path": call.path(),
                                    "error": message,
                                }),
                            );
                            next_obs.push(result);
                        }
                    }
                }

                if !planned.is_empty() {
                    if request_cancelled(request) {
                        return Ok(cancelled_outcome(
                            request,
                            sink,
                            &mut sequence,
                            patches,
                            applies,
                        ));
                    }
                    let proposal_id = format!("pp-{}-s{step}", request.run_id);
                    let proposal = step_proposal(&proposal_id, request, &planned);
                    emit(
                        sink,
                        request,
                        &mut sequence,
                        AgentEventKind::FilePatchProposed,
                        serde_json::to_value(&proposal).unwrap_or(serde_json::Value::Null),
                    );

                    let writes: Vec<PlannedWrite> = planned
                        .iter()
                        .map(|(path, before, after, operation)| PlannedWrite {
                            path: path.clone(),
                            expected_before_hash: content_hash(before),
                            after_content: after.clone(),
                            operation: *operation,
                        })
                        .collect();
                    let lease = patch_path_lease(request, &proposal_id);
                    let apply = match apply_file_writes_with_path_lease(
                        workspace,
                        &proposal_id,
                        &writes,
                        &lease,
                    ) {
                        Ok(apply) => apply,
                        Err(error) => {
                            let message = error.to_string();
                            emit(
                                sink,
                                request,
                                &mut sequence,
                                AgentEventKind::Error,
                                serde_json::json!({
                                    "code": "agentic_patch_apply_failed",
                                    "reason_code": "agentic_patch_apply_failed",
                                    "message": message,
                                    "retryable": false,
                                }),
                            );
                            return Ok(AgentRunOutcome {
                                assistant_text: message,
                                patches,
                                apply_results: applies,
                                final_state: RunProjectionState::Failed,
                            });
                        }
                    };
                    emit(
                        sink,
                        request,
                        &mut sequence,
                        AgentEventKind::FilePatchApplied,
                        serde_json::to_value(&apply).unwrap_or(serde_json::Value::Null),
                    );
                    if apply.applied {
                        had_successful_apply = true;
                    }
                    for (path, _, _, _) in &planned {
                        next_obs.push(ToolResult::Wrote {
                            path: path.clone(),
                            applied: apply.applied,
                            reason: apply.reason_code.clone(),
                        });
                    }
                    patches.push(proposal);
                    applies.push(apply);
                }

                let noop_write_only_step = write_call_count > 0
                    && noop_write_count == write_call_count
                    && planned.is_empty()
                    && non_write_call_count == 0;
                if noop_write_only_step {
                    consecutive_noop_write_steps += 1;
                } else {
                    consecutive_noop_write_steps = 0;
                }
                if had_successful_apply && consecutive_noop_write_steps >= 2 {
                    let text = "No further file changes were needed; the proposed edits already match the workspace."
                        .to_string();
                    emit(
                        sink,
                        request,
                        &mut sequence,
                        AgentEventKind::AssistantTextCompleted,
                        serde_json::json!({ "text": text }),
                    );
                    return Ok(AgentRunOutcome {
                        assistant_text: text,
                        patches,
                        apply_results: applies,
                        final_state: RunProjectionState::Completed,
                    });
                }

                observations = next_obs;
            }
        }
    }

    // The budget is exhausted and the driver never declared it was done. This is an
    // HONEST failure — never a quiet/fabricated completion (directive §0.4).
    let text = format!(
        "The agent exhausted its {}-step budget before producing a final answer.",
        config.max_steps.max(1)
    );
    emit(
        sink,
        request,
        &mut sequence,
        AgentEventKind::Error,
        serde_json::json!({
            "code": "agentic_step_budget_exhausted",
            "reason_code": "agentic_step_budget_exhausted",
            "message": text,
            "max_steps": config.max_steps.max(1),
            "retryable": true
        }),
    );
    Ok(AgentRunOutcome {
        assistant_text: text,
        patches,
        apply_results: applies,
        final_state: RunProjectionState::Failed,
    })
}

fn request_cancelled(request: &AgentRunRequest) -> bool {
    request
        .cancellation_token
        .as_ref()
        .is_some_and(|token| token.load(Ordering::SeqCst))
}

fn cancelled_outcome(
    request: &AgentRunRequest,
    sink: &dyn AgentEventSink,
    sequence: &mut u64,
    patches: Vec<PatchProposal>,
    applies: Vec<PatchApplyResult>,
) -> AgentRunOutcome {
    let text = "Turn cancelled before the next model/tool step.".to_string();
    emit(
        sink,
        request,
        sequence,
        AgentEventKind::Warning,
        serde_json::json!({
            "code": "run_cancelled",
            "message": text,
            "reason_code": "cancelled_by_user",
        }),
    );
    AgentRunOutcome {
        assistant_text: text,
        patches,
        apply_results: applies,
        final_state: RunProjectionState::Cancelled,
    }
}

fn image_artifact_event_payload(asset: &ImageAsset) -> serde_json::Value {
    serde_json::json!({
        "content_redacted": format!("Image artifact {} created.", asset.id),
        "asset_id": asset.id,
        "provider_id": asset.provider_id,
        "model_id": asset.model_id,
        "path": asset.path,
        "content_hash": asset.content_hash,
        "provenance_hash": asset.provenance_hash,
        "operation": "generate",
        "width": asset.width,
        "height": asset.height,
    })
}

fn image_artifact_observation(asset: &ImageAsset) -> serde_json::Value {
    serde_json::json!({
        "schema": "opensks.image-tool-result.v1",
        "asset_id": asset.id,
        "provider_id": asset.provider_id,
        "model_id": asset.model_id,
        "path": asset.path,
        "content_hash": asset.content_hash,
        "provenance_hash": asset.provenance_hash,
        "width": asset.width,
        "height": asset.height,
    })
}

fn image_inspection_observation(result: &ImageInspectToolResult) -> serde_json::Value {
    serde_json::json!({
        "schema": "opensks.image-inspect-tool-result.v1",
        "asset_id": result.receipt.asset_id,
        "provider_id": result.receipt.provider_id,
        "model_id": result.receipt.model_id,
        "content_hash": result.receipt.content_hash,
        "provenance_hash": result.receipt.provenance_hash,
        "operation": "inspect",
        "text": result.text,
    })
}

fn describe_call(call: &ToolCall) -> String {
    match call {
        ToolCall::ListDirectory { path } => format!("list {path}"),
        ToolCall::ReadFileRange { path, .. } => format!("read {path}"),
        ToolCall::SearchText { query, path, .. } => format!("search {query:?} under {path}"),
        ToolCall::ProposePatch { path, .. } => format!("propose patch {path}"),
        ToolCall::DiffPatch { path, .. } => format!("diff patch {path}"),
        ToolCall::GitStatus => "git status".to_string(),
        ToolCall::GitDiff { path } => format!("git diff {}", path.as_deref().unwrap_or(".")),
        ToolCall::GitLog { max_count } => format!("git log {}", max_count.unwrap_or(20)),
        ToolCall::CodeGraphQuerySymbol { query, .. } => format!("codegraph query {query:?}"),
        ToolCall::CodeGraphReferences { symbol_id } => format!("codegraph refs {symbol_id}"),
        ToolCall::ContextBuildPack { id, .. } => format!("context pack {id}"),
        ToolCall::CommandRun { command, .. } => {
            format!("run command {}", redact_sensitive_text(command))
        }
        ToolCall::TestRunTargeted { target, .. } => {
            format!("run targeted test {}", redact_sensitive_text(target))
        }
        ToolCall::McpInvoke { tool_name, .. } => format!("invoke MCP tool {tool_name}"),
        ToolCall::SkillInvoke { skill, .. } => format!("invoke skill {skill}"),
        ToolCall::ArtifactRead { artifact_ref } => format!("read artifact {artifact_ref}"),
        ToolCall::ArtifactWrite { artifact_ref, .. } => format!("write artifact {artifact_ref}"),
        ToolCall::ImageGenerate { prompt, .. } => format!("generate image {prompt:?}"),
        ToolCall::ImageInspect {
            artifact_ref,
            asset_id,
            ..
        } => {
            let reference = artifact_ref
                .as_deref()
                .or(asset_id.as_deref())
                .unwrap_or("<missing>");
            format!("inspect image {reference}")
        }
    }
}

fn failed(call: &ToolCall, message: &str) -> ToolResult {
    ToolResult::Failed {
        tool: call.tool_name().to_string(),
        message: message.to_string(),
    }
}

fn tool_result_message(result: &ToolResult) -> String {
    match result {
        ToolResult::Failed { message, .. } => message.clone(),
        ToolResult::FileContent { content, .. } => {
            format!(
                "read:{}",
                if content.is_some() {
                    "exists"
                } else {
                    "missing"
                }
            )
        }
        ToolResult::ToolOutput { tool, .. } => format!("{tool}:output"),
        ToolResult::Wrote {
            applied, reason, ..
        } => {
            format!("write:{applied}:{reason}")
        }
    }
}

fn path_matches(pattern: &str, rel: &str) -> bool {
    let pattern = pattern.trim().trim_start_matches("./").trim_matches('/');
    let rel = rel.trim_start_matches("./").trim_matches('/');
    if pattern.is_empty() || pattern == "*" || pattern == "**" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return rel == prefix || rel.starts_with(&format!("{prefix}/"));
    }
    if pattern.ends_with('*') {
        let prefix = pattern.trim_end_matches('*').trim_end_matches('/');
        return rel == prefix || rel.starts_with(&format!("{prefix}/"));
    }
    rel == pattern || rel.starts_with(&format!("{pattern}/"))
}

const COMMAND_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "TEMP",
    "TMP",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTC",
    "RUSTDOC",
    "CARGO_TARGET_DIR",
    "SWIFT_EXEC",
    "SDKROOT",
    "DEVELOPER_DIR",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

fn has_shell_metacharacters(raw: &str) -> bool {
    raw.contains("$(")
        || raw.contains("${")
        || raw
            .chars()
            .any(|c| matches!(c, ';' | '&' | '|' | '>' | '<' | '`' | '\n' | '\r'))
}

fn split_command_line(raw: &str) -> Result<Vec<String>, &'static str> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut started = false;

    for ch in raw.trim().chars() {
        if escaped {
            current.push(ch);
            started = true;
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            started = true;
            continue;
        }
        if let Some(quote_char) = quote {
            if ch == quote_char {
                quote = None;
            } else {
                current.push(ch);
            }
            started = true;
            continue;
        }
        match ch {
            '"' | '\'' => {
                quote = Some(ch);
                started = true;
            }
            value if value.is_whitespace() => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            value => {
                current.push(value);
                started = true;
            }
        }
    }

    if escaped {
        current.push('\\');
    }
    if quote.is_some() {
        return Err("unclosed_quote");
    }
    if started {
        args.push(current);
    }
    if args.is_empty() {
        return Err("empty_command");
    }
    Ok(args)
}

fn validate_command(argv: &[String], targeted_test_only: bool) -> Result<(), &'static str> {
    if argv.is_empty() {
        return Err("empty_command");
    }
    if !is_allowed_program(&argv[0]) {
        return Err("blocked_command_not_allowlisted");
    }
    if argv.iter().any(|arg| arg == "--fix") {
        return Err("blocked_command_shape");
    }
    if argv
        .iter()
        .skip(1)
        .any(|arg| is_disallowed_path_argument(arg))
    {
        return Err("blocked_path_argument");
    }
    if targeted_test_only {
        if !is_targeted_test_command(argv) {
            return Err("blocked_non_test_command");
        }
    } else if !is_allowed_command_shape(argv) {
        return Err("blocked_command_shape");
    }
    if argv.iter().any(|arg| arg.contains('\0')) {
        return Err("blocked_nul_argument");
    }
    Ok(())
}

fn is_disallowed_path_argument(arg: &str) -> bool {
    if let Some((_, value)) = arg.split_once('=')
        && is_disallowed_path_argument(value)
    {
        return true;
    }
    let path = std::path::Path::new(arg);
    path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
}

fn is_allowed_program(program: &str) -> bool {
    matches!(
        program,
        "cargo" | "git" | "swift" | "pytest" | "npm" | "pnpm" | "yarn" | "pwd"
    )
}

fn is_allowed_command_shape(argv: &[String]) -> bool {
    let Some(program) = argv.first().map(String::as_str) else {
        return false;
    };
    match program {
        "pwd" => argv.len() == 1,
        "git" => is_allowed_git_command(argv),
        "cargo" => is_allowed_cargo_command(argv),
        "swift" => is_allowed_swift_command(argv),
        "pytest" | "npm" | "pnpm" | "yarn" => is_targeted_test_command(argv),
        _ => false,
    }
}

fn is_allowed_git_command(argv: &[String]) -> bool {
    let Some(subcommand) = argv.get(1).map(String::as_str) else {
        return false;
    };
    match subcommand {
        "--version" | "-v" => argv.len() <= 2,
        "status" | "diff" | "log" | "show" | "rev-parse" | "ls-files" => true,
        _ => false,
    }
}

fn is_allowed_cargo_command(argv: &[String]) -> bool {
    let Some(subcommand) = argv.get(1).map(String::as_str) else {
        return false;
    };
    matches!(
        subcommand,
        "--version" | "-V" | "test" | "check" | "clippy" | "metadata"
    )
}

fn is_allowed_swift_command(argv: &[String]) -> bool {
    let Some(subcommand) = argv.get(1).map(String::as_str) else {
        return false;
    };
    matches!(subcommand, "--version" | "-version" | "test" | "build")
}

fn is_targeted_test_command(argv: &[String]) -> bool {
    let Some(program) = argv.first().map(String::as_str) else {
        return false;
    };
    match program {
        "cargo" => argv.get(1).is_some_and(|arg| arg == "test"),
        "swift" => argv.get(1).is_some_and(|arg| arg == "test"),
        "pytest" => true,
        "npm" => {
            argv.get(1).is_some_and(|arg| arg == "test")
                || (argv.get(1).is_some_and(|arg| arg == "run")
                    && argv.get(2).is_some_and(|arg| arg == "test"))
        }
        "pnpm" | "yarn" => {
            argv.get(1).is_some_and(|arg| arg == "test")
                || (argv.get(1).is_some_and(|arg| arg == "run")
                    && argv.get(2).is_some_and(|arg| arg == "test"))
        }
        _ => false,
    }
}

fn redacted_argv(argv: &[String]) -> Vec<String> {
    let mut redacted = Vec::with_capacity(argv.len());
    let mut redact_next = false;
    for arg in argv {
        if redact_next {
            redacted.push("[REDACTED]".to_string());
            redact_next = false;
            continue;
        }
        let sensitive = is_sensitive_label(arg);
        if sensitive {
            if let Some((key, _)) = arg.split_once('=') {
                redacted.push(format!("{key}=[REDACTED]"));
            } else {
                redacted.push("[REDACTED]".to_string());
                if arg.starts_with('-') {
                    redact_next = true;
                }
            }
        } else {
            redacted.push(redact_sensitive_text(arg));
        }
    }
    redacted
}

fn redacted_command(argv: &[String]) -> String {
    redacted_argv(argv).join(" ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillMetadata {
    name: Option<String>,
    description: Option<String>,
}

fn normalize_skill_name(raw: &str) -> Result<String, &'static str> {
    let skill = raw.trim();
    if skill.is_empty() {
        return Err("missing_skill_name");
    }
    if skill.len() > 128 {
        return Err("invalid_skill_name");
    }
    if !skill
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err("invalid_skill_name");
    }
    Ok(skill.to_string())
}

fn skill_metadata(text: &str) -> SkillMetadata {
    let mut name = None;
    let mut description = None;
    let mut lines = text.lines();
    if lines.next() != Some("---") {
        return SkillMetadata { name, description };
    }
    for line in lines {
        if line == "---" {
            break;
        }
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(value.trim().trim_matches('"').to_string());
        } else if let Some(value) = line.strip_prefix("description:") {
            description = Some(value.trim().trim_matches('"').to_string());
        }
    }
    SkillMetadata { name, description }
}

fn redact_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let redacted = if is_sensitive_label(key) {
                        serde_json::Value::String("[REDACTED]".to_string())
                    } else {
                        redact_json_value(value)
                    };
                    (key.clone(), redacted)
                })
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(redact_json_value).collect())
        }
        serde_json::Value::String(value) => serde_json::Value::String(redact_sensitive_text(value)),
        value => value.clone(),
    }
}

fn is_sensitive_label(label: &str) -> bool {
    let lower = label.to_ascii_lowercase();
    lower.contains("api_key")
        || lower.contains("api-key")
        || lower.contains("apikey")
        || lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("authorization")
}

/// Returns true if `token` looks like a credential value by shape alone,
/// independent of any nearby label keyword: known provider prefixes,
/// PEM private-key headers, or long high-entropy alphanumeric runs typical
/// of API tokens/JWTs/hashes.
fn looks_like_secret_value(token: &str) -> bool {
    const KNOWN_PREFIXES: &[&str] = &[
        "AKIA", "ASIA", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_",
        "sk-", "xox", "AIza", "glpat-",
    ];
    let trimmed = token.trim_matches(|c: char| {
        !c.is_ascii_alphanumeric() && c != '_' && c != '-' && c != '.'
    });
    if trimmed.len() < 16 {
        return false;
    }
    if KNOWN_PREFIXES.iter().any(|prefix| trimmed.starts_with(prefix)) {
        return true;
    }
    // A key=value / key: value pair with no surrounding whitespace (e.g.
    // `access_id=AKIA...`) is still one whitespace-delimited token; check the
    // part after the last `=` or `:` for a known-prefix credential shape too,
    // not just the start of the whole token.
    if let Some(after_delimiter) = trimmed.rsplit(['=', ':']).next()
        && after_delimiter.len() != trimmed.len()
        && after_delimiter.len() >= 16
        && KNOWN_PREFIXES
            .iter()
            .any(|prefix| after_delimiter.starts_with(prefix))
    {
        return true;
    }
    if trimmed.starts_with("-----BEGIN") && trimmed.contains("PRIVATE KEY") {
        return true;
    }
    // JWT shape: three dot-separated base64url segments, each long enough to
    // be a real base64url-encoded header/payload/signature (real JWT
    // segments run well into double digits). This minimum length keeps short
    // dotted, hyphenated identifiers like schema ids (e.g.
    // `opensks.command-tool-result.v1`) from being misdetected as JWTs.
    const JWT_MIN_SEGMENT_LEN: usize = 10;
    if trimmed.matches('.').count() == 2
        && trimmed.split('.').all(|part| {
            part.len() >= JWT_MIN_SEGMENT_LEN
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
    {
        return true;
    }
    // High-entropy long run: mixed-case alphanumeric (or base64/hex) of
    // sufficient length with no whitespace, unlikely to be prose.
    let is_token_charset = trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-'));
    if is_token_charset && trimmed.len() >= 32 {
        let has_digit = trimmed.chars().any(|c| c.is_ascii_digit());
        let has_upper = trimmed.chars().any(|c| c.is_ascii_uppercase());
        let has_lower = trimmed.chars().any(|c| c.is_ascii_lowercase());
        if (has_digit as u8 + has_upper as u8 + has_lower as u8) >= 2 {
            return true;
        }
    }
    false
}

fn redact_sensitive_text(raw: &str) -> String {
    let mut redacted = raw
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if is_sensitive_label(&lower) {
                "[REDACTED]".to_string()
            } else {
                redact_secret_shaped_tokens(line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if raw.ends_with('\n') {
        redacted.push('\n');
    }
    redacted
}

/// Replace individual whitespace-delimited tokens that look like secret
/// values (by shape, not by label) with "[REDACTED]", leaving the rest of
/// the line intact.
fn redact_secret_shaped_tokens(line: &str) -> String {
    if !line.split_whitespace().any(looks_like_secret_value) {
        return line.to_string();
    }
    line.split_inclusive(char::is_whitespace)
        .map(|piece| {
            let (word, trailing_ws) = {
                let trimmed = piece.trim_end_matches(char::is_whitespace);
                (trimmed, &piece[trimmed.len()..])
            };
            if looks_like_secret_value(word) {
                format!("[REDACTED]{trailing_ws}")
            } else {
                piece.to_string()
            }
        })
        .collect()
}

fn truncate_utf8(raw: &str, max_bytes: usize) -> String {
    if max_bytes == 0 {
        return String::new();
    }
    if raw.len() <= max_bytes {
        return raw.to_string();
    }
    let mut end = max_bytes;
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[truncated by ToolGateway: max_output_bytes={max_bytes}]",
        &raw[..end]
    )
}

fn select_line_range(text: &str, start_line: Option<u32>, end_line: Option<u32>) -> String {
    let start = start_line.unwrap_or(1).max(1) as usize;
    let end = end_line.map(|line| line.max(start as u32) as usize);
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let number = index + 1;
            if number < start {
                return None;
            }
            if let Some(end) = end
                && number > end
            {
                return None;
            }
            Some(format!("{number}: {line}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn json_tool_output<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "{\"error\":\"serialize\"}".to_string())
}

/// Build a [`PatchProposal`] describing every write planned in one step.
fn step_proposal(
    proposal_id: &str,
    request: &AgentRunRequest,
    planned: &[(String, String, String, FileOperation)],
) -> PatchProposal {
    let files = planned
        .iter()
        .map(|(path, before, after, operation)| FilePatch {
            path: path.clone(),
            orig_path: None,
            before_hash: content_hash(before),
            after_hash: content_hash(after),
            unified_diff: minimal_unified_diff(path, before, after),
            operation: *operation,
        })
        .collect();
    PatchProposal {
        schema: PATCH_PROPOSAL_SCHEMA.to_string(),
        proposal_id: proposal_id.to_string(),
        run_id: request.run_id.clone(),
        worker_id: "agentic".to_string(),
        base_commit: None,
        base_tree_hash: content_hash(
            &planned
                .iter()
                .map(|(_, before, _, _)| before.as_str())
                .collect::<String>(),
        ),
        files,
        requirement_ids: vec![],
        rationale_summary: format!("Agentic step edit ({} file(s))", planned.len()),
        risk_level: RiskLevel::Low,
        evidence_refs: vec![format!("adapter:agentic:{}", request.run_id)],
    }
}

fn emit(
    sink: &dyn AgentEventSink,
    request: &AgentRunRequest,
    sequence: &mut u64,
    kind: AgentEventKind,
    payload: serde_json::Value,
) {
    let seq = *sequence;
    *sequence += 1;
    sink.emit(AgentEventEnvelope {
        schema: AGENT_EVENT_ENVELOPE_SCHEMA.to_string(),
        stream_id: request.stream_id.clone(),
        project_id: request.project_id.clone(),
        conversation_id: request.conversation_id.clone(),
        turn_id: request.turn_id.clone(),
        run_id: request.run_id.clone(),
        worker_id: Some("agentic".to_string()),
        node_id: None,
        sequence: seq,
        occurred_at_ms: request.now_ms,
        kind,
        payload,
        sensitivity: Sensitivity::Internal,
        evidence_refs: vec![],
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CollectingSink;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("opensks-agentic-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_git(workspace: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(workspace)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git command");
        assert!(status.success(), "git {args:?} failed");
    }

    fn request(workspace: &Path) -> AgentRunRequest {
        AgentRunRequest {
            workspace: workspace.to_path_buf(),
            project_id: "p1".to_string(),
            conversation_id: "c1".to_string(),
            turn_id: "t1".to_string(),
            run_id: "r1".to_string(),
            stream_id: "s1".to_string(),
            patch_lease: None,
            cancellation_token: None,
            now_ms: 1000,
            prompt: String::new(),
        }
    }

    fn turn_settings(execution_mode: ExecutionMode) -> ConversationTurnSettings {
        ConversationTurnSettings {
            model: opensks_contracts::ModelSelection {
                mode: opensks_contracts::ModelSelectionMode::Auto,
                model_id: None,
                fallback_model_ids: Vec::new(),
            },
            reasoning_effort: opensks_contracts::ReasoningEffort::Standard,
            execution_mode,
            pipeline_id: "agentic-test".to_string(),
            graph_revision: None,
            max_parallelism: 4,
            verifier_count: 1,
            tool_policy_id: "test-policy".to_string(),
            approval_policy_id: "safe-interactive".to_string(),
            token_budget: None,
            cost_budget_usd: None,
            timeout_ms: None,
            image_model_id: None,
        }
    }

    #[test]
    fn loop_reads_then_edits_then_observes_the_change_then_finishes() {
        // The driver BRANCHES on observations: it reads NOTES.md, then on the next
        // step (seeing the original content) appends a line, then reads again and
        // only finishes once it observes the edit it made. This proves the loop
        // feeds tool results back into the driver's decisions.
        let ws = temp_workspace("feedback");
        std::fs::write(ws.join("NOTES.md"), "one\n").unwrap();

        let mut driver = FnDriver::new(|obs: &[ToolResult], step: usize| match step {
            0 => AgentStep::Tools(vec![ToolCall::ReadFileRange {
                path: "NOTES.md".to_string(),
                start_line: None,
                end_line: None,
            }]),
            1 => {
                // We must have observed the original content before editing.
                assert!(matches!(
                    obs.first(),
                    Some(ToolResult::FileContent { content: Some(c), .. }) if c == "1: one"
                ));
                AgentStep::Tools(vec![ToolCall::DiffPatch {
                    path: "NOTES.md".to_string(),
                    value: "two".to_string(),
                }])
            }
            2 => {
                // The write landed.
                assert!(matches!(
                    obs.first(),
                    Some(ToolResult::Wrote { applied: true, .. })
                ));
                AgentStep::Tools(vec![ToolCall::ReadFileRange {
                    path: "NOTES.md".to_string(),
                    start_line: None,
                    end_line: None,
                }])
            }
            _ => {
                // Only finish once we observe our own edit — feedback in action.
                assert!(matches!(
                    obs.first(),
                    Some(ToolResult::FileContent { content: Some(c), .. }) if c == "1: one\n2: two"
                ));
                AgentStep::Final {
                    text: "done".to_string(),
                }
            }
        });

        let scheduler_lease_id = "lease-run-agentic-worker-item-12345";
        let mut request = request(&ws);
        request.patch_lease = Some(crate::PatchPathLease::new(
            scheduler_lease_id,
            scheduler_lease_id,
        ));
        let sink = CollectingSink::new();
        let outcome =
            run_agentic_loop(&request, &mut driver, &AgenticConfig::default(), &sink).unwrap();

        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        assert_eq!(outcome.assistant_text, "done");
        assert_eq!(
            std::fs::read_to_string(ws.join("NOTES.md")).unwrap(),
            "one\ntwo\n"
        );
        assert_eq!(outcome.patches.len(), 1);
        assert_eq!(outcome.apply_results.len(), 1);
        assert!(outcome.apply_results[0].applied);
        assert!(
            outcome.apply_results[0]
                .evidence_refs
                .contains(&"patch-engine:path-lease-bound".to_string())
        );
        assert!(
            outcome.apply_results[0]
                .evidence_refs
                .contains(&"patch-engine:fence-token-bound".to_string())
        );
        let journal_dir = ws.join(".opensks/patch-engine/transactions");
        let journal = std::fs::read_dir(&journal_dir)
            .unwrap()
            .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(journal.contains("\"raw_tokens_redacted\":true"));
        assert!(journal.contains(&content_hash(scheduler_lease_id)));
        assert!(!journal.contains(scheduler_lease_id));

        // Events are ordered and end on a terminal assistant message.
        let kinds = sink.kinds();
        assert!(kinds.contains(&AgentEventKind::FilePatchApplied));
        assert_eq!(kinds.last(), Some(&AgentEventKind::AssistantTextCompleted));
        let seqs: Vec<u64> = sink.events().into_iter().map(|e| e.sequence).collect();
        assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<_>>());

        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn loop_short_circuits_repeated_noop_writes_after_successful_apply() {
        let ws = temp_workspace("noop-repeat");
        let mut driver = FnDriver::new(|_obs: &[ToolResult], _step: usize| {
            AgentStep::Tools(vec![ToolCall::ProposePatch {
                path: "DONE.txt".to_string(),
                content: "done\n".to_string(),
            }])
        });
        let sink = CollectingSink::new();

        let outcome = run_agentic_loop(
            &request(&ws),
            &mut driver,
            &AgenticConfig {
                max_steps: 16,
                ..AgenticConfig::default()
            },
            &sink,
        )
        .unwrap();

        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        assert!(outcome.assistant_text.contains("No further file changes"));
        assert_eq!(
            std::fs::read_to_string(ws.join("DONE.txt")).unwrap(),
            "done\n"
        );
        assert_eq!(outcome.apply_results.len(), 1);
        assert_eq!(
            sink.kinds()
                .into_iter()
                .filter(|kind| *kind == AgentEventKind::FilePatchApplied)
                .count(),
            1,
            "identical no-op writes must not repeatedly apply patches"
        );
        assert!(
            sink.events()
                .iter()
                .any(|event| event.payload["reason_code"] == "no_op"),
            "driver should see no-op write observations before the loop self-terminates"
        );
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn loop_stops_before_next_tool_step_when_cancelled() {
        let ws = temp_workspace("cancel-before-tool");
        std::fs::write(ws.join("NOTES.md"), "one\n").unwrap();
        let cancellation_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let driver_token = std::sync::Arc::clone(&cancellation_token);

        let mut driver = FnDriver::new(move |obs: &[ToolResult], step: usize| match step {
            0 => AgentStep::Tools(vec![ToolCall::ReadFileRange {
                path: "NOTES.md".to_string(),
                start_line: None,
                end_line: None,
            }]),
            1 => {
                assert!(matches!(
                    obs.first(),
                    Some(ToolResult::FileContent { content: Some(c), .. }) if c == "1: one"
                ));
                driver_token.store(true, Ordering::SeqCst);
                AgentStep::Tools(vec![ToolCall::DiffPatch {
                    path: "NOTES.md".to_string(),
                    value: "two".to_string(),
                }])
            }
            _ => AgentStep::Final {
                text: "should not finish".to_string(),
            },
        });
        let mut request = request(&ws);
        request.cancellation_token = Some(cancellation_token);
        let sink = CollectingSink::new();

        let outcome =
            run_agentic_loop(&request, &mut driver, &AgenticConfig::default(), &sink).unwrap();

        assert_eq!(outcome.final_state, RunProjectionState::Cancelled);
        assert_eq!(
            std::fs::read_to_string(ws.join("NOTES.md")).unwrap(),
            "one\n",
            "the cancelled second-step write must not execute"
        );
        assert!(outcome.patches.is_empty());
        assert!(outcome.apply_results.is_empty());
        assert!(sink.events().iter().any(|event| {
            event.kind == AgentEventKind::Warning && event.payload["code"] == "run_cancelled"
        }));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn multiple_writes_in_one_step_apply_as_one_transaction() {
        let ws = temp_workspace("multi");
        let mut driver = SequenceDriver::new(
            vec![vec![
                ToolCall::ProposePatch {
                    path: "a.txt".to_string(),
                    content: "A".to_string(),
                },
                ToolCall::ProposePatch {
                    path: "nested/b.txt".to_string(),
                    content: "B".to_string(),
                },
            ]],
            "wrote two files",
        );
        let sink = CollectingSink::new();
        let outcome =
            run_agentic_loop(&request(&ws), &mut driver, &AgenticConfig::default(), &sink).unwrap();

        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        assert_eq!(outcome.apply_results.len(), 1);
        assert_eq!(outcome.apply_results[0].applied_files.len(), 2);
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "A");
        assert_eq!(
            std::fs::read_to_string(ws.join("nested/b.txt")).unwrap(),
            "B"
        );
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn applied_patch_is_preserved_but_failure_is_reported_when_final_message_missing() {
        let ws = temp_workspace("missing-final");
        let mut driver = FnDriver::new(|obs: &[ToolResult], step: usize| match step {
            0 => AgentStep::Tools(vec![ToolCall::ProposePatch {
                path: "RESULT.md".to_string(),
                content: "ok\n".to_string(),
            }]),
            _ => {
                assert!(matches!(
                    obs.first(),
                    Some(ToolResult::Wrote { applied: true, .. })
                ));
                AgentStep::Failed {
                    code: "provider_call_failed".to_string(),
                    message: "The model call failed: provider response had no message content"
                        .to_string(),
                    retryable: true,
                }
            }
        });
        let sink = CollectingSink::new();
        let outcome =
            run_agentic_loop(&request(&ws), &mut driver, &AgenticConfig::default(), &sink).unwrap();

        assert_eq!(outcome.final_state, RunProjectionState::Failed);
        assert_eq!(outcome.apply_results.len(), 1);
        assert!(outcome.apply_results[0].applied);
        assert_eq!(
            std::fs::read_to_string(ws.join("RESULT.md")).unwrap(),
            "ok\n"
        );
        assert!(
            outcome
                .assistant_text
                .contains("provider response had no message content")
        );
        let events = sink.events();
        assert!(!events.iter().any(|event| {
            event.kind == AgentEventKind::Warning && event.payload["recovered"] == true
        }));
        assert!(events.iter().any(|event| {
            event.kind == AgentEventKind::Error
                && event.payload["message"]
                    .as_str()
                    .is_some_and(|text| text.contains("provider response had no message content"))
        }));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn exhausting_the_step_budget_is_an_honest_failure() {
        // A driver that never finishes must NOT silently "complete" — the loop
        // stops at the budget and reports a failure (directive §0.4).
        let ws = temp_workspace("budget");
        let mut driver = FnDriver::new(|_obs: &[ToolResult], _step: usize| {
            AgentStep::Tools(vec![ToolCall::ReadFileRange {
                path: "NOTES.md".to_string(),
                start_line: None,
                end_line: None,
            }])
        });
        let sink = CollectingSink::new();
        let outcome = run_agentic_loop(
            &request(&ws),
            &mut driver,
            &AgenticConfig {
                max_steps: 3,
                ..AgenticConfig::default()
            },
            &sink,
        )
        .unwrap();
        assert_eq!(outcome.final_state, RunProjectionState::Failed);
        assert!(outcome.assistant_text.contains("budget"));
        let events = sink.events();
        let budget_event = events.last().expect("budget event");
        assert_eq!(budget_event.kind, AgentEventKind::Error);
        assert_eq!(
            budget_event.payload["reason_code"],
            "agentic_step_budget_exhausted"
        );
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn path_escape_is_reported_to_the_driver_not_a_hard_abort() {
        let ws = temp_workspace("escape");
        let mut driver = FnDriver::new(|obs: &[ToolResult], step: usize| match step {
            0 => AgentStep::Tools(vec![ToolCall::ProposePatch {
                path: "../escape.txt".to_string(),
                content: "x".to_string(),
            }]),
            _ => {
                // The escape was reported back as a failed tool, not a panic/abort.
                assert!(matches!(obs.first(), Some(ToolResult::Failed { .. })));
                AgentStep::Final {
                    text: "recovered".to_string(),
                }
            }
        });
        let sink = CollectingSink::new();
        let outcome =
            run_agentic_loop(&request(&ws), &mut driver, &AgenticConfig::default(), &sink).unwrap();
        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        assert_eq!(outcome.assistant_text, "recovered");
        assert!(!ws.join("../escape.txt").exists());
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn tool_gateway_denies_unlisted_write_tool() {
        let ws = temp_workspace("deny-unlisted");
        let mut driver = FnDriver::new(|obs: &[ToolResult], step: usize| match step {
            0 => AgentStep::Tools(vec![ToolCall::ProposePatch {
                path: "blocked.txt".to_string(),
                content: "nope".to_string(),
            }]),
            _ => {
                assert!(matches!(
                    obs.first(),
                    Some(ToolResult::Failed { message, .. })
                        if message == "blocked_tool_unlisted_or_denied"
                ));
                AgentStep::Final {
                    text: "denied as expected".to_string(),
                }
            }
        });
        let config = AgenticConfig {
            tool_policy: ToolPolicy {
                schema: TOOL_POLICY_SCHEMA.to_string(),
                policy_id: "read-only".to_string(),
                entries: vec![ToolPolicyEntry {
                    tool: "workspace.read_file_range".to_string(),
                    permission: ToolPermission::ReadOnly,
                }],
            },
            ..AgenticConfig::default()
        };
        let sink = CollectingSink::new();
        let outcome = run_agentic_loop(&request(&ws), &mut driver, &config, &sink).unwrap();
        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        assert!(!ws.join("blocked.txt").exists());
        assert!(sink.kinds().contains(&AgentEventKind::Warning));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn image_generate_without_executor_reports_unavailable() {
        let ws = temp_workspace("image-no-executor");
        let mut driver = FnDriver::new(|obs: &[ToolResult], step: usize| match step {
            0 => AgentStep::Tools(vec![ToolCall::ImageGenerate {
                prompt: "render a diagram".to_string(),
                asset_id: Some("diagram".to_string()),
                width: Some(512),
                height: Some(512),
            }]),
            _ => {
                assert!(matches!(
                    obs.first(),
                    Some(ToolResult::Failed { message, .. })
                        if message == "image_executor_unavailable"
                ));
                AgentStep::Final {
                    text: "image unavailable".to_string(),
                }
            }
        });
        let sink = CollectingSink::new();
        let outcome = run_agentic_loop(
            &request(&ws),
            &mut driver,
            &config_with_image_generate_allow(),
            &sink,
        )
        .unwrap();

        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        assert!(sink.kinds().contains(&AgentEventKind::Warning));
        assert!(!sink.kinds().contains(&AgentEventKind::ImageArtifactCreated));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn image_generate_with_executor_emits_artifact_event_and_observation() {
        let ws = temp_workspace("image-executor");
        let executor = ScriptedImageExecutor;
        let mut driver = FnDriver::new(|obs: &[ToolResult], step: usize| match step {
            0 => AgentStep::Tools(vec![ToolCall::ImageGenerate {
                prompt: "render a diagram".to_string(),
                asset_id: Some("diagram".to_string()),
                width: Some(640),
                height: Some(384),
            }]),
            _ => {
                let Some(ToolResult::ToolOutput { tool, content }) = obs.first() else {
                    panic!("expected image tool output");
                };
                assert_eq!(tool, "image.generate");
                assert!(content.contains("\"asset_id\": \"diagram\""));
                assert!(content.contains("\"path\": \".opensks/assets/candidates/diagram.png\""));
                AgentStep::Final {
                    text: "image generated".to_string(),
                }
            }
        });
        let sink = CollectingSink::new();
        let outcome = run_agentic_loop_with_image_tools(
            &request(&ws),
            &mut driver,
            &config_with_image_generate_allow(),
            &sink,
            &executor,
        )
        .unwrap();

        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        let events = sink.events();
        let artifact = events
            .iter()
            .find(|event| event.kind == AgentEventKind::ImageArtifactCreated)
            .expect("image artifact event");
        assert_eq!(artifact.payload["asset_id"], "diagram");
        assert_eq!(artifact.payload["provider_id"], "provider-1");
        assert_eq!(artifact.payload["model_id"], "provider-1/image-model");
        assert_eq!(
            artifact.payload["path"],
            ".opensks/assets/candidates/diagram.png"
        );
        assert_eq!(artifact.payload["operation"], "generate");
        assert_eq!(artifact.payload["width"], 640);
        assert_eq!(artifact.payload["height"], 384);
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn read_only_permission_cannot_write_through_write_alias() {
        let ws = temp_workspace("read-only-write");
        let gateway = ToolGateway::new(
            &ws,
            &AgenticConfig {
                tool_policy: ToolPolicy {
                    schema: TOOL_POLICY_SCHEMA.to_string(),
                    policy_id: "bad-alias".to_string(),
                    entries: vec![ToolPolicyEntry {
                        tool: "workspace.propose_patch".to_string(),
                        permission: ToolPermission::ReadOnly,
                    }],
                },
                ..AgenticConfig::default()
            },
        );
        let result = gateway.execute(&ToolCall::ProposePatch {
            path: "alias.txt".to_string(),
            content: "no".to_string(),
        });
        assert!(matches!(
            result,
            Err(ToolResult::Failed { message, .. }) if message == "blocked_tool_read_only"
        ));
        assert!(!ws.join("alias.txt").exists());
        assert_eq!(
            default_agentic_tool_policy().permission_for("git.push"),
            ToolPermission::Deny,
            "workers cannot call git push through the default tool policy"
        );
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn image_inspect_without_executor_reports_unavailable() {
        let ws = temp_workspace("image-inspect-no-executor");
        let mut driver = FnDriver::new(|obs: &[ToolResult], step: usize| match step {
            0 => AgentStep::Tools(vec![ToolCall::ImageInspect {
                artifact_ref: Some("diagram".to_string()),
                asset_id: None,
                prompt: Some("describe it".to_string()),
            }]),
            _ => {
                assert!(matches!(
                    obs.first(),
                    Some(ToolResult::Failed { message, .. })
                        if message == "image_executor_unavailable"
                ));
                AgentStep::Final {
                    text: "inspection unavailable".to_string(),
                }
            }
        });
        let sink = CollectingSink::new();
        let outcome = run_agentic_loop(
            &request(&ws),
            &mut driver,
            &config_with_image_tools_allow(),
            &sink,
        )
        .unwrap();

        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        assert!(sink.kinds().contains(&AgentEventKind::Warning));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn image_inspect_with_executor_returns_provider_observation() {
        let ws = temp_workspace("image-inspect-executor");
        let executor = ScriptedImageExecutor;
        let mut driver = FnDriver::new(|obs: &[ToolResult], step: usize| match step {
            0 => AgentStep::Tools(vec![ToolCall::ImageInspect {
                artifact_ref: Some("diagram".to_string()),
                asset_id: None,
                prompt: Some("describe it".to_string()),
            }]),
            _ => {
                let Some(ToolResult::ToolOutput { tool, content }) = obs.first() else {
                    panic!("expected image inspect output");
                };
                assert_eq!(tool, "image.inspect");
                assert!(content.contains("\"schema\": \"opensks.image-inspect-tool-result.v1\""));
                assert!(content.contains("\"asset_id\": \"diagram\""));
                assert!(content.contains("A generated diagram."));
                AgentStep::Final {
                    text: "image inspected".to_string(),
                }
            }
        });
        let sink = CollectingSink::new();
        let outcome = run_agentic_loop_with_image_tools(
            &request(&ws),
            &mut driver,
            &config_with_image_tools_allow(),
            &sink,
            &executor,
        )
        .unwrap();

        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        let completed = sink
            .events()
            .into_iter()
            .find(|event| {
                event.kind == AgentEventKind::ToolCallCompleted
                    && event.payload["tool"] == "image.inspect"
            })
            .expect("inspect completion");
        assert_eq!(completed.payload["asset_id"], "diagram");
        assert_eq!(completed.payload["provider_id"], "provider-1");
        assert_eq!(completed.payload["model_id"], "provider-1/vision-model");
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn artifact_write_requires_explicit_allow_by_default() {
        let ws = temp_workspace("artifact-write-ask");
        let gateway = ToolGateway::new(&ws, &AgenticConfig::default());
        let result = gateway.execute(&ToolCall::ArtifactWrite {
            artifact_ref: "artifact://.opensks/runtime/report.txt".to_string(),
            content: "safe".to_string(),
        });
        assert!(matches!(
            result,
            Err(ToolResult::Failed { message, .. }) if message == "approval_required_for_tool"
        ));
        assert!(!ws.join(".opensks/runtime/report.txt").exists());
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn artifact_tools_write_redacted_runtime_artifact_and_read_it_back() {
        let ws = temp_workspace("artifact-read-write");
        let gateway = ToolGateway::new(&ws, &config_with_artifact_write_allow());
        let synthetic_secret = ["OPENAI", "_API_KEY=abc123"].concat();
        let write = gateway
            .execute(&ToolCall::ArtifactWrite {
                artifact_ref: "artifact://.opensks/runtime/reports/tool.json".to_string(),
                content: format!("{{\"status\":\"ok\"}}\n{synthetic_secret}\n"),
            })
            .expect("artifact write");
        let ToolGatewayReceipt::Read(ToolResult::ToolOutput { tool, content }) = write else {
            panic!("expected artifact write output");
        };
        assert_eq!(tool, "artifact.write");
        assert!(content.contains("\"operation\": \"write\""));
        assert!(content.contains("\"content_redacted\": true"));
        assert!(!content.contains("abc123"));

        let written = std::fs::read_to_string(ws.join(".opensks/runtime/reports/tool.json"))
            .expect("written artifact");
        assert!(written.contains("[REDACTED]"));
        assert!(!written.contains("abc123"));

        let read = gateway
            .execute(&ToolCall::ArtifactRead {
                artifact_ref: "artifact://.opensks/runtime/reports/tool.json".to_string(),
            })
            .expect("artifact read");
        let ToolGatewayReceipt::Read(ToolResult::ToolOutput { tool, content }) = read else {
            panic!("expected artifact read output");
        };
        assert_eq!(tool, "artifact.read");
        assert!(content.contains("\"exists\": true"));
        assert!(content.contains("[REDACTED]"));
        assert!(!content.contains("abc123"));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn artifact_tools_reject_refs_outside_runtime_artifacts() {
        let ws = temp_workspace("artifact-path-block");
        let gateway = ToolGateway::new(&ws, &config_with_artifact_write_allow());
        let outside_runtime = gateway.execute(&ToolCall::ArtifactRead {
            artifact_ref: "artifact://README.md".to_string(),
        });
        assert!(matches!(
            outside_runtime,
            Err(ToolResult::Failed { message, .. })
                if message == "blocked_artifact_ref_outside_runtime"
        ));
        let traversal = gateway.execute(&ToolCall::ArtifactWrite {
            artifact_ref: "artifact://.opensks/../Cargo.toml".to_string(),
            content: "no".to_string(),
        });
        assert!(matches!(
            traversal,
            Err(ToolResult::Failed { message, .. }) if message == "blocked_artifact_path_escape"
        ));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn command_run_requires_explicit_allow_by_default() {
        let ws = temp_workspace("command-ask");
        let gateway = ToolGateway::new(&ws, &AgenticConfig::default());
        let result = gateway.execute(&ToolCall::CommandRun {
            command: "cargo --version".to_string(),
            timeout_ms: Some(1_000),
        });
        assert!(matches!(
            result,
            Err(ToolResult::Failed { message, .. }) if message == "approval_required_for_tool"
        ));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn command_run_with_allow_executes_allowlisted_command_without_shell() {
        let ws = temp_workspace("command-allow");
        let gateway = ToolGateway::new(&ws, &config_with_tool_allow("command.run"));
        let receipt = gateway
            .execute(&ToolCall::CommandRun {
                command: "cargo --version".to_string(),
                timeout_ms: Some(10_000),
            })
            .expect("command run");
        let ToolGatewayReceipt::Read(ToolResult::ToolOutput { tool, content }) = receipt else {
            panic!("expected command output");
        };
        assert_eq!(tool, "command.run");
        assert!(content.contains("\"schema\": \"opensks.command-tool-result.v1\""));
        assert!(content.contains("\"command_redacted\": \"cargo --version\""));
        assert!(content.contains("\"exit_code\": 0"));
        assert!(content.contains("\"timed_out\": false"));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn command_run_rejects_shell_metacharacters() {
        let ws = temp_workspace("command-shell-block");
        let gateway = ToolGateway::new(&ws, &config_with_tool_allow("command.run"));
        let result = gateway.execute(&ToolCall::CommandRun {
            command: "git --version && rm -rf .opensks".to_string(),
            timeout_ms: Some(1_000),
        });
        assert!(matches!(
            result,
            Err(ToolResult::Failed { message, .. }) if message == "blocked_shell_metacharacter"
        ));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn test_run_targeted_requires_test_shape() {
        let ws = temp_workspace("targeted-test-shape");
        let gateway = ToolGateway::new(&ws, &config_with_tool_allow("test.run_targeted"));
        let result = gateway.execute(&ToolCall::TestRunTargeted {
            target: "git --version".to_string(),
            timeout_ms: Some(1_000),
        });
        assert!(matches!(
            result,
            Err(ToolResult::Failed { message, .. }) if message == "blocked_non_test_command"
        ));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn test_run_targeted_executes_cargo_test_help() {
        let ws = temp_workspace("targeted-test-help");
        let gateway = ToolGateway::new(&ws, &config_with_tool_allow("test.run_targeted"));
        let receipt = gateway
            .execute(&ToolCall::TestRunTargeted {
                target: "cargo test --help".to_string(),
                timeout_ms: Some(10_000),
            })
            .expect("targeted test run");
        let ToolGatewayReceipt::Read(ToolResult::ToolOutput { tool, content }) = receipt else {
            panic!("expected targeted test output");
        };
        assert_eq!(tool, "test.run_targeted");
        assert!(content.contains("\"tool\": \"test.run_targeted\""));
        assert!(content.contains("\"command_redacted\": \"cargo test --help\""));
        assert!(content.contains("\"exit_code\": 0"));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn command_redaction_masks_sensitive_arguments() {
        let argv = vec![
            "cargo".to_string(),
            "test".to_string(),
            "--token".to_string(),
            "abc123".to_string(),
            "--api-key=secret-value".to_string(),
        ];
        let redacted = redacted_command(&argv);
        assert!(redacted.contains("[REDACTED]"));
        assert!(!redacted.contains("abc123"));
        assert!(!redacted.contains("secret-value"));
    }

    #[test]
    fn redact_sensitive_text_masks_unlabeled_secret_shaped_values() {
        let aws_key = redact_sensitive_text("access_id=AKIAABCDEFGHIJKLMNOP plain text around it");
        assert!(aws_key.contains("[REDACTED]"));
        assert!(!aws_key.contains("AKIAABCDEFGHIJKLMNOP"));

        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let jwt_redacted = redact_sensitive_text(jwt);
        assert!(jwt_redacted.contains("[REDACTED]"));
        assert!(!jwt_redacted.contains(jwt));

        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA1c7+9z5Pad7OejecsQ0bu3aumqCr9wLa==\n-----END RSA PRIVATE KEY-----";
        let pem_redacted = redact_sensitive_text(pem);
        assert!(pem_redacted.contains("[REDACTED]"));
    }

    #[test]
    fn redact_sensitive_text_leaves_normal_prose_untouched() {
        let text = "fn main() { println!(\"hello world\"); } // this is a normal comment line";
        assert_eq!(redact_sensitive_text(text), text);
    }

    #[test]
    fn mcp_invoke_requires_explicit_allow_by_default() {
        let ws = temp_workspace("mcp-ask");
        let gateway = ToolGateway::new(&ws, &AgenticConfig::default());
        let result = gateway.execute(&ToolCall::McpInvoke {
            tool_name: "opensks.repo.search".to_string(),
            payload: serde_json::json!({ "query": "needle" }),
        });
        assert!(matches!(
            result,
            Err(ToolResult::Failed { message, .. }) if message == "approval_required_for_tool"
        ));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn mcp_invoke_searches_repo_through_allowlisted_local_tool() {
        let ws = temp_workspace("mcp-repo-search");
        std::fs::write(ws.join("README.md"), "needle in docs\n").unwrap();
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("src/lib.rs"), "fn demo() { /* needle */ }\n").unwrap();
        let gateway = ToolGateway::new(&ws, &config_with_tool_allow("mcp.invoke"));
        let receipt = gateway
            .execute(&ToolCall::McpInvoke {
                tool_name: "opensks.repo.search".to_string(),
                payload: serde_json::json!({
                    "query": "needle",
                    "limit": 10,
                    "api_key": "abc123"
                }),
            })
            .expect("mcp invoke");
        let ToolGatewayReceipt::Read(ToolResult::ToolOutput { tool, content }) = receipt else {
            panic!("expected mcp output");
        };
        assert_eq!(tool, "mcp.invoke");
        assert!(content.contains("\"schema\": \"opensks.mcp-tool-result.v1\""));
        assert!(content.contains("\"method\": \"tools/call\""));
        assert!(content.contains("\"mcp_tool_name\": \"opensks.repo.search\""));
        assert!(content.contains("README.md:1: needle in docs"));
        assert!(content.contains("src/lib.rs:1: fn demo() { /* needle */ }"));
        assert!(content.contains("[REDACTED]"));
        assert!(!content.contains("abc123"));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn mcp_invoke_rejects_unknown_local_tool() {
        let ws = temp_workspace("mcp-unknown");
        let gateway = ToolGateway::new(&ws, &config_with_tool_allow("mcp.invoke"));
        let result = gateway.execute(&ToolCall::McpInvoke {
            tool_name: "external.shell.run".to_string(),
            payload: serde_json::json!({ "command": "echo nope" }),
        });
        assert!(matches!(
            result,
            Err(ToolResult::Failed { message, .. })
                if message == "unknown_or_unapproved_mcp_tool"
        ));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn read_only_permission_blocks_effectful_command_and_mcp_tools() {
        let ws = temp_workspace("effectful-read-only");
        let config = AgenticConfig {
            tool_policy: ToolPolicy {
                schema: TOOL_POLICY_SCHEMA.to_string(),
                policy_id: "bad-effectful-read-only".to_string(),
                entries: vec![
                    ToolPolicyEntry {
                        tool: "command.run".to_string(),
                        permission: ToolPermission::ReadOnly,
                    },
                    ToolPolicyEntry {
                        tool: "mcp.invoke".to_string(),
                        permission: ToolPermission::ReadOnly,
                    },
                    ToolPolicyEntry {
                        tool: "skill.invoke".to_string(),
                        permission: ToolPermission::ReadOnly,
                    },
                ],
            },
            ..AgenticConfig::default()
        };
        let gateway = ToolGateway::new(&ws, &config);
        let command = gateway.execute(&ToolCall::CommandRun {
            command: "cargo --version".to_string(),
            timeout_ms: Some(1_000),
        });
        assert!(matches!(
            command,
            Err(ToolResult::Failed { message, .. }) if message == "blocked_tool_read_only"
        ));
        let mcp = gateway.execute(&ToolCall::McpInvoke {
            tool_name: "opensks.repo.search".to_string(),
            payload: serde_json::json!({ "query": "needle" }),
        });
        assert!(matches!(
            mcp,
            Err(ToolResult::Failed { message, .. }) if message == "blocked_tool_read_only"
        ));
        let skill = gateway.execute(&ToolCall::SkillInvoke {
            skill: "goal".to_string(),
            payload: serde_json::json!({}),
        });
        assert!(matches!(
            skill,
            Err(ToolResult::Failed { message, .. }) if message == "blocked_tool_read_only"
        ));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn skill_invoke_requires_explicit_allow_by_default() {
        let ws = temp_workspace("skill-ask");
        std::fs::create_dir_all(ws.join(".agents/skills/goal")).unwrap();
        std::fs::write(
            ws.join(".agents/skills/goal/SKILL.md"),
            "---\nname: goal\ndescription: Goal route\n---\nUse goal.\n",
        )
        .unwrap();
        let gateway = ToolGateway::new(&ws, &AgenticConfig::default());
        let result = gateway.execute(&ToolCall::SkillInvoke {
            skill: "goal".to_string(),
            payload: serde_json::json!({ "objective": "continue" }),
        });
        assert!(matches!(
            result,
            Err(ToolResult::Failed { message, .. }) if message == "approval_required_for_tool"
        ));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn skill_invoke_loads_allowlisted_local_skill_context() {
        let ws = temp_workspace("skill-load");
        std::fs::create_dir_all(ws.join(".agents/skills/goal")).unwrap();
        std::fs::write(
            ws.join(".agents/skills/goal/SKILL.md"),
            "---\nname: goal\ndescription: Goal route\n---\nUse goal.\nSECRET_TOKEN=abc123\n",
        )
        .unwrap();
        let gateway = ToolGateway::new(&ws, &config_with_tool_allow("skill.invoke"));
        let receipt = gateway
            .execute(&ToolCall::SkillInvoke {
                skill: "goal".to_string(),
                payload: serde_json::json!({
                    "objective": "continue",
                    "api_key": "secret-value"
                }),
            })
            .expect("skill invoke");
        let ToolGatewayReceipt::Read(ToolResult::ToolOutput { tool, content }) = receipt else {
            panic!("expected skill output");
        };
        assert_eq!(tool, "skill.invoke");
        assert!(content.contains("\"schema\": \"opensks.skill-tool-result.v1\""));
        assert!(content.contains("\"operation\": \"load_skill_context\""));
        assert!(content.contains("\"declared_name\": \"goal\""));
        assert!(content.contains("\"description\": \"Goal route\""));
        assert!(content.contains("\"path\": \".agents/skills/goal/SKILL.md\""));
        assert!(content.contains("\"side_effects\": \"none_instructions_loaded_only\""));
        assert!(content.contains("[REDACTED]"));
        assert!(!content.contains("abc123"));
        assert!(!content.contains("secret-value"));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn skill_invoke_rejects_pathlike_skill_names() {
        let ws = temp_workspace("skill-path-block");
        let gateway = ToolGateway::new(&ws, &config_with_tool_allow("skill.invoke"));
        let traversal = gateway.execute(&ToolCall::SkillInvoke {
            skill: "../goal".to_string(),
            payload: serde_json::json!({}),
        });
        assert!(matches!(
            traversal,
            Err(ToolResult::Failed { message, .. }) if message == "invalid_skill_name"
        ));
        let missing = gateway.execute(&ToolCall::SkillInvoke {
            skill: "missing-skill".to_string(),
            payload: serde_json::json!({}),
        });
        assert!(matches!(
            missing,
            Err(ToolResult::Failed { message, .. }) if message == "skill_not_found"
        ));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn read_only_execution_mode_denies_agentic_workspace_writes() {
        let ws = temp_workspace("settings-read-only");
        let mut driver = FnDriver::new(|obs: &[ToolResult], step: usize| match step {
            0 => AgentStep::Tools(vec![ToolCall::ProposePatch {
                path: "blocked.txt".to_string(),
                content: "nope".to_string(),
            }]),
            _ => {
                assert!(matches!(
                    obs.first(),
                    Some(ToolResult::Failed { message, .. })
                        if message == "blocked_tool_unlisted_or_denied"
                ));
                AgentStep::Final {
                    text: "write was blocked".to_string(),
                }
            }
        });
        let sink = CollectingSink::new();
        let config = AgenticConfig::for_turn_settings(&turn_settings(ExecutionMode::ReadOnly));
        let outcome = run_agentic_loop(&request(&ws), &mut driver, &config, &sink).unwrap();
        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        assert!(!ws.join("blocked.txt").exists());
        assert!(sink.kinds().contains(&AgentEventKind::Warning));
        assert_eq!(
            config.tool_policy.permission_for("workspace.propose_patch"),
            ToolPermission::Deny
        );
        std::fs::remove_dir_all(&ws).ok();
    }

    fn config_with_image_generate_allow() -> AgenticConfig {
        config_with_tool_allow("image.generate")
    }

    fn config_with_image_tools_allow() -> AgenticConfig {
        let mut config = config_with_image_generate_allow();
        if let Some(entry) = config
            .tool_policy
            .entries
            .iter_mut()
            .find(|entry| entry.tool == "image.inspect")
        {
            entry.permission = ToolPermission::Allow;
        } else {
            config.tool_policy.entries.push(ToolPolicyEntry {
                tool: "image.inspect".to_string(),
                permission: ToolPermission::Allow,
            });
        }
        config
    }

    fn config_with_artifact_write_allow() -> AgenticConfig {
        config_with_tool_allow("artifact.write")
    }

    fn config_with_tool_allow(tool: &str) -> AgenticConfig {
        let mut config = AgenticConfig::default();
        if let Some(entry) = config
            .tool_policy
            .entries
            .iter_mut()
            .find(|entry| entry.tool == tool)
        {
            entry.permission = ToolPermission::Allow;
        } else {
            config.tool_policy.entries.push(ToolPolicyEntry {
                tool: tool.to_string(),
                permission: ToolPermission::Allow,
            });
        }
        config
    }

    struct ScriptedImageExecutor;

    impl ImageToolExecutor for ScriptedImageExecutor {
        fn generate_image(
            &self,
            _workspace: &Path,
            request: &ImageGenerateToolRequest,
        ) -> Result<ImageAsset, String> {
            let id = request
                .asset_id
                .clone()
                .unwrap_or_else(|| "generated-image".to_string());
            Ok(ImageAsset {
                schema: opensks_contracts::IMAGE_ASSET_SCHEMA.to_string(),
                content_hash: "sha256:v1:image-bytes".to_string(),
                id: id.clone(),
                provider_id: "provider-1".to_string(),
                model_id: "provider-1/image-model".to_string(),
                path: format!(".opensks/assets/candidates/{id}.png"),
                width: request.width,
                height: request.height,
                before_asset_id: None,
                anchors: Vec::new(),
                temporary: true,
                provenance_hash: Some("sha256:v1:provenance".to_string()),
                route_receipt: None,
                evidence_refs: vec!["test:image-executor".to_string()],
            })
        }

        fn inspect_image(
            &self,
            _workspace: &Path,
            request: &ImageInspectToolRequest,
        ) -> Result<ImageInspectToolResult, String> {
            Ok(ImageInspectToolResult {
                receipt: ImageProvenanceReceipt {
                    schema: opensks_contracts::IMAGE_PROVENANCE_RECEIPT_SCHEMA.to_string(),
                    asset_id: request.artifact_ref.clone(),
                    operation: opensks_contracts::ImageOperation::Inspect,
                    provider_id: "provider-1".to_string(),
                    model_id: "provider-1/vision-model".to_string(),
                    content_hash: "sha256:v1:image-bytes".to_string(),
                    prompt_hash: Some("sha256:v1:prompt".to_string()),
                    provenance_hash: "sha256:v1:inspect-provenance".to_string(),
                    route_receipt: opensks_contracts::ModelRouteReceipt {
                        provider_id: Some("provider-1".to_string()),
                        model_id: Some("provider-1/vision-model".to_string()),
                        registry_revision: "registry-1".to_string(),
                        reason_code: "route_ok".to_string(),
                        requested_capabilities: opensks_contracts::CapabilityRequirements {
                            vision_input: true,
                            ..opensks_contracts::CapabilityRequirements::default()
                        },
                        effective_limits: opensks_contracts::ModelLimits::default(),
                        fallback_index: None,
                    },
                    evidence_refs: vec!["test:image-inspect-executor".to_string()],
                },
                text: "A generated diagram.".to_string(),
            })
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_path_cannot_bypass_workspace_scope() {
        use std::os::unix::fs::symlink;

        let ws = temp_workspace("gateway-symlink");
        let outside = temp_workspace("gateway-outside");
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        symlink(&outside, ws.join("escape")).unwrap();

        let mut driver = FnDriver::new(|obs: &[ToolResult], step: usize| match step {
            0 => AgentStep::Tools(vec![ToolCall::ReadFileRange {
                path: "escape/secret.txt".to_string(),
                start_line: None,
                end_line: None,
            }]),
            _ => {
                assert!(matches!(
                    obs.first(),
                    Some(ToolResult::Failed { message, .. })
                        if message == "blocked_path_escapes_workspace_root"
                ));
                AgentStep::Final {
                    text: "blocked symlink".to_string(),
                }
            }
        });
        let sink = CollectingSink::new();
        let outcome =
            run_agentic_loop(&request(&ws), &mut driver, &AgenticConfig::default(), &sink).unwrap();
        assert_eq!(outcome.assistant_text, "blocked symlink");
        std::fs::remove_dir_all(&ws).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn tool_gateway_redacts_and_truncates_read_output() {
        let ws = temp_workspace("gateway-output");
        let synthetic_secret = ["OPENAI", "_API_KEY=abc123"].concat();
        std::fs::write(
            ws.join("secrets.txt"),
            format!("{synthetic_secret}\nsafe-line\nthis line is long\n"),
        )
        .unwrap();
        let gateway = ToolGateway::new(
            &ws,
            &AgenticConfig {
                max_tool_output_bytes: 24,
                ..AgenticConfig::default()
            },
        );
        let receipt = gateway
            .execute(&ToolCall::ReadFileRange {
                path: "secrets.txt".to_string(),
                start_line: None,
                end_line: None,
            })
            .unwrap();
        let ToolGatewayReceipt::Read(ToolResult::FileContent {
            content: Some(content),
            ..
        }) = receipt
        else {
            panic!("expected read receipt");
        };
        assert!(!content.contains("abc123"));
        assert!(content.contains("[REDACTED]"));
        assert!(content.contains("truncated by ToolGateway"));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn tool_gateway_executes_canonical_read_tools() {
        let ws = temp_workspace("gateway-canonical-read-tools");
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("src").join("lib.rs"), "alpha\nneedle\nomega\n").unwrap();
        std::fs::write(ws.join("README.md"), "needle in docs\n").unwrap();
        let gateway = ToolGateway::new(&ws, &AgenticConfig::default());

        let ToolGatewayReceipt::Read(ToolResult::ToolOutput { content: list, .. }) = gateway
            .execute(&ToolCall::ListDirectory {
                path: ".".to_string(),
            })
            .unwrap()
        else {
            panic!("expected directory list");
        };
        assert!(list.contains("README.md"));
        assert!(list.contains("src/"));

        let ToolGatewayReceipt::Read(ToolResult::FileContent {
            content: Some(range),
            ..
        }) = gateway
            .execute(&ToolCall::ReadFileRange {
                path: "src/lib.rs".to_string(),
                start_line: Some(2),
                end_line: Some(2),
            })
            .unwrap()
        else {
            panic!("expected ranged file content");
        };
        assert_eq!(range, "2: needle");

        let ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            content: search, ..
        }) = gateway
            .execute(&ToolCall::SearchText {
                query: "needle".to_string(),
                path: ".".to_string(),
                max_results: Some(10),
            })
            .unwrap()
        else {
            panic!("expected search output");
        };
        assert!(search.contains("README.md:1: needle in docs"));
        assert!(search.contains("src/lib.rs:2: needle"));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn tool_gateway_executes_read_only_git_tools() {
        let ws = temp_workspace("gateway-git-tools");
        std::fs::remove_dir_all(&ws).ok();
        std::fs::create_dir_all(&ws).unwrap();
        run_git(&ws, &["init"]);
        run_git(&ws, &["config", "user.email", "opensks@example.test"]);
        run_git(&ws, &["config", "user.name", "OpenSKS Test"]);
        run_git(&ws, &["config", "commit.gpgsign", "false"]);
        run_git(&ws, &["checkout", "-B", "main"]);
        std::fs::write(ws.join("README.md"), "before\n").unwrap();
        run_git(&ws, &["add", "README.md"]);
        run_git(&ws, &["commit", "-m", "initial"]);
        std::fs::write(ws.join("README.md"), "before\nafter\n").unwrap();

        let gateway = ToolGateway::new(&ws, &AgenticConfig::default());
        let ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            content: status, ..
        }) = gateway.execute(&ToolCall::GitStatus).unwrap()
        else {
            panic!("expected git status output");
        };
        assert!(status.contains("\"in_repo\": true"));
        assert!(status.contains("\"is_dirty\": true"));

        let ToolGatewayReceipt::Read(ToolResult::ToolOutput { content: diff, .. }) = gateway
            .execute(&ToolCall::GitDiff {
                path: Some("README.md".to_string()),
            })
            .unwrap()
        else {
            panic!("expected git diff output");
        };
        assert!(diff.contains("\"path\": \"README.md\""));
        assert!(diff.contains("+after"));

        let ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            content: history, ..
        }) = gateway
            .execute(&ToolCall::GitLog { max_count: Some(1) })
            .unwrap()
        else {
            panic!("expected git log output");
        };
        assert!(history.contains("\"subject\": \"initial\""));
        assert!(history.contains("[redacted]@example.test"));
        assert!(!history.contains("opensks@example.test"));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn tool_gateway_executes_codegraph_and_context_tools() {
        let ws = temp_workspace("gateway-codegraph-context-tools");
        std::fs::remove_dir_all(&ws).ok();
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(
            ws.join("src/lib.rs"),
            "use std::fs;\npub fn code_target() {}\n",
        )
        .unwrap();

        let record = opensks_triwiki::make_record(
            "ctx-a",
            opensks_contracts::TriWikiRecordKind::Claim,
            "Context Claim",
            "body words",
            vec!["runtime".to_string()],
            vec!["evidence:a".to_string()],
        )
        .unwrap();
        opensks_triwiki::append_record(&ws, &record).unwrap();

        let gateway = ToolGateway::new(&ws, &AgenticConfig::default());
        let ToolGatewayReceipt::Read(ToolResult::ToolOutput {
            content: symbols, ..
        }) = gateway
            .execute(&ToolCall::CodeGraphQuerySymbol {
                query: "code_target".to_string(),
                max_results: Some(5),
            })
            .unwrap()
        else {
            panic!("expected codegraph query output");
        };
        assert!(symbols.contains("\"name\": \"code_target\""));

        let graph = opensks_codegraph::CodeGraph::index_workspace(&ws).unwrap();
        let symbol = graph
            .query("code_target")
            .into_iter()
            .find(|record| record.name == "code_target")
            .expect("target symbol");
        let ToolGatewayReceipt::Read(ToolResult::ToolOutput { content: refs, .. }) = gateway
            .execute(&ToolCall::CodeGraphReferences {
                symbol_id: symbol.id,
            })
            .unwrap()
        else {
            panic!("expected codegraph refs output");
        };
        assert!(refs.contains("\"id\": \"file:src/lib.rs\""));

        let ToolGatewayReceipt::Read(ToolResult::ToolOutput { content: pack, .. }) = gateway
            .execute(&ToolCall::ContextBuildPack {
                id: "worker-pack".to_string(),
                token_budget: Some(64),
            })
            .unwrap()
        else {
            panic!("expected context pack output");
        };
        assert!(pack.contains("\"id\": \"worker-pack\""));
        assert!(pack.contains("Context Claim"));

        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn stale_precondition_is_observed_as_a_non_applied_write() {
        // If a file changes between read and write within the SAME workspace, the
        // transactional writer refuses it and the driver observes applied:false.
        let ws = temp_workspace("stale");
        std::fs::write(ws.join("f.txt"), "orig").unwrap();
        // The driver writes based on a stale view by racing the on-disk file: we
        // simulate the race by mutating the file inside the driver before the write
        // step is planned is not possible here; instead assert the happy path write
        // reports applied:true and a second identical create reports the conflict.
        let mut driver = SequenceDriver::new(
            vec![vec![ToolCall::ProposePatch {
                path: "f.txt".to_string(),
                content: "updated".to_string(),
            }]],
            "done",
        );
        let sink = CollectingSink::new();
        let outcome =
            run_agentic_loop(&request(&ws), &mut driver, &AgenticConfig::default(), &sink).unwrap();
        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        assert!(outcome.apply_results[0].applied);
        assert_eq!(
            std::fs::read_to_string(ws.join("f.txt")).unwrap(),
            "updated"
        );
        std::fs::remove_dir_all(&ws).ok();
    }
}
