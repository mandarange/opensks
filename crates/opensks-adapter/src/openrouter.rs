//! A real model adapter backed by OpenRouter's OpenAI-compatible chat API
//! (recovery release §7 — RUN-002 / CHAT-002: actual model output, not a
//! deterministic stub).
//!
//! Secret handling (§7.5 / §19.5): the API key is read from an environment
//! variable at call time — never hard-coded, never logged, never persisted, and
//! never placed in process argv. The request is made by shelling out to the
//! system `curl` (an allowlisted external tool, like git — no new crate, so the
//! dependency/advisory gate is untouched). The key is handed to `curl` through
//! its stdin config, so it appears in neither the command line (`ps`) nor any
//! file on disk. Errors are scrubbed of any `sk-`-prefixed token before they
//! surface.

use std::io::Write;
use std::process::{Command, Stdio};

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
    /// is read at call time and delivered via curl stdin — never argv, disk, or log.
    /// A transport failure (curl error / non-JSON) is an `Err`; a provider `error`
    /// field is left in the returned JSON for the caller to interpret.
    fn post_chat(&self, body: &serde_json::Value) -> Result<serde_json::Value, AgentAdapterError> {
        let api_key = std::env::var(&self.api_key_env)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| AgentAdapterError::MissingApiKey(self.api_key_env.clone()))?;

        let body = body.to_string();

        // curl config delivered via stdin: the key lives only here (a pipe),
        // never in argv or on disk.
        let mut config = String::new();
        config.push_str(&format!("url = \"{}\"\n", self.base_url));
        config.push_str("request = \"POST\"\n");
        config.push_str(&format!(
            "header = \"Authorization: Bearer {}\"\n",
            escape_curl_config(&api_key)
        ));
        config.push_str("header = \"Content-Type: application/json\"\n");
        config.push_str(&format!("data-raw = \"{}\"\n", escape_curl_config(&body)));

        let mut child = Command::new("curl")
            .args(["-sS", "--max-time", "120", "--config", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AgentAdapterError::Provider(format!("could not launch curl: {e}")))?;

        child
            .stdin
            .take()
            .ok_or_else(|| AgentAdapterError::Provider("curl stdin unavailable".to_string()))?
            .write_all(config.as_bytes())
            .map_err(|e| AgentAdapterError::Provider(redact(&e.to_string())))?;

        let output = child
            .wait_with_output()
            .map_err(|e| AgentAdapterError::Provider(redact(&e.to_string())))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(AgentAdapterError::Provider(format!(
                "curl exited with {}: {}",
                output.status,
                redact(err.trim())
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str(&stdout)
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

/// The real completer: a live OpenRouter call via curl (key via stdin only).
pub struct CurlChatCompleter {
    adapter: OpenRouterAdapter,
}

impl CurlChatCompleter {
    pub fn new(adapter: OpenRouterAdapter) -> Self {
        Self { adapter }
    }
}

impl ChatCompleter for CurlChatCompleter {
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
/// calls if the model emitted any, otherwise its final text answer. A provider
/// `error` field becomes a final answer carrying the (redacted) message.
pub fn parse_step(response: &serde_json::Value) -> AgentStep {
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown provider error");
        return AgentStep::Final {
            text: format!("The model returned an error: {}", redact(message)),
        };
    }
    let message = response.pointer("/choices/0/message");
    if let Some(calls) = message
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.as_array())
    {
        let parsed: Vec<ToolCall> = calls.iter().filter_map(parse_tool_call).collect();
        if !parsed.is_empty() {
            return AgentStep::Tools(parsed);
        }
    }
    let text = message
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    AgentStep::Final { text }
}

/// Map one OpenAI/OpenRouter `tool_calls[]` entry to a workspace [`ToolCall`].
/// `arguments` arrives as a JSON-encoded STRING (or, defensively, an object).
fn parse_tool_call(tc: &serde_json::Value) -> Option<ToolCall> {
    let func = tc.get("function")?;
    let name = func.get("name")?.as_str()?;
    let args: serde_json::Value = match func.get("arguments") {
        Some(serde_json::Value::String(s)) => serde_json::from_str(s).ok()?,
        Some(other) => other.clone(),
        None => return None,
    };
    let path = args.get("path")?.as_str()?.to_string();
    match name {
        "read_file" => Some(ToolCall::ReadFile { path }),
        "write_file" => Some(ToolCall::WriteFile {
            path,
            content: args.get("content")?.as_str()?.to_string(),
        }),
        "append_line" => Some(ToolCall::AppendLine {
            path,
            value: args.get("value")?.as_str()?.to_string(),
        }),
        _ => None,
    }
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
            Err(error) => AgentStep::Final {
                text: format!("The model call failed: {}", redact(&error.to_string())),
            },
        }
    }
}

/// Escape a value for a curl `--config` double-quoted field: backslash and quote
/// only (curl interprets `\\` and `\"`). JSON serialized to a single line has no
/// literal newlines, so this is sufficient.
fn escape_curl_config(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
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
    fn escape_handles_json_quotes_and_backslashes() {
        let body = r#"{"a":"b\nc"}"#;
        let escaped = escape_curl_config(body);
        assert!(escaped.contains("\\\""));
        assert!(escaped.contains("\\\\n"));
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
    /// key or network. Run manually with the key in env:
    ///   OPENROUTER_API_KEY=… cargo test -p opensks-adapter -- --ignored live_
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
    /// loop are tested with NO key/network (the live path only swaps this for curl).
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
    fn parse_step_surfaces_provider_error_as_final() {
        let resp = serde_json::json!({ "error": { "message": "rate limited" } });
        match parse_step(&resp) {
            AgentStep::Final { text } => assert!(text.contains("rate limited")),
            other => panic!("expected final, got {other:?}"),
        }
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
