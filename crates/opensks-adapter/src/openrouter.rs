//! A real model adapter backed by OpenRouter's OpenAI-compatible chat API
//! (recovery release §7 — RUN-002 / CHAT-002: actual model output, not a
//! deterministic stub).
//!
//! Secret handling (§7.5 / §19.5): the API key is read from an environment
//! variable at call time — never hard-coded, never logged, never persisted, and
//! never placed in process argv. Transport is native HTTP over rustls-backed
//! `reqwest`, so provider dispatch no longer depends on a subprocess.

use std::time::Duration;

use opensks_contracts::projection::RunProjectionState;
use opensks_contracts::{
    AGENT_ADAPTER_DESCRIPTOR_SCHEMA, AgentAdapterDescriptor, AgentAdapterKind,
};

use crate::{
    AgentAdapter, AgentAdapterError, AgentEventKind, AgentEventSink, AgentRunOutcome,
    AgentRunRequest, AgentStep, ToolCall, ToolDriver, ToolResult,
};

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const DEFAULT_API_KEY_ENV: &str = "OPENROUTER_API_KEY";

/// A live text/code model adapter. Configured with a model id and the env var
/// holding the API key; performs one non-streaming chat completion per turn.
#[derive(Debug, Clone)]
pub struct OpenRouterAdapter {
    pub model: String,
    pub api_key_env: String,
    pub base_url: String,
    /// Hard ceiling on completion tokens for a turn (frugal default; a thread's
    /// token budget overrides this once settings routing lands).
    pub max_tokens: u32,
}

impl OpenRouterAdapter {
    /// Adapter for `model` reading the key from `OPENROUTER_API_KEY`.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
            base_url: DEFAULT_BASE_URL.to_string(),
            max_tokens: 1024,
        }
    }

    /// A frugal default model for smoke checks.
    pub fn default_model() -> Self {
        Self::new("openai/gpt-4o-mini")
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Whether the configured API key is present in the environment. Callers use
    /// this to decide between the live model and the local simulation lane
    /// without ever reading the key value.
    pub fn is_configured(&self) -> bool {
        std::env::var(&self.api_key_env)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    }

    fn request_body(&self, prompt: &str) -> serde_json::Value {
        serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": [{ "role": "user", "content": prompt }],
        })
    }

    /// POST a chat-completions `body` and return the parsed JSON response. The key
    /// is read at call time and used only as an authorization header in the native
    /// HTTP request; it is never placed in argv, disk, or event payloads.
    fn post_chat(&self, body: &serde_json::Value) -> Result<serde_json::Value, AgentAdapterError> {
        let api_key = std::env::var(&self.api_key_env)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| AgentAdapterError::MissingApiKey(self.api_key_env.clone()))?;

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .user_agent("opensks-adapter/0.1")
            .build()
            .map_err(|error| AgentAdapterError::Provider(redact(&error.to_string())))?;
        let response = client
            .post(&self.base_url)
            .bearer_auth(api_key)
            .json(body)
            .send()
            .map_err(|error| AgentAdapterError::Provider(redact(&error.to_string())))?;
        let status = response.status();
        let response_body = response
            .text()
            .map_err(|error| AgentAdapterError::Provider(redact(&error.to_string())))?;
        if !status.is_success() {
            return Err(AgentAdapterError::Provider(format!(
                "provider HTTP status {}: {}",
                status.as_u16(),
                redact(response_body.trim())
            )));
        }
        serde_json::from_str(&response_body)
            .map_err(|_| AgentAdapterError::Provider("provider returned non-JSON".to_string()))
    }

    /// Perform one plain-text completion. Returns the assistant text. Never logs
    /// the key.
    fn complete(&self, prompt: &str) -> Result<String, AgentAdapterError> {
        let parsed = self.post_chat(&self.request_body(prompt))?;

        if let Some(error) = parsed.get("error") {
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown provider error");
            return Err(AgentAdapterError::Provider(redact(message)));
        }

        parsed
            .pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                AgentAdapterError::Provider("provider response had no message content".to_string())
            })
    }
}

impl AgentAdapter for OpenRouterAdapter {
    fn descriptor(&self) -> AgentAdapterDescriptor {
        AgentAdapterDescriptor {
            schema: AGENT_ADAPTER_DESCRIPTOR_SCHEMA.to_string(),
            adapter_id: format!("openrouter:{}", self.model),
            display_name: format!("OpenRouter · {}", self.model),
            kind: AgentAdapterKind::Model,
            supports_streaming: false,
            supports_tools: false,
            supports_resume: false,
            supports_parallel_sessions: true,
            supported_reasoning_efforts: vec![],
        }
    }

