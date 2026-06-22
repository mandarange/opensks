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
    AgentRunRequest,
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

    fn request_body(&self, prompt: &str) -> String {
        serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": [{ "role": "user", "content": prompt }],
        })
        .to_string()
    }

    /// Perform one completion. Returns the assistant text. Never logs the key.
    fn complete(&self, prompt: &str) -> Result<String, AgentAdapterError> {
        let api_key = std::env::var(&self.api_key_env)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| AgentAdapterError::MissingApiKey(self.api_key_env.clone()))?;

        let body = self.request_body(prompt);

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
        let parsed: serde_json::Value = serde_json::from_str(&stdout)
            .map_err(|_| AgentAdapterError::Provider("provider returned non-JSON".to_string()))?;

        if let Some(error) = parsed.get("error") {
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown provider error");
            return Err(AgentAdapterError::Provider(redact(message)));
        }

        let content = parsed
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                AgentAdapterError::Provider("provider response had no message content".to_string())
            })?;

        Ok(content)
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
}