    fn run(
        &self,
        request: &AgentRunRequest,
        sink: &dyn AgentEventSink,
    ) -> Result<AgentRunOutcome, AgentAdapterError> {
        let mut sequence: u64 = 0;
        let emit = |kind: AgentEventKind, payload: serde_json::Value, seq: &mut u64| {
            let s = *seq;
            *seq += 1;
            sink.emit(crate::AgentEventEnvelope {
                schema: opensks_contracts::AGENT_EVENT_ENVELOPE_SCHEMA.to_string(),
                stream_id: request.stream_id.clone(),
                project_id: request.project_id.clone(),
                conversation_id: request.conversation_id.clone(),
                turn_id: request.turn_id.clone(),
                run_id: request.run_id.clone(),
                worker_id: Some(self.descriptor().adapter_id),
                node_id: None,
                sequence: s,
                occurred_at_ms: request.now_ms,
                kind,
                payload,
                sensitivity: opensks_contracts::Sensitivity::Internal,
                evidence_refs: vec![],
            });
        };

        match self.complete(&request.prompt) {
            Ok(text) => {
                emit(
                    AgentEventKind::AssistantTextCompleted,
                    serde_json::json!({ "text": text }),
                    &mut sequence,
                );
                Ok(AgentRunOutcome {
                    assistant_text: text,
                    patches: vec![],
                    apply_results: vec![],
                    final_state: RunProjectionState::Completed,
                })
            }
            Err(error) => {
                let message = redact(&error.to_string());
                emit(
                    AgentEventKind::Error,
                    serde_json::json!({ "message": message }),
                    &mut sequence,
                );
                Ok(AgentRunOutcome {
                    assistant_text: format!("The model call failed: {message}"),
                    patches: vec![],
                    apply_results: vec![],
                    final_state: RunProjectionState::Failed,
                })
            }
        }
    }
}

// ===========================================================================
// Live model → agentic loop seam (the model-driven code-edit path).
//
// `OpenRouterToolDriver` makes the real `run_agentic_loop` model-drivable: each
// step sends the running message history + the workspace tool schema, parses the
// model's reply into tool calls (or a final answer), and threads the previous
// step's tool RESULTS back as the next message. The ONLY env-blocked piece is the
// HTTP call itself (a key + network), abstracted behind `ChatCompleter` so the
// parsing, threading, and loop are all exercised in tests with a scripted model.
// ===========================================================================

/// The HTTP seam for chat completions, so the tool-calling driver is testable
/// without a key or network.
pub trait ChatCompleter {
    /// POST `body` (an OpenAI-compatible chat-completions request) and return the
    /// parsed JSON response.
    fn complete(&self, body: &serde_json::Value) -> Result<serde_json::Value, AgentAdapterError>;
}

/// The real completer: a live OpenRouter call through native HTTP.
pub struct NativeHttpChatCompleter {
    adapter: OpenRouterAdapter,
}

impl NativeHttpChatCompleter {
    pub fn new(adapter: OpenRouterAdapter) -> Self {
        Self { adapter }
    }
}

impl ChatCompleter for NativeHttpChatCompleter {
    fn complete(&self, body: &serde_json::Value) -> Result<serde_json::Value, AgentAdapterError> {
        self.adapter.post_chat(body)
    }
}

/// The function/tool schema advertised to the model — the three workspace edit
/// tools the agentic loop knows how to execute.
pub fn tool_definitions() -> serde_json::Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a workspace-relative file's full text.",
                "parameters": {
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Write (create or replace) a workspace-relative file's full content.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "append_line",
                "description": "Append a line to a workspace-relative file (creating it if absent).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "value": { "type": "string" }
                    },
                    "required": ["path", "value"]
                }
            }
        }
    ])
}

/// Parse an OpenRouter chat-completions response into the loop's next step: tool
/// calls if the model emitted any, otherwise its final text answer. Provider and
/// protocol failures become explicit failed steps so they cannot be recorded as
/// successful assistant completions.
pub fn parse_step(response: &serde_json::Value) -> AgentStep {
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown provider error");
        return failed_step(
            "provider_error",
            format!("The model returned an error: {}", redact(message)),
            provider_error_retryable(message),
        );
    }
    let message = response.pointer("/choices/0/message");
    if let Some(calls) = message
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.as_array())
    {
        let mut parsed = Vec::with_capacity(calls.len());
        for call in calls {
            match parse_tool_call(call) {
                Ok(tool_call) => parsed.push(tool_call),
                Err(reason) => {
                    return failed_step(
                        "malformed_tool_call",
                        format!("The model returned a malformed tool call: {reason}"),
                        true,
                    );
                }
            }
        }
        if calls.is_empty() {
            return failed_step(
                "provider_protocol",
                "The model returned an empty tool call list.".to_string(),
                true,
            );
        }
        return AgentStep::Tools(parsed);
    }
    let Some(text) = message
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
    else {
        return failed_step(
            "provider_protocol",
            "The model response had no assistant content or tool calls.".to_string(),
            true,
        );
    };
    if text.trim().is_empty() {
        return failed_step(
            "empty_assistant_result",
            "The model returned an empty assistant result.".to_string(),
            true,
        );
    }
    AgentStep::Final {
        text: text.to_string(),
    }
}

/// Map one OpenAI/OpenRouter `tool_calls[]` entry to a workspace [`ToolCall`].
/// `arguments` arrives as a JSON-encoded STRING (or, defensively, an object).
fn parse_tool_call(tc: &serde_json::Value) -> Result<ToolCall, String> {
    let func = tc
        .get("function")
        .ok_or_else(|| "missing function object".to_string())?;
    let name = func
        .get("name")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "missing function name".to_string())?;
    let args: serde_json::Value = match func.get("arguments") {
        Some(serde_json::Value::String(s)) => {
            serde_json::from_str(s).map_err(|_| "arguments were not valid JSON".to_string())?
        }
        Some(other) => other.clone(),
        None => return Err("missing arguments".to_string()),
    };
    let path = args
        .get("path")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "missing path".to_string())?
        .to_string();
    match name {
        "read_file" => Ok(ToolCall::ReadFile { path }),
        "write_file" => Ok(ToolCall::WriteFile {
            path,
            content: args
                .get("content")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "missing content".to_string())?
                .to_string(),
        }),
        "append_line" => Ok(ToolCall::AppendLine {
            path,
            value: args
                .get("value")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "missing value".to_string())?
                .to_string(),
        }),
        _ => Err(format!("unknown tool `{name}`")),
    }
}

fn failed_step(code: &str, message: String, retryable: bool) -> AgentStep {
    AgentStep::Failed {
        code: code.to_string(),
        message,
        retryable,
    }
}

fn provider_error_retryable(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("rate")
        || lower.contains("429")
        || lower.contains("timeout")
        || lower.contains("temporar")
        || lower.contains("try again")
}

/// Render the previous step's tool results as a compact text block for the next
/// model turn, so the model sees what its calls produced.
fn observations_message(results: &[ToolResult]) -> String {
    results
        .iter()
        .map(|result| match result {
            ToolResult::FileContent { path, content } => match content {
                Some(text) => format!("read {path}:\n{text}"),
                None => format!("read {path}: (file does not exist)"),
            },
            ToolResult::Wrote {
                path,
                applied,
                reason,
            } => format!("wrote {path}: applied={applied} ({reason})"),
            ToolResult::Failed { tool, message } => format!("tool {tool} failed: {message}"),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Drives [`run_agentic_loop`](crate::run_agentic_loop) from a live (or scripted)
/// model. The model call is the only env-blocked seam; threading + parsing + loop
/// are exercised in tests via a scripted [`ChatCompleter`].
pub struct OpenRouterToolDriver<C: ChatCompleter> {
    model: String,
    max_tokens: u32,
    completer: C,
    messages: Vec<serde_json::Value>,
}

impl<C: ChatCompleter> OpenRouterToolDriver<C> {
    /// `system` frames the task + workspace contract; `goal` is the user's request.
    pub fn new(
        model: impl Into<String>,
        max_tokens: u32,
        completer: C,
        system: impl Into<String>,
        goal: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            max_tokens,
            completer,
            messages: vec![
                serde_json::json!({ "role": "system", "content": system.into() }),
                serde_json::json!({ "role": "user", "content": goal.into() }),
            ],
        }
    }

    fn request_body(&self) -> serde_json::Value {
        serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "tools": tool_definitions(),
            "messages": self.messages,
        })
    }
}

impl<C: ChatCompleter> ToolDriver for OpenRouterToolDriver<C> {
    fn next_step(&mut self, observations: &[ToolResult]) -> AgentStep {
        if !observations.is_empty() {
            self.messages.push(serde_json::json!({
                "role": "user",
                "content": observations_message(observations),
            }));
        }
        match self.completer.complete(&self.request_body()) {
            Ok(response) => {
                let step = parse_step(&response);
                // Record the assistant turn so the next request carries full context.
                if let Some(message) = response.pointer("/choices/0/message") {
                    self.messages.push(message.clone());
                }
                step
            }
            Err(error) => failed_step(
                "provider_call_failed",
                format!("The model call failed: {}", redact(&error.to_string())),
                true,
            ),
        }
    }
}

/// Scrub any `sk-`-prefixed token from a diagnostic string so a provider/API key
/// can never reach a log, event, or error surface.
fn redact(s: &str) -> String {
    s.split_whitespace()
        .map(|tok| {
            if tok.starts_with("sk-") || tok.contains("Bearer") {
                "[REDACTED]"
            } else {
                tok
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CollectingSink;
    use std::path::PathBuf;

    fn request(prompt: &str) -> AgentRunRequest {
        AgentRunRequest {
            workspace: PathBuf::from("."),
            project_id: "p1".to_string(),
            conversation_id: "c1".to_string(),
            turn_id: "t1".to_string(),
            run_id: "r1".to_string(),
            stream_id: "s1".to_string(),
            now_ms: 1000,
            prompt: prompt.to_string(),
        }
    }

    #[test]
    fn redact_scrubs_keys() {
        // Fixture deliberately uses a clearly-fake token (no real key prefix).
        let out = redact("error: Bearer sk-FAKEKEY-000 was rejected");
        assert!(!out.contains("sk-FAKEKEY-000"));
        assert!(!out.contains("Bearer"));
    }

    #[test]
    fn missing_key_is_reported_without_network() {
        // With the key env explicitly empty, the adapter fails fast with a typed
        // error rather than calling out. Uses a unique env var so it never
        // depends on the ambient OPENROUTER_API_KEY.
        let adapter = OpenRouterAdapter {
            model: "openai/gpt-4o-mini".to_string(),
            api_key_env: "OPENSKS_TEST_ABSENT_KEY_ENV".to_string(),
            base_url: DEFAULT_BASE_URL.to_string(),
            max_tokens: 5,
        };
        assert!(!adapter.is_configured());
        let sink = CollectingSink::new();
        let outcome = adapter.run(&request("hi"), &sink).unwrap();
        assert_eq!(outcome.final_state, RunProjectionState::Failed);
        assert!(outcome.assistant_text.contains("failed"));
    }

    /// Live smoke check — ignored by default so `cargo test`/CI never needs a
    /// key or network. Export an OpenRouter API key, then run:
    ///   cargo test -p opensks-adapter -- --ignored live_
    #[test]
    #[ignore = "requires OPENROUTER_API_KEY + network; run manually, costs a few tokens"]
    fn live_openrouter_returns_real_text() {
        let adapter = OpenRouterAdapter::default_model().with_max_tokens(5);
        assert!(
            adapter.is_configured(),
            "set OPENROUTER_API_KEY to run this"
        );
        let sink = CollectingSink::new();
        let outcome = adapter
            .run(&request("Reply with the single word OK"), &sink)
            .unwrap();
        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        assert!(!outcome.assistant_text.trim().is_empty());
        assert!(
            sink.kinds()
                .contains(&AgentEventKind::AssistantTextCompleted)
        );
    }

    // ---- Tool-calling driver (the model → loop → edit seam) -----------------

    use crate::{AgenticConfig, run_agentic_loop};

    /// A scripted `ChatCompleter` that replays canned responses, so the driver +
    /// loop are tested with no key/network. The live path swaps this for the
    /// native HTTP completer.
    struct ScriptedCompleter {
        responses: std::cell::RefCell<std::collections::VecDeque<serde_json::Value>>,
    }
    impl ScriptedCompleter {
        fn new(responses: Vec<serde_json::Value>) -> Self {
            Self {
                responses: std::cell::RefCell::new(responses.into()),
            }
        }
    }
    impl ChatCompleter for ScriptedCompleter {
        fn complete(
            &self,
            _body: &serde_json::Value,
        ) -> Result<serde_json::Value, AgentAdapterError> {
            Ok(self.responses.borrow_mut().pop_front().unwrap_or_else(
                || serde_json::json!({ "choices": [{ "message": { "content": "done" } }] }),
            ))
        }
    }

    struct FailingCompleter;

    impl ChatCompleter for FailingCompleter {
        fn complete(
            &self,
            _body: &serde_json::Value,
        ) -> Result<serde_json::Value, AgentAdapterError> {
            Err(AgentAdapterError::Provider(
                "temporary provider outage".to_string(),
            ))
        }
    }

    fn tool_call_response(name: &str, args: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": name, "arguments": args.to_string() }
                    }]
                }
            }]
        })
    }

    fn final_response(text: &str) -> serde_json::Value {
        serde_json::json!({ "choices": [{ "message": { "role": "assistant", "content": text } }] })
    }

    #[test]
    fn parse_step_maps_tool_calls_with_string_arguments() {
        let resp = tool_call_response(
            "append_line",
            serde_json::json!({ "path": "NOTES.md", "value": "two" }),
        );
        match parse_step(&resp) {
            AgentStep::Tools(calls) => assert_eq!(
                calls,
                vec![ToolCall::AppendLine {
                    path: "NOTES.md".to_string(),
                    value: "two".to_string()
                }]
            ),
            other => panic!("expected tools, got {other:?}"),
        }
    }

    #[test]
    fn parse_step_returns_final_text_when_no_tool_calls() {
        assert_eq!(
            parse_step(&final_response("all done")),
            AgentStep::Final {
                text: "all done".to_string()
            }
        );
    }

    #[test]
    fn parse_step_surfaces_provider_error_as_failure() {
        let resp = serde_json::json!({ "error": { "message": "rate limited" } });
        match parse_step(&resp) {
            AgentStep::Failed {
                code,
                message,
                retryable,
            } => {
                assert_eq!(code, "provider_error");
                assert!(message.contains("rate limited"));
                assert!(retryable);
            }
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[test]
    fn parse_step_rejects_empty_assistant_result() {
        let resp = final_response("");
        match parse_step(&resp) {
            AgentStep::Failed {
                code, retryable, ..
            } => {
                assert_eq!(code, "empty_assistant_result");
                assert!(retryable);
            }
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[test]
    fn parse_step_rejects_malformed_tool_call() {
        let resp = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "append_line", "arguments": "{\"path\":\"NOTES.md\"" }
                    }]
                }
            }]
        });
        match parse_step(&resp) {
            AgentStep::Failed { code, message, .. } => {
                assert_eq!(code, "malformed_tool_call");
                assert!(message.contains("valid JSON"));
            }
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[test]
    fn provider_http_error_fails_the_agentic_loop() {
        let ws =
            std::env::temp_dir().join(format!("opensks-or-provider-error-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let mut driver = OpenRouterToolDriver::new(
            "test-model",
            256,
            FailingCompleter,
            "You edit files in the workspace.",
            "Edit NOTES.md",
        );
        let request = AgentRunRequest {
            workspace: ws.clone(),
            project_id: "p".to_string(),
            conversation_id: "c".to_string(),
            turn_id: "t".to_string(),
            run_id: "r".to_string(),
            stream_id: "s".to_string(),
            now_ms: 1,
            prompt: String::new(),
        };
        let sink = CollectingSink::new();
        let outcome =
            run_agentic_loop(&request, &mut driver, &AgenticConfig::default(), &sink).unwrap();

        assert_eq!(outcome.final_state, RunProjectionState::Failed);
        assert!(outcome.assistant_text.contains("model call failed"));
        assert_eq!(sink.kinds().last(), Some(&AgentEventKind::Error));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn scripted_model_drives_the_loop_to_a_real_file_edit() {
        // A scripted "model" emits a tool call, then a final answer. The driver +
        // run_agentic_loop turn that into a REAL on-disk edit — exactly the live
        // path with only the HTTP call swapped for a script.
        let ws = std::env::temp_dir().join(format!("opensks-or-driver-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("NOTES.md"), "one\n").unwrap();

        let completer = ScriptedCompleter::new(vec![
            tool_call_response(
                "append_line",
                serde_json::json!({ "path": "NOTES.md", "value": "two" }),
            ),
            final_response("Added the line."),
        ]);
        let mut driver = OpenRouterToolDriver::new(
            "test-model",
            256,
            completer,
            "You edit files in the workspace.",
            "Append 'two' to NOTES.md",
        );

        let request = AgentRunRequest {
            workspace: ws.clone(),
            project_id: "p".to_string(),
            conversation_id: "c".to_string(),
            turn_id: "t".to_string(),
            run_id: "r".to_string(),
            stream_id: "s".to_string(),
            now_ms: 1,
            prompt: String::new(),
        };
        let sink = CollectingSink::new();
        let outcome =
            run_agentic_loop(&request, &mut driver, &AgenticConfig::default(), &sink).unwrap();

        assert_eq!(outcome.final_state, RunProjectionState::Completed);
        assert_eq!(outcome.assistant_text, "Added the line.");
        assert_eq!(
            std::fs::read_to_string(ws.join("NOTES.md")).unwrap(),
            "one\ntwo\n"
        );
        assert_eq!(outcome.apply_results.len(), 1);
        assert!(outcome.apply_results[0].applied);
        std::fs::remove_dir_all(&ws).ok();
    }
}
