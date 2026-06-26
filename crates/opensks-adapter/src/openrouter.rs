//! A real model adapter backed by OpenRouter's OpenAI-compatible chat API
//! (recovery release §7 — RUN-002 / CHAT-002: actual model output, not a
//! deterministic stub).
//!
//! Secret handling (§7.5 / §19.5): the API key is read from an environment
//! variable at call time — never hard-coded, never logged, never persisted, and
//! never placed in process argv. Transport is native HTTP over rustls-backed
//! `reqwest`, so provider dispatch no longer depends on a subprocess.

use std::{collections::BTreeMap, sync::atomic::Ordering, time::Duration};

use base64::Engine as _;
use opensks_contracts::projection::RunProjectionState;
use opensks_contracts::{
    AGENT_ADAPTER_DESCRIPTOR_SCHEMA, AgentAdapterDescriptor, AgentAdapterKind, ProviderKind,
    ReasoningEffort, default_tool_registry,
};
use opensks_image::{
    ImageError, ImageGenerationClient, ImageInspectionClient, ImageInspectionProviderOutput,
    ImageInspectionProviderRequest, ImageProviderOutput, ImageProviderRequest,
};
use reqwest::header::CONTENT_TYPE;

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
            .bearer_auth(&api_key)
            .json(body)
            .send()
            .map_err(|error| {
                AgentAdapterError::Provider(redact_with_secrets(
                    &error.to_string(),
                    &[api_key.as_str()],
                ))
            })?;
        let status = response.status();
        let response_body = response.text().map_err(|error| {
            AgentAdapterError::Provider(redact_with_secrets(
                &error.to_string(),
                &[api_key.as_str()],
            ))
        })?;
        if !status.is_success() {
            return Err(AgentAdapterError::Provider(format!(
                "provider HTTP status {}: {}",
                status.as_u16(),
                redact_with_secrets(response_body.trim(), &[api_key.as_str()])
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
            supported_reasoning_efforts: openrouter_supported_reasoning_efforts(),
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
        let cancelled_outcome = |seq: &mut u64| {
            emit(
                AgentEventKind::Warning,
                serde_json::json!({
                    "code": "run_cancelled",
                    "message": "Turn cancelled before the provider response was accepted.",
                    "reason_code": "cancelled_by_user",
                }),
                seq,
            );
            AgentRunOutcome {
                assistant_text: "Turn cancelled before the provider response was accepted."
                    .to_string(),
                patches: vec![],
                apply_results: vec![],
                final_state: RunProjectionState::Cancelled,
            }
        };

        if openrouter_request_cancelled(request) {
            return Ok(cancelled_outcome(&mut sequence));
        }

        let completion = self.complete(&request.prompt);
        if openrouter_request_cancelled(request) {
            return Ok(cancelled_outcome(&mut sequence));
        }

        match completion {
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

fn openrouter_request_cancelled(request: &AgentRunRequest) -> bool {
    request
        .cancellation_token
        .as_ref()
        .is_some_and(|token| token.load(Ordering::SeqCst))
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

/// Native OpenAI-compatible chat-completions transport for provider-registry
/// dispatch. The bearer token is accepted as an in-memory value and is scrubbed
/// from provider diagnostics before anything reaches events, logs, or UI text.
pub struct OpenAiCompatibleChatCompleter {
    base_url: String,
    bearer_token: String,
    timeout: Duration,
}

pub enum ProviderChatCompleter {
    ChatCompletions(OpenAiCompatibleChatCompleter),
    Responses(OpenAiResponsesChatCompleter),
}

impl ProviderChatCompleter {
    pub fn new_for_provider(
        provider_kind: ProviderKind,
        base_url: impl Into<String>,
        bearer_token: impl Into<String>,
    ) -> Result<Self, AgentAdapterError> {
        let base_url = base_url.into();
        let bearer_token = bearer_token.into();
        if provider_kind == ProviderKind::CodexLb {
            return Ok(Self::Responses(OpenAiResponsesChatCompleter::new(
                base_url,
                bearer_token,
            )?));
        }
        Ok(Self::ChatCompletions(OpenAiCompatibleChatCompleter::new(
            base_url,
            bearer_token,
        )?))
    }
}

impl ChatCompleter for ProviderChatCompleter {
    fn complete(&self, body: &serde_json::Value) -> Result<serde_json::Value, AgentAdapterError> {
        match self {
            Self::ChatCompletions(completer) => completer.complete(body),
            Self::Responses(completer) => completer.complete(body),
        }
    }
}

impl OpenAiCompatibleChatCompleter {
    pub fn new(
        base_url: impl Into<String>,
        bearer_token: impl Into<String>,
    ) -> Result<Self, AgentAdapterError> {
        let base_url = base_url.into();
        let bearer_token = bearer_token.into();
        if bearer_token.trim().is_empty() {
            return Err(AgentAdapterError::Provider(
                "provider credential resolved empty".to_string(),
            ));
        }
        validate_chat_base_url(&base_url)?;
        Ok(Self {
            base_url,
            bearer_token,
            timeout: Duration::from_secs(120),
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn chat_completions_endpoint(&self) -> Result<String, AgentAdapterError> {
        validate_chat_base_url(&self.base_url)?;
        Ok(format!(
            "{}/chat/completions",
            self.base_url.trim_end_matches('/')
        ))
    }
}

/// Native OpenAI-compatible image generation transport for provider-registry
/// dispatch. The key is accepted as an in-memory bearer token and is scrubbed
/// from every provider diagnostic.
#[cfg(test)]
type TestHttpResponder =
    std::sync::Arc<dyn Fn(TestHttpRequest) -> Result<String, ImageError> + Send + Sync>;

pub struct OpenAiCompatibleImageGenerator {
    base_url: String,
    bearer_token: String,
    timeout: Duration,
    #[cfg(test)]
    test_http: Option<TestHttpResponder>,
}

impl OpenAiCompatibleImageGenerator {
    pub fn new(
        base_url: impl Into<String>,
        bearer_token: impl Into<String>,
    ) -> Result<Self, AgentAdapterError> {
        let base_url = base_url.into();
        let bearer_token = bearer_token.into();
        if bearer_token.trim().is_empty() {
            return Err(AgentAdapterError::Provider(
                "provider credential resolved empty".to_string(),
            ));
        }
        validate_chat_base_url(&base_url)?;
        Ok(Self {
            base_url,
            bearer_token,
            timeout: Duration::from_secs(120),
            #[cfg(test)]
            test_http: None,
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[cfg(test)]
    fn with_test_http(
        mut self,
        responder: impl Fn(TestHttpRequest) -> Result<String, ImageError> + Send + Sync + 'static,
    ) -> Self {
        self.test_http = Some(std::sync::Arc::new(responder));
        self
    }

    fn image_generations_endpoint(&self) -> Result<String, AgentAdapterError> {
        validate_chat_base_url(&self.base_url)?;
        Ok(format!(
            "{}/images/generations",
            self.base_url.trim_end_matches('/')
        ))
    }

    fn chat_completions_endpoint(&self) -> Result<String, AgentAdapterError> {
        validate_chat_base_url(&self.base_url)?;
        Ok(format!(
            "{}/chat/completions",
            self.base_url.trim_end_matches('/')
        ))
    }

    fn post_generation(
        &self,
        request: &ImageProviderRequest<'_>,
    ) -> Result<serde_json::Value, ImageError> {
        let endpoint = self
            .image_generations_endpoint()
            .map_err(|error| ImageError::Provider(error.to_string()))?;
        let body = serde_json::json!({
            "model": request.remote_model_id,
            "prompt": request.prompt,
            "n": 1,
            "size": format!("{}x{}", request.width, request.height),
        });
        self.post_provider_json(&endpoint, &body, "opensks-adapter/0.1 provider-image")
    }

    fn download_image_url(&self, url: &str) -> Result<(Vec<u8>, Option<String>), ImageError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .user_agent("opensks-adapter/0.1 provider-image-download")
            .build()
            .map_err(|error| ImageError::Provider(error.to_string()))?;
        let response = client.get(url).send().map_err(|error| {
            ImageError::Provider(redact_with_secrets(
                &error.to_string(),
                &[self.bearer_token.as_str()],
            ))
        })?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        if !status.is_success() {
            return Err(ImageError::Provider(format!(
                "provider image URL download HTTP status {}",
                status.as_u16()
            )));
        }
        let bytes = response.bytes().map_err(|error| {
            ImageError::Provider(redact_with_secrets(
                &error.to_string(),
                &[self.bearer_token.as_str()],
            ))
        })?;
        Ok((bytes.to_vec(), content_type))
    }
}

impl ImageInspectionClient for OpenAiCompatibleImageGenerator {
    fn inspect_image(
        &self,
        request: &ImageInspectionProviderRequest<'_>,
    ) -> Result<ImageInspectionProviderOutput, ImageError> {
        let endpoint = self
            .chat_completions_endpoint()
            .map_err(|error| ImageError::Provider(error.to_string()))?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(request.bytes);
        let data_url = format!("data:{};base64,{}", request.mime_type, encoded);
        let body = serde_json::json!({
            "model": request.remote_model_id,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": request.prompt },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": data_url
                        }
                    }
                ]
            }],
            "max_completion_tokens": 512,
        });
        let json =
            self.post_provider_json(&endpoint, &body, "opensks-adapter/0.1 provider-vision")?;
        if let Some(error) = json.get("error") {
            let message = error
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown provider error");
            return Err(ImageError::Provider(redact_with_secrets(
                message,
                &[self.bearer_token.as_str()],
            )));
        }
        let text = json
            .pointer("/choices/0/message/content")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ImageError::Provider("provider returned no vision text".to_string()))?
            .to_string();
        Ok(ImageInspectionProviderOutput {
            text,
            evidence_refs: vec!["adapter:openai-compatible-vision:chat-completions".to_string()],
        })
    }
}

impl OpenAiCompatibleImageGenerator {
    fn post_provider_json(
        &self,
        endpoint: &str,
        body: &serde_json::Value,
        user_agent: &str,
    ) -> Result<serde_json::Value, ImageError> {
        #[cfg(test)]
        if let Some(responder) = &self.test_http {
            let response_body = responder(TestHttpRequest {
                endpoint: endpoint.to_string(),
                bearer_token: self.bearer_token.clone(),
                user_agent: user_agent.to_string(),
                body: body.clone(),
            })?;
            return serde_json::from_str(&response_body)
                .map_err(|_| ImageError::Provider("provider returned non-JSON".to_string()));
        }

        let client = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .user_agent(user_agent)
            .build()
            .map_err(|error| ImageError::Provider(error.to_string()))?;
        let response = client
            .post(endpoint)
            .bearer_auth(&self.bearer_token)
            .json(body)
            .send()
            .map_err(|error| {
                ImageError::Provider(redact_with_secrets(
                    &error.to_string(),
                    &[self.bearer_token.as_str()],
                ))
            })?;
        let status = response.status();
        let response_body = response.text().map_err(|error| {
            ImageError::Provider(redact_with_secrets(
                &error.to_string(),
                &[self.bearer_token.as_str()],
            ))
        })?;
        if !status.is_success() {
            return Err(ImageError::Provider(format!(
                "provider HTTP status {}: {}",
                status.as_u16(),
                redact_with_secrets(response_body.trim(), &[self.bearer_token.as_str()])
            )));
        }
        serde_json::from_str(&response_body)
            .map_err(|_| ImageError::Provider("provider returned non-JSON".to_string()))
    }
}

#[cfg(test)]
struct TestHttpRequest {
    endpoint: String,
    bearer_token: String,
    user_agent: String,
    body: serde_json::Value,
}

impl ImageGenerationClient for OpenAiCompatibleImageGenerator {
    fn generate_image(
        &self,
        request: &ImageProviderRequest<'_>,
    ) -> Result<ImageProviderOutput, ImageError> {
        let response = self.post_generation(request)?;
        if let Some(error) = response.get("error") {
            let message = error
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown provider error");
            return Err(ImageError::Provider(redact_with_secrets(
                message,
                &[self.bearer_token.as_str()],
            )));
        }
        let image = response
            .get("data")
            .and_then(|value| value.as_array())
            .and_then(|items| items.first())
            .ok_or_else(|| ImageError::Provider("provider returned no image data".to_string()))?;
        if let Some(encoded) = image.get("b64_json").and_then(|value| value.as_str()) {
            let bytes = decode_base64_image(encoded)?;
            let extension = image_extension_for(&bytes, None);
            return Ok(ImageProviderOutput {
                bytes,
                extension,
                mime_type: None,
                evidence_refs: vec!["adapter:openai-compatible-images:b64_json".to_string()],
            });
        }
        if let Some(url) = image.get("url").and_then(|value| value.as_str()) {
            let (bytes, content_type) = self.download_image_url(url)?;
            let extension = image_extension_for(&bytes, content_type.as_deref());
            return Ok(ImageProviderOutput {
                bytes,
                extension,
                mime_type: content_type,
                evidence_refs: vec!["adapter:openai-compatible-images:url".to_string()],
            });
        }
        Err(ImageError::Provider(
            "provider image object had no b64_json or url".to_string(),
        ))
    }
}

impl ChatCompleter for OpenAiCompatibleChatCompleter {
    fn complete(&self, body: &serde_json::Value) -> Result<serde_json::Value, AgentAdapterError> {
        let endpoint = self.chat_completions_endpoint()?;
        let client = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .user_agent("opensks-adapter/0.1 provider-chat")
            .build()
            .map_err(|error| AgentAdapterError::Provider(error.to_string()))?;
        let response = client
            .post(endpoint)
            .bearer_auth(&self.bearer_token)
            .json(body)
            .send()
            .map_err(|error| {
                AgentAdapterError::Provider(redact_with_secrets(
                    &error.to_string(),
                    &[self.bearer_token.as_str()],
                ))
            })?;
        let status = response.status();
        let response_body = response.text().map_err(|error| {
            AgentAdapterError::Provider(redact_with_secrets(
                &error.to_string(),
                &[self.bearer_token.as_str()],
            ))
        })?;
        if !status.is_success() {
            return Err(AgentAdapterError::Provider(format!(
                "provider HTTP status {}: {}",
                status.as_u16(),
                redact_with_secrets(response_body.trim(), &[self.bearer_token.as_str()])
            )));
        }
        serde_json::from_str(&response_body)
            .map_err(|_| AgentAdapterError::Provider("provider returned non-JSON".to_string()))
    }
}

/// Responses API transport for Codex LB. The OpenSKS tool loop is written
/// against Chat Completions-shaped messages, so this adapter translates the
/// request and response at the provider boundary only.
pub struct OpenAiResponsesChatCompleter {
    base_url: String,
    bearer_token: String,
    timeout: Duration,
}

impl OpenAiResponsesChatCompleter {
    pub fn new(
        base_url: impl Into<String>,
        bearer_token: impl Into<String>,
    ) -> Result<Self, AgentAdapterError> {
        let base_url = base_url.into();
        let bearer_token = bearer_token.into();
        if bearer_token.trim().is_empty() {
            return Err(AgentAdapterError::Provider(
                "provider credential resolved empty".to_string(),
            ));
        }
        validate_chat_base_url(&base_url)?;
        Ok(Self {
            base_url,
            bearer_token,
            timeout: Duration::from_secs(120),
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn responses_endpoint(&self) -> Result<String, AgentAdapterError> {
        validate_chat_base_url(&self.base_url)?;
        Ok(format!("{}/responses", self.base_url.trim_end_matches('/')))
    }
}

impl ChatCompleter for OpenAiResponsesChatCompleter {
    fn complete(&self, body: &serde_json::Value) -> Result<serde_json::Value, AgentAdapterError> {
        let endpoint = self.responses_endpoint()?;
        let responses_body = responses_body_from_chat_body(body)?;
        let client = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .user_agent("opensks-adapter/0.1 provider-responses")
            .build()
            .map_err(|error| AgentAdapterError::Provider(error.to_string()))?;
        let response = client
            .post(endpoint)
            .bearer_auth(&self.bearer_token)
            .json(&responses_body)
            .send()
            .map_err(|error| {
                AgentAdapterError::Provider(redact_with_secrets(
                    &error.to_string(),
                    &[self.bearer_token.as_str()],
                ))
            })?;
        let status = response.status();
        let response_body = response.text().map_err(|error| {
            AgentAdapterError::Provider(redact_with_secrets(
                &error.to_string(),
                &[self.bearer_token.as_str()],
            ))
        })?;
        if !status.is_success() {
            return Err(AgentAdapterError::Provider(format!(
                "provider HTTP status {}: {}",
                status.as_u16(),
                redact_with_secrets(response_body.trim(), &[self.bearer_token.as_str()])
            )));
        }
        let parsed = parse_responses_transport_body(&response_body)?;
        chat_completion_from_responses_body(&parsed)
    }
}

fn parse_responses_transport_body(body: &str) -> Result<serde_json::Value, AgentAdapterError> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        return Ok(value);
    }
    let mut output_items = Vec::new();
    let mut text = String::new();
    let mut completed_response = None;
    let mut function_arguments: BTreeMap<String, String> = BTreeMap::new();
    for line in body.lines() {
        let Some(payload) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(payload) else {
            continue;
        };
        match event.get("type").and_then(serde_json::Value::as_str) {
            Some("response.completed") => {
                if let Some(response) = event.get("response") {
                    completed_response = Some(response.clone());
                }
            }
            Some("response.function_call_arguments.delta") => {
                if let (Some(key), Some(delta)) = (
                    response_event_item_key(&event, None),
                    event.get("delta").and_then(serde_json::Value::as_str),
                ) {
                    function_arguments.entry(key).or_default().push_str(delta);
                }
            }
            Some("response.function_call_arguments.done") => {
                if let (Some(key), Some(arguments)) = (
                    response_event_item_key(&event, None),
                    event.get("arguments").and_then(serde_json::Value::as_str),
                ) {
                    function_arguments.insert(key, arguments.to_string());
                }
            }
            Some("response.output_item.added") => {
                // This is a provisional Responses API frame. Function-call
                // arguments are still empty here and arrive through later
                // response.function_call_arguments.* + output_item.done events.
            }
            Some("response.output_item.done") => {
                if let Some(item) = event.get("item") {
                    output_items.push(response_item_with_streamed_arguments(
                        item,
                        &event,
                        &function_arguments,
                    ));
                }
            }
            Some("response.output_text.delta") => {
                if let Some(delta) = event.get("delta").and_then(serde_json::Value::as_str) {
                    text.push_str(delta);
                }
            }
            _ => {
                if let Some(response) = event.get("response") {
                    completed_response = Some(response.clone());
                } else if let Some(item) = event.get("item") {
                    output_items.push(response_item_with_streamed_arguments(
                        item,
                        &event,
                        &function_arguments,
                    ));
                }
            }
        }
    }
    if let Some(response) = completed_response.filter(response_body_has_output) {
        return Ok(response);
    }
    if !output_items.is_empty() {
        return Ok(serde_json::json!({ "output": output_items }));
    }
    if !text.trim().is_empty() {
        return Ok(serde_json::json!({ "output_text": text }));
    }
    Err(AgentAdapterError::Provider(
        "provider returned non-JSON".to_string(),
    ))
}

fn response_event_item_key(
    event: &serde_json::Value,
    item: Option<&serde_json::Value>,
) -> Option<String> {
    item.and_then(|value| value.get("id"))
        .or_else(|| item.and_then(|value| value.get("call_id")))
        .or_else(|| event.get("item_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            event
                .get("output_index")
                .and_then(serde_json::Value::as_u64)
                .map(|index| format!("output_index:{index}"))
        })
}

fn response_item_with_streamed_arguments(
    item: &serde_json::Value,
    event: &serde_json::Value,
    function_arguments: &BTreeMap<String, String>,
) -> serde_json::Value {
    let mut item = item.clone();
    if !matches!(
        item.get("type").and_then(serde_json::Value::as_str),
        Some("function_call") | Some("custom_tool_call")
    ) {
        return item;
    }
    let has_valid_arguments = item
        .get("arguments")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|arguments| !arguments.trim().is_empty());
    if has_valid_arguments {
        return item;
    }
    let streamed = response_event_item_key(event, Some(&item))
        .and_then(|key| function_arguments.get(&key))
        .or_else(|| {
            event
                .get("output_index")
                .and_then(serde_json::Value::as_u64)
                .and_then(|index| function_arguments.get(&format!("output_index:{index}")))
        });
    if let Some(arguments) = streamed {
        item["arguments"] = serde_json::Value::String(arguments.clone());
    }
    item
}

fn response_body_has_output(response: &serde_json::Value) -> bool {
    response
        .get("output")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| !items.is_empty())
        || response
            .get("output_text")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|text| !text.trim().is_empty())
        || response.pointer("/choices/0/message").is_some()
        || response.pointer("/response/choices/0/message").is_some()
}

fn responses_body_from_chat_body(
    body: &serde_json::Value,
) -> Result<serde_json::Value, AgentAdapterError> {
    let model = body
        .get("model")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| AgentAdapterError::Provider("chat body missing model".to_string()))?;
    let messages = body
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| AgentAdapterError::Provider("chat body missing messages".to_string()))?;
    let mut request = serde_json::json!({
        "model": model,
        "stream": false,
        "input": messages
            .iter()
            .map(response_input_from_chat_message)
            .collect::<Vec<_>>(),
    });
    if let Some(max_tokens) = body.get("max_tokens").and_then(serde_json::Value::as_u64) {
        request["max_output_tokens"] = serde_json::Value::from(max_tokens);
    }
    if let Some(tools) = body.get("tools").and_then(serde_json::Value::as_array) {
        let converted = tools
            .iter()
            .filter_map(response_tool_from_chat_tool)
            .collect::<Vec<_>>();
        if !converted.is_empty() {
            request["tools"] = serde_json::Value::Array(converted);
        }
    }
    if let Some(effort) = body
        .get("reasoning_effort")
        .and_then(serde_json::Value::as_str)
    {
        request["reasoning"] = serde_json::json!({ "effort": effort });
    } else if let Some(reasoning) = body.get("reasoning") {
        request["reasoning"] = reasoning.clone();
    }
    if let Some(tool_choice) = body.get("tool_choice") {
        request["tool_choice"] = tool_choice.clone();
    }
    Ok(request)
}

fn response_input_from_chat_message(message: &serde_json::Value) -> serde_json::Value {
    let role = message
        .get("role")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("user");
    let content = message
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    serde_json::json!({
        "role": role,
        "content": content,
    })
}

fn response_tool_from_chat_tool(tool: &serde_json::Value) -> Option<serde_json::Value> {
    let function = tool.get("function")?;
    let name = function.get("name")?.clone();
    let mut converted = serde_json::json!({
        "type": "function",
        "name": name,
    });
    if let Some(description) = function.get("description") {
        converted["description"] = description.clone();
    }
    if let Some(parameters) = function.get("parameters") {
        converted["parameters"] = parameters.clone();
    }
    Some(converted)
}

fn chat_completion_from_responses_body(
    response: &serde_json::Value,
) -> Result<serde_json::Value, AgentAdapterError> {
    if response.pointer("/choices/0/message").is_some() {
        return Ok(response.clone());
    }
    if let Some(message) = response.pointer("/response/choices/0/message") {
        return Ok(serde_json::json!({
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": "stop"
            }],
            "usage": response.get("usage").cloned().unwrap_or(serde_json::Value::Null),
        }));
    }
    let output = response
        .get("output")
        .or_else(|| response.pointer("/response/output"))
        .and_then(serde_json::Value::as_array);
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();
    if let Some(output) = output {
        for item in output {
            match item.get("type").and_then(serde_json::Value::as_str) {
                Some("message") => collect_response_message_text(item, &mut text_parts),
                Some("function_call") => {
                    if let Some(call) = chat_tool_call_from_response_item(item) {
                        tool_calls.push(call);
                    }
                }
                Some("custom_tool_call") => {
                    if let Some(call) = chat_tool_call_from_response_item(item) {
                        tool_calls.push(call);
                    }
                }
                Some("output_text") => {
                    if let Some(text) = item.get("text").and_then(serde_json::Value::as_str) {
                        text_parts.push(text.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(text) = response
        .get("output_text")
        .and_then(serde_json::Value::as_str)
    {
        text_parts.push(text.to_string());
    }
    let mut message = serde_json::json!({
        "role": "assistant",
        "content": text_parts.join("\n").trim().to_string(),
    });
    let finish_reason = if tool_calls.is_empty() {
        if message
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .is_empty()
        {
            return Err(AgentAdapterError::Provider(
                "provider response had no message content".to_string(),
            ));
        }
        "stop"
    } else {
        message["content"] = serde_json::Value::Null;
        message["tool_calls"] = serde_json::Value::Array(tool_calls);
        "tool_calls"
    };
    Ok(serde_json::json!({
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": response.get("usage").cloned().unwrap_or(serde_json::Value::Null),
    }))
}

fn collect_response_message_text(item: &serde_json::Value, text_parts: &mut Vec<String>) {
    if let Some(content) = item.get("content").and_then(serde_json::Value::as_array) {
        for part in content {
            match part.get("type").and_then(serde_json::Value::as_str) {
                Some("output_text") => {
                    if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
                        text_parts.push(text.to_string());
                    }
                }
                Some("refusal") => {
                    if let Some(text) = part.get("refusal").and_then(serde_json::Value::as_str) {
                        text_parts.push(text.to_string());
                    }
                }
                _ => {}
            }
        }
    } else if let Some(text) = item.get("content").and_then(serde_json::Value::as_str) {
        text_parts.push(text.to_string());
    }
}

fn chat_tool_call_from_response_item(item: &serde_json::Value) -> Option<serde_json::Value> {
    let name = item
        .get("name")
        .or_else(|| item.pointer("/function/name"))?
        .as_str()?;
    let arguments = item
        .get("arguments")
        .or_else(|| item.get("input"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!("{}"));
    let arguments = match arguments {
        serde_json::Value::String(value) => value,
        other => other.to_string(),
    };
    Some(serde_json::json!({
        "id": item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("call_responses"),
        "type": "function",
        "function": {
            "name": name,
            "arguments": arguments,
        }
    }))
}

fn validate_chat_base_url(base_url: &str) -> Result<(), AgentAdapterError> {
    let parsed = reqwest::Url::parse(base_url).map_err(|error| {
        AgentAdapterError::Provider(format!("invalid provider endpoint: {error}"))
    })?;
    if parsed.scheme() == "http" && !is_loopback_host(parsed.host_str()) {
        return Err(AgentAdapterError::Provider(
            "insecure HTTP provider endpoint must be local".to_string(),
        ));
    }
    if base_url.contains('@') {
        return Err(AgentAdapterError::Provider(
            "provider endpoint must not contain userinfo credentials".to_string(),
        ));
    }
    Ok(())
}

fn is_loopback_host(host: Option<&str>) -> bool {
    matches!(host, Some("localhost") | Some("127.0.0.1") | Some("::1"))
}

fn decode_base64_image(encoded: &str) -> Result<Vec<u8>, ImageError> {
    let payload = encoded
        .strip_prefix("data:")
        .and_then(|_| encoded.split_once(',').map(|(_, payload)| payload))
        .unwrap_or(encoded);
    base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|_| ImageError::Provider("provider returned invalid base64 image".to_string()))
}

fn image_extension_for(bytes: &[u8], content_type: Option<&str>) -> String {
    match content_type
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("image/png") => return "png".to_string(),
        Some("image/jpeg") | Some("image/jpg") => return "jpeg".to_string(),
        Some("image/webp") => return "webp".to_string(),
        _ => {}
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png".to_string()
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "jpeg".to_string()
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        "webp".to_string()
    } else {
        "bin".to_string()
    }
}

/// The function/tool schema advertised to OpenAI-compatible providers. The
/// canonical ToolRegistry is the single source; provider function names use a
/// safe spelling while preserving the dotted tool id in the description.
pub fn tool_definitions() -> serde_json::Value {
    tool_definitions_with_extra_available_tools(&[])
}

pub fn tool_definitions_with_extra_available_tools(extra_tool_names: &[&str]) -> serde_json::Value {
    serde_json::Value::Array(
        default_tool_registry()
            .tools
            .iter()
            .filter(|tool| {
                let explicitly_enabled = extra_tool_names.iter().any(|name| *name == tool.name);
                explicitly_enabled
                    || (tool.is_available() && !requires_runtime_executor(&tool.name))
            })
            .filter(|tool| !matches!(tool.permission, opensks_contracts::ToolPermission::Deny))
            .map(provider_tool_definition)
            .collect(),
    )
}

fn requires_runtime_executor(tool_name: &str) -> bool {
    matches!(tool_name, "image.generate" | "image.inspect")
}

fn provider_tool_definition(tool: &opensks_contracts::ToolDescriptor) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": tool.provider_function_name(),
            "description": format!("{} Canonical tool id: {}.", tool.description, tool.name),
            "parameters": tool.input_schema.clone(),
        }
    })
}

#[allow(dead_code)]
fn legacy_tool_definitions() -> serde_json::Value {
    serde_json::Value::Array(
        default_tool_registry()
            .available_provider_tools()
            .into_iter()
            .map(provider_tool_definition)
            .collect(),
    )
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
        Some(serde_json::Value::String(s)) => parse_tool_arguments_string(s)?,
        Some(other) => other.clone(),
        None => return Err("missing arguments".to_string()),
    };
    let path = args
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or(".")
        .to_string();
    match name {
        "workspace__read_file_range" | "workspace.read_file_range" | "read_file" => {
            Ok(ToolCall::ReadFileRange {
                path,
                start_line: args
                    .get("start_line")
                    .and_then(|value| value.as_u64())
                    .map(|value| value as u32),
                end_line: args
                    .get("end_line")
                    .and_then(|value| value.as_u64())
                    .map(|value| value as u32),
            })
        }
        "workspace__list_directory" | "workspace.list_directory" => {
            Ok(ToolCall::ListDirectory { path })
        }
        "workspace__search_text" | "workspace.search_text" => Ok(ToolCall::SearchText {
            query: args
                .get("query")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "missing query".to_string())?
                .to_string(),
            path,
            max_results: args
                .get("max_results")
                .and_then(|value| value.as_u64())
                .map(|value| value as usize),
        }),
        "workspace__propose_patch" | "workspace.propose_patch" | "write_file" => {
            Ok(ToolCall::ProposePatch {
                path,
                content: args
                    .get("content")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| "missing content".to_string())?
                    .to_string(),
            })
        }
        "workspace__diff_patch" | "workspace.diff_patch" | "append_line" => {
            Ok(ToolCall::DiffPatch {
                path,
                value: args
                    .get("value")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| "missing value".to_string())?
                    .to_string(),
            })
        }
        "git__status" | "git.status" => Ok(ToolCall::GitStatus),
        "git__diff" | "git.diff" => Ok(ToolCall::GitDiff {
            path: args
                .get("path")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        }),
        "git__log" | "git.log" => Ok(ToolCall::GitLog {
            max_count: args
                .get("max_count")
                .and_then(|value| value.as_u64())
                .map(|value| value as u32),
        }),
        "codegraph__query_symbol" | "codegraph.query_symbol" => {
            Ok(ToolCall::CodeGraphQuerySymbol {
                query: args
                    .get("query")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| "missing query".to_string())?
                    .to_string(),
                max_results: args
                    .get("max_results")
                    .and_then(|value| value.as_u64())
                    .map(|value| value as usize),
            })
        }
        "codegraph__references" | "codegraph.references" => Ok(ToolCall::CodeGraphReferences {
            symbol_id: args
                .get("symbol_id")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "missing symbol_id".to_string())?
                .to_string(),
        }),
        "context__build_pack" | "context.build_pack" => Ok(ToolCall::ContextBuildPack {
            id: args
                .get("id")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "missing id".to_string())?
                .to_string(),
            token_budget: args
                .get("token_budget")
                .and_then(|value| value.as_u64())
                .map(|value| value as u32),
        }),
        "command__run" | "command.run" => Ok(ToolCall::CommandRun {
            command: args
                .get("command")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "missing command".to_string())?
                .to_string(),
            timeout_ms: args.get("timeout_ms").and_then(|value| value.as_u64()),
        }),
        "test__run_targeted" | "test.run_targeted" => Ok(ToolCall::TestRunTargeted {
            target: args
                .get("target")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "missing target".to_string())?
                .to_string(),
            timeout_ms: args.get("timeout_ms").and_then(|value| value.as_u64()),
        }),
        "mcp__invoke" | "mcp.invoke" => Ok(ToolCall::McpInvoke {
            tool_name: args
                .get("tool")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "missing tool".to_string())?
                .to_string(),
            payload: args
                .get("payload")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        }),
        "skill__invoke" | "skill.invoke" => Ok(ToolCall::SkillInvoke {
            skill: args
                .get("skill")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "missing skill".to_string())?
                .to_string(),
            payload: args
                .get("payload")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        }),
        "artifact__read" | "artifact.read" => Ok(ToolCall::ArtifactRead {
            artifact_ref: args
                .get("artifact_ref")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "missing artifact_ref".to_string())?
                .to_string(),
        }),
        "artifact__write" | "artifact.write" => Ok(ToolCall::ArtifactWrite {
            artifact_ref: args
                .get("artifact_ref")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "missing artifact_ref".to_string())?
                .to_string(),
            content: args
                .get("content")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "missing content".to_string())?
                .to_string(),
        }),
        "image__generate" | "image.generate" => Ok(ToolCall::ImageGenerate {
            prompt: args
                .get("prompt")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "missing prompt".to_string())?
                .to_string(),
            asset_id: args
                .get("asset_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            width: args
                .get("width")
                .and_then(|value| value.as_u64())
                .map(|value| value as u32),
            height: args
                .get("height")
                .and_then(|value| value.as_u64())
                .map(|value| value as u32),
        }),
        "image__inspect" | "image.inspect" => Ok(ToolCall::ImageInspect {
            artifact_ref: args
                .get("artifact_ref")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            asset_id: args
                .get("asset_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            prompt: args
                .get("prompt")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        }),
        _ => Err(format!("unknown tool `{name}`")),
    }
}

fn parse_tool_arguments_string(raw: &str) -> Result<serde_json::Value, String> {
    if let Ok(value) = serde_json::from_str(raw) {
        return Ok(value);
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(serde_json::json!({}));
    }
    if let Some(unfenced) = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|value| value.strip_suffix("```"))
    {
        if let Ok(value) = serde_json::from_str(unfenced.trim()) {
            return Ok(value);
        }
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if start < end {
            if let Ok(value) = serde_json::from_str(&trimmed[start..=end]) {
                return Ok(value);
            }
        }
    }
    Err(format!(
        "arguments were not valid JSON: {}",
        diagnostic_snippet(raw)
    ))
}

fn diagnostic_snippet(raw: &str) -> String {
    let redacted = redact(raw);
    let mut snippet = redacted.chars().take(160).collect::<String>();
    if redacted.chars().count() > 160 {
        snippet.push_str("...");
    }
    snippet.replace('\n', "\\n")
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
            ToolResult::ToolOutput { tool, content } => format!("{tool}:\n{content}"),
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
    reasoning_effort: Option<(ChatReasoningEffortWire, ReasoningEffort)>,
    completer: C,
    messages: Vec<serde_json::Value>,
    tools: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatReasoningEffortWire {
    OpenRouterReasoningObject,
    OpenAiReasoningEffort,
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
            reasoning_effort: None,
            completer,
            messages: vec![
                serde_json::json!({ "role": "system", "content": system.into() }),
                serde_json::json!({ "role": "user", "content": goal.into() }),
            ],
            tools: tool_definitions(),
        }
    }

    pub fn with_tools(mut self, tools: serde_json::Value) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_openrouter_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some((ChatReasoningEffortWire::OpenRouterReasoningObject, effort));
        self
    }

    pub fn with_openrouter_reasoning_effort_if_some(self, effort: Option<ReasoningEffort>) -> Self {
        match effort {
            Some(effort) => self.with_openrouter_reasoning_effort(effort),
            None => self,
        }
    }

    pub fn with_openai_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some((ChatReasoningEffortWire::OpenAiReasoningEffort, effort));
        self
    }

    pub fn with_chat_reasoning_effort_if_some(
        mut self,
        wire: Option<ChatReasoningEffortWire>,
        effort: ReasoningEffort,
    ) -> Self {
        if let Some(wire) = wire {
            self.reasoning_effort = Some((wire, effort));
        }
        self
    }

    fn request_body(&self) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "tools": self.tools.clone(),
            "messages": self.messages,
        });
        if let Some((wire, effort)) = self.reasoning_effort {
            match wire {
                ChatReasoningEffortWire::OpenRouterReasoningObject => {
                    body["reasoning"] = serde_json::json!({
                        "effort": openrouter_reasoning_effort_value(effort),
                    });
                }
                ChatReasoningEffortWire::OpenAiReasoningEffort => {
                    body["reasoning_effort"] = serde_json::Value::String(
                        openai_reasoning_effort_value(effort).to_string(),
                    );
                }
            }
        }
        body
    }
}

pub fn openrouter_supported_reasoning_efforts() -> Vec<ReasoningEffort> {
    vec![
        ReasoningEffort::Quick,
        ReasoningEffort::Standard,
        ReasoningEffort::Deep,
        ReasoningEffort::Maximum,
    ]
}

pub fn openrouter_reasoning_effort_value(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Quick => "low",
        ReasoningEffort::Standard => "medium",
        ReasoningEffort::Deep => "high",
        ReasoningEffort::Maximum => "xhigh",
    }
}

pub fn openai_reasoning_effort_value(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Quick => "low",
        ReasoningEffort::Standard => "medium",
        ReasoningEffort::Deep => "high",
        ReasoningEffort::Maximum => "xhigh",
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
                if let AgentStep::Failed {
                    code,
                    message,
                    retryable: true,
                } = &step
                {
                    if code == "malformed_tool_call" {
                        self.messages.push(serde_json::json!({
                            "role": "user",
                            "content": format!(
                                "The previous tool call was malformed: {message}. Retry exactly once with valid JSON arguments matching the selected tool schema."
                            ),
                        }));
                        return match self.completer.complete(&self.request_body()) {
                            Ok(retry_response) => {
                                let retry_step = parse_step(&retry_response);
                                if let Some(message) = retry_response.pointer("/choices/0/message")
                                {
                                    self.messages.push(message.clone());
                                }
                                retry_step
                            }
                            Err(error) => failed_step(
                                "provider_call_failed",
                                format!("The model call failed: {}", redact(&error.to_string())),
                                true,
                            ),
                        };
                    }
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

fn redact_with_secrets(s: &str, secrets: &[&str]) -> String {
    let mut redacted = redact(s);
    for secret in secrets {
        if !secret.trim().is_empty() {
            redacted = redacted.replace(secret, "[REDACTED]");
        }
    }
    redacted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CollectingSink;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn request(prompt: &str) -> AgentRunRequest {
        AgentRunRequest {
            workspace: PathBuf::from("."),
            project_id: "p1".to_string(),
            conversation_id: "c1".to_string(),
            turn_id: "t1".to_string(),
            run_id: "r1".to_string(),
            stream_id: "s1".to_string(),
            patch_lease: None,
            cancellation_token: None,
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

    #[test]
    fn cancelled_direct_adapter_request_does_not_start_provider_call() {
        let adapter = OpenRouterAdapter {
            model: "openai/gpt-4o-mini".to_string(),
            api_key_env: "OPENSKS_TEST_CANCELLED_ABSENT_KEY_ENV".to_string(),
            base_url: DEFAULT_BASE_URL.to_string(),
            max_tokens: 5,
        };
        let cancellation_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let mut request = request("hi");
        request.cancellation_token = Some(cancellation_token);
        let sink = CollectingSink::new();

        let outcome = adapter.run(&request, &sink).unwrap();

        assert_eq!(outcome.final_state, RunProjectionState::Cancelled);
        assert!(outcome.assistant_text.contains("cancelled"));
        assert!(sink.events().iter().any(|event| {
            event.kind == AgentEventKind::Warning && event.payload["code"] == "run_cancelled"
        }));
        assert!(
            !sink
                .events()
                .iter()
                .any(|event| event.payload.to_string().contains("MissingApiKey")),
            "pre-cancelled requests must not read the credential path or attempt provider dispatch"
        );
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
    fn tool_driver_retries_one_malformed_tool_call() {
        let malformed = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_bad",
                        "type": "function",
                        "function": {
                            "name": "workspace__propose_patch",
                            "arguments": ""
                        }
                    }]
                }
            }]
        });
        let valid = tool_call_response(
            "workspace__propose_patch",
            serde_json::json!({"path": "RESULT.md", "content": "ok"}),
        );
        let completer = ScriptedCompleter::new(vec![malformed, valid]);
        let mut driver = OpenRouterToolDriver::new("model", 128, completer, "system", "goal");

        match driver.next_step(&[]) {
            AgentStep::Tools(calls) => {
                assert_eq!(calls.len(), 1);
                assert!(matches!(calls[0], ToolCall::ProposePatch { .. }));
            }
            other => panic!("expected retry to recover tool call, got {other:?}"),
        }
    }

    #[test]
    fn responses_body_maps_chat_messages_tools_and_reasoning() {
        let chat_body = serde_json::json!({
            "model": "gpt-5.5",
            "max_tokens": 42,
            "reasoning_effort": "xhigh",
            "messages": [
                {"role": "system", "content": "Use tools."},
                {"role": "user", "content": "Write a file."}
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "workspace.propose_patch",
                    "description": "write a file",
                    "parameters": {
                        "type": "object",
                        "properties": {"path": {"type": "string"}}
                    }
                }
            }]
        });

        let responses = responses_body_from_chat_body(&chat_body).expect("responses body");

        assert_eq!(responses["model"], "gpt-5.5");
        assert_eq!(responses["stream"], false);
        assert_eq!(responses["max_output_tokens"], 42);
        assert_eq!(responses["reasoning"]["effort"], "xhigh");
        assert_eq!(responses["input"][0]["role"], "system");
        assert_eq!(responses["input"][1]["content"], "Write a file.");
        assert_eq!(responses["tools"][0]["type"], "function");
        assert_eq!(responses["tools"][0]["name"], "workspace.propose_patch");
        assert!(responses["tools"][0].get("function").is_none());
    }

    #[test]
    fn responses_output_text_maps_to_chat_completion_message() {
        let response = serde_json::json!({
            "id": "resp_1",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "done"
                }]
            }],
            "usage": {"total_tokens": 7}
        });

        let chat = chat_completion_from_responses_body(&response).expect("chat response");

        assert_eq!(chat["choices"][0]["message"]["role"], "assistant");
        assert_eq!(chat["choices"][0]["message"]["content"], "done");
        assert_eq!(chat["choices"][0]["finish_reason"], "stop");
        assert_eq!(chat["usage"]["total_tokens"], 7);
    }

    #[test]
    fn responses_function_call_maps_to_chat_tool_call() {
        let response = serde_json::json!({
            "id": "resp_1",
            "output": [{
                "type": "function_call",
                "call_id": "call_123",
                "name": "workspace.propose_patch",
                "arguments": "{\"path\":\"RESULT.md\",\"content\":\"ok\"}"
            }]
        });

        let chat = chat_completion_from_responses_body(&response).expect("chat response");

        let message = &chat["choices"][0]["message"];
        assert!(message["content"].is_null());
        assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(message["tool_calls"][0]["id"], "call_123");
        assert_eq!(
            message["tool_calls"][0]["function"]["name"],
            "workspace.propose_patch"
        );
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"RESULT.md\",\"content\":\"ok\"}"
        );
    }

    #[test]
    fn responses_sse_completed_event_maps_to_chat_completion() {
        let sse = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}]}}\n\n",
            "data: [DONE]\n",
        );

        let parsed = parse_responses_transport_body(sse).expect("parse sse");
        let chat = chat_completion_from_responses_body(&parsed).expect("chat response");

        assert_eq!(chat["choices"][0]["message"]["content"], "ok");
    }

    #[test]
    fn responses_sse_uses_output_item_when_completed_response_is_empty() {
        let sse = concat!(
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"from item\"}]}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"output\":[]}}\n\n",
            "data: [DONE]\n",
        );

        let parsed = parse_responses_transport_body(sse).expect("parse sse");
        let chat = chat_completion_from_responses_body(&parsed).expect("chat response");

        assert_eq!(chat["choices"][0]["message"]["content"], "from item");
    }

    #[test]
    fn responses_sse_collects_function_call_argument_deltas() {
        let sse = concat!(
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{\\\"path\\\":\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"\\\"RESULT.md\\\"}\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"workspace.propose_patch\",\"arguments\":\"\"}}\n\n",
            "data: [DONE]\n",
        );

        let parsed = parse_responses_transport_body(sse).expect("parse sse");
        let chat = chat_completion_from_responses_body(&parsed).expect("chat response");

        assert_eq!(
            chat["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"RESULT.md\"}"
        );
    }

    #[test]
    fn responses_sse_ignores_provisional_empty_function_call_items() {
        let sse = concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"workspace.propose_patch\",\"arguments\":\"\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_1\",\"output_index\":0,\"arguments\":\"{\\\"path\\\":\\\"RESULT.md\\\",\\\"content\\\":\\\"ok\\\"}\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"workspace.propose_patch\",\"arguments\":\"\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"output\":[]}}\n\n",
            "data: [DONE]\n",
        );

        let parsed = parse_responses_transport_body(sse).expect("parse sse");
        let chat = chat_completion_from_responses_body(&parsed).expect("chat response");
        let tool_calls = chat["choices"][0]["message"]["tool_calls"]
            .as_array()
            .expect("tool calls");

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(
            tool_calls[0]["function"]["arguments"],
            "{\"path\":\"RESULT.md\",\"content\":\"ok\"}"
        );
    }

    #[test]
    fn parse_tool_arguments_accepts_fenced_json() {
        let parsed = parse_tool_arguments_string(
            "```json\n{\"path\":\"RESULT.md\",\"content\":\"ok\"}\n```",
        )
        .expect("parse fenced json");

        assert_eq!(parsed["path"], "RESULT.md");
        assert_eq!(parsed["content"], "ok");
    }

    #[test]
    fn parse_tool_arguments_treats_empty_as_empty_object() {
        let parsed = parse_tool_arguments_string("").expect("parse empty arguments");

        assert_eq!(parsed, serde_json::json!({}));
    }

    fn test_image_generator(
        response_body: String,
    ) -> (OpenAiCompatibleImageGenerator, Arc<Mutex<Vec<String>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured_requests = Arc::clone(&requests);
        let generator =
            OpenAiCompatibleImageGenerator::new("http://127.0.0.1/v1", "sk-test-secret")
                .expect("image generator")
                .with_test_http(move |request| {
                    captured_requests
                        .lock()
                        .expect("capture request")
                        .push(render_test_http_request(&request));
                    Ok(response_body.clone())
                });
        (generator, requests)
    }

    fn render_test_http_request(request: &TestHttpRequest) -> String {
        let path = reqwest::Url::parse(&request.endpoint)
            .map(|url| {
                let mut path = url.path().to_string();
                if let Some(query) = url.query() {
                    path.push('?');
                    path.push_str(query);
                }
                path
            })
            .unwrap_or_else(|_| request.endpoint.clone());
        format!(
            "POST {path} HTTP/1.1\r\nUser-Agent: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\n\r\n{}",
            request.user_agent,
            request.bearer_token,
            serde_json::to_string(&request.body).expect("json body")
        )
    }

    #[test]
    fn openai_compatible_image_generator_decodes_base64_image_response() {
        let png = b"\x89PNG\r\n\x1a\nopensks-adapter-image";
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);
        let (generator, requests) = test_image_generator(format!(
            r#"{{"created":1,"data":[{{"b64_json":"{encoded}"}}]}}"#
        ));
        let receipt = opensks_contracts::ModelRouteReceipt {
            provider_id: Some("provider-1".to_string()),
            model_id: Some("provider-1/image-model".to_string()),
            registry_revision: "rev-1".to_string(),
            reason_code: "route_ok".to_string(),
            requested_capabilities: opensks_contracts::CapabilityRequirements::image_output(),
            effective_limits: opensks_contracts::ModelLimits::default(),
            fallback_index: None,
        };
        let output = generator
            .generate_image(&ImageProviderRequest {
                provider_id: "provider-1",
                model_id: "provider-1/image-model",
                remote_model_id: "gpt-image-1.5",
                prompt: "render something durable",
                width: 1024,
                height: 1024,
                route_receipt: &receipt,
            })
            .expect("generated image");

        assert_eq!(output.bytes, png);
        assert_eq!(output.extension, "png");
        assert!(
            output
                .evidence_refs
                .contains(&"adapter:openai-compatible-images:b64_json".to_string())
        );
        let request = requests.lock().expect("captured request").join("\n");
        assert!(request.starts_with("POST /v1/images/generations "));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer sk-test-secret")
        );
        assert!(request.contains(r#""model":"gpt-image-1.5""#));
        assert!(request.contains(r#""size":"1024x1024""#));
        assert!(request.contains(r#""prompt":"render something durable""#));
    }

    #[test]
    fn openai_compatible_image_generator_sends_vision_data_url_request() {
        let (generator, requests) = test_image_generator(
            serde_json::json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "The image shows a test fixture."
                    }
                }]
            })
            .to_string(),
        );
        let receipt = opensks_contracts::ModelRouteReceipt {
            provider_id: Some("provider-1".to_string()),
            model_id: Some("provider-1/vision-model".to_string()),
            registry_revision: "rev-1".to_string(),
            reason_code: "route_ok".to_string(),
            requested_capabilities: opensks_contracts::CapabilityRequirements {
                vision_input: true,
                ..opensks_contracts::CapabilityRequirements::default()
            },
            effective_limits: opensks_contracts::ModelLimits::default(),
            fallback_index: None,
        };
        let output = opensks_image::ImageInspectionClient::inspect_image(
            &generator,
            &ImageInspectionProviderRequest {
                provider_id: "provider-1",
                model_id: "provider-1/vision-model",
                remote_model_id: "gpt-vision-1.5",
                asset_id: "fixture-image",
                content_hash: "sha256:v1:image",
                mime_type: "image/png",
                bytes: b"\x89PNG\r\n\x1a\nopensks-vision",
                prompt: "Describe the image",
                route_receipt: &receipt,
            },
        )
        .expect("vision output");

        assert_eq!(output.text, "The image shows a test fixture.");
        assert!(
            output
                .evidence_refs
                .contains(&"adapter:openai-compatible-vision:chat-completions".to_string())
        );
        let request = requests.lock().expect("captured request").join("\n");
        assert!(request.starts_with("POST /v1/chat/completions "));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer sk-test-secret")
        );
        assert!(request.contains(r#""model":"gpt-vision-1.5""#));
        assert!(request.contains(r#""type":"image_url""#));
        assert!(request.contains("data:image/png;base64,"));
        assert!(request.contains("Describe the image"));
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
                vec![ToolCall::DiffPatch {
                    path: "NOTES.md".to_string(),
                    value: "two".to_string()
                }]
            ),
            other => panic!("expected tools, got {other:?}"),
        }
    }

    #[test]
    fn tool_definitions_are_registry_backed_canonical_tools() {
        let tools = tool_definitions();
        let names = tools
            .as_array()
            .expect("tool array")
            .iter()
            .map(|tool| {
                tool.pointer("/function/name")
                    .and_then(serde_json::Value::as_str)
                    .expect("tool name")
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert!(names.contains(&"workspace__list_directory".to_string()));
        assert!(names.contains(&"workspace__read_file_range".to_string()));
        assert!(names.contains(&"workspace__search_text".to_string()));
        assert!(names.contains(&"workspace__propose_patch".to_string()));
        assert!(names.contains(&"workspace__diff_patch".to_string()));
        assert!(names.contains(&"git__status".to_string()));
        assert!(names.contains(&"git__diff".to_string()));
        assert!(names.contains(&"git__log".to_string()));
        assert!(names.contains(&"codegraph__query_symbol".to_string()));
        assert!(names.contains(&"codegraph__references".to_string()));
        assert!(names.contains(&"context__build_pack".to_string()));
        assert!(names.contains(&"command__run".to_string()));
        assert!(names.contains(&"test__run_targeted".to_string()));
        assert!(names.contains(&"mcp__invoke".to_string()));
        assert!(names.contains(&"skill__invoke".to_string()));
        assert!(names.contains(&"artifact__read".to_string()));
        assert!(names.contains(&"artifact__write".to_string()));
        assert!(!names.contains(&"append_line".to_string()));
        assert!(!names.contains(&"image__generate".to_string()));
        assert!(!names.contains(&"image__inspect".to_string()));
    }

    #[test]
    fn tool_definitions_can_selectively_advertise_image_generate() {
        let tools =
            tool_definitions_with_extra_available_tools(&["image.generate", "image.inspect"]);
        let names = tools
            .as_array()
            .expect("tool array")
            .iter()
            .map(|tool| {
                tool.pointer("/function/name")
                    .and_then(serde_json::Value::as_str)
                    .expect("tool name")
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert!(names.contains(&"image__generate".to_string()));
        assert!(names.contains(&"image__inspect".to_string()));
        let image = tools
            .as_array()
            .expect("tool array")
            .iter()
            .find(|tool| {
                tool.pointer("/function/name")
                    .and_then(serde_json::Value::as_str)
                    == Some("image__generate")
            })
            .expect("image tool");
        assert_eq!(
            image.pointer("/function/parameters/required/0"),
            Some(&serde_json::Value::String("prompt".to_string()))
        );
        let inspect = tools
            .as_array()
            .expect("tool array")
            .iter()
            .find(|tool| {
                tool.pointer("/function/name")
                    .and_then(serde_json::Value::as_str)
                    == Some("image__inspect")
            })
            .expect("inspect tool");
        assert_eq!(
            inspect.pointer("/function/parameters/required/0"),
            Some(&serde_json::Value::String("artifact_ref".to_string()))
        );
    }

    #[test]
    fn parse_step_maps_canonical_tool_calls() {
        let resp = tool_call_response(
            "workspace__search_text",
            serde_json::json!({ "query": "needle", "max_results": 3 }),
        );
        match parse_step(&resp) {
            AgentStep::Tools(calls) => assert_eq!(
                calls,
                vec![ToolCall::SearchText {
                    query: "needle".to_string(),
                    path: ".".to_string(),
                    max_results: Some(3),
                }]
            ),
            other => panic!("expected tools, got {other:?}"),
        }
    }

    #[test]
    fn parse_step_maps_canonical_git_tool_calls() {
        let resp = tool_call_response("git__log", serde_json::json!({ "max_count": 2 }));
        match parse_step(&resp) {
            AgentStep::Tools(calls) => {
                assert_eq!(calls, vec![ToolCall::GitLog { max_count: Some(2) }])
            }
            other => panic!("expected tools, got {other:?}"),
        }
    }

    #[test]
    fn parse_step_maps_canonical_codegraph_tool_calls() {
        let resp = serde_json::json!({
            "choices": [{
                "message": {
                    "tool_calls": [
                        {
                            "function": {
                                "name": "codegraph__query_symbol",
                                "arguments": serde_json::json!({
                                    "query": "ProviderStore",
                                    "max_results": 3
                                }).to_string()
                            }
                        },
                        {
                            "function": {
                                "name": "codegraph__references",
                                "arguments": serde_json::json!({
                                    "symbol_id": "src/lib.rs:1:ProviderStore"
                                }).to_string()
                            }
                        }
                    ]
                }
            }]
        });
        match parse_step(&resp) {
            AgentStep::Tools(calls) => assert_eq!(
                calls,
                vec![
                    ToolCall::CodeGraphQuerySymbol {
                        query: "ProviderStore".to_string(),
                        max_results: Some(3),
                    },
                    ToolCall::CodeGraphReferences {
                        symbol_id: "src/lib.rs:1:ProviderStore".to_string(),
                    },
                ]
            ),
            other => panic!("expected tools, got {other:?}"),
        }
    }

    #[test]
    fn parse_step_maps_canonical_context_tool_calls() {
        let resp = tool_call_response(
            "context__build_pack",
            serde_json::json!({ "id": "worker-a", "token_budget": 128 }),
        );
        match parse_step(&resp) {
            AgentStep::Tools(calls) => assert_eq!(
                calls,
                vec![ToolCall::ContextBuildPack {
                    id: "worker-a".to_string(),
                    token_budget: Some(128),
                }]
            ),
            other => panic!("expected tools, got {other:?}"),
        }
    }

    #[test]
    fn parse_step_maps_canonical_command_tool_calls() {
        let resp = serde_json::json!({
            "choices": [{
                "message": {
                    "tool_calls": [
                        {
                            "function": {
                                "name": "command__run",
                                "arguments": serde_json::json!({
                                    "command": "git status --short",
                                    "timeout_ms": 1000
                                }).to_string()
                            }
                        },
                        {
                            "function": {
                                "name": "test__run_targeted",
                                "arguments": serde_json::json!({
                                    "target": "cargo test -p opensks-adapter parse_step --locked",
                                    "timeout_ms": 30000
                                }).to_string()
                            }
                        }
                    ]
                }
            }]
        });
        match parse_step(&resp) {
            AgentStep::Tools(calls) => assert_eq!(
                calls,
                vec![
                    ToolCall::CommandRun {
                        command: "git status --short".to_string(),
                        timeout_ms: Some(1000),
                    },
                    ToolCall::TestRunTargeted {
                        target: "cargo test -p opensks-adapter parse_step --locked".to_string(),
                        timeout_ms: Some(30000),
                    },
                ]
            ),
            other => panic!("expected tools, got {other:?}"),
        }
    }

    #[test]
    fn parse_step_maps_canonical_mcp_tool_call() {
        let resp = tool_call_response(
            "mcp__invoke",
            serde_json::json!({
                "tool": "opensks.repo.search",
                "payload": {
                    "query": "ProviderStore",
                    "limit": 5
                }
            }),
        );
        match parse_step(&resp) {
            AgentStep::Tools(calls) => assert_eq!(
                calls,
                vec![ToolCall::McpInvoke {
                    tool_name: "opensks.repo.search".to_string(),
                    payload: serde_json::json!({
                        "query": "ProviderStore",
                        "limit": 5
                    }),
                }]
            ),
            other => panic!("expected tools, got {other:?}"),
        }
    }

    #[test]
    fn parse_step_maps_canonical_skill_tool_call() {
        let resp = tool_call_response(
            "skill__invoke",
            serde_json::json!({
                "skill": "goal",
                "payload": {
                    "objective": "continue"
                }
            }),
        );
        match parse_step(&resp) {
            AgentStep::Tools(calls) => assert_eq!(
                calls,
                vec![ToolCall::SkillInvoke {
                    skill: "goal".to_string(),
                    payload: serde_json::json!({
                        "objective": "continue"
                    }),
                }]
            ),
            other => panic!("expected tools, got {other:?}"),
        }
    }

    #[test]
    fn parse_step_maps_canonical_artifact_tool_calls() {
        let resp = serde_json::json!({
            "choices": [{
                "message": {
                    "tool_calls": [
                        {
                            "function": {
                                "name": "artifact__read",
                                "arguments": serde_json::json!({
                                    "artifact_ref": "artifact://.opensks/runtime/report.json"
                                }).to_string()
                            }
                        },
                        {
                            "function": {
                                "name": "artifact__write",
                                "arguments": serde_json::json!({
                                    "artifact_ref": "artifact://.opensks/runtime/report.json",
                                    "content": "{\"ok\":true}"
                                }).to_string()
                            }
                        }
                    ]
                }
            }]
        });
        match parse_step(&resp) {
            AgentStep::Tools(calls) => assert_eq!(
                calls,
                vec![
                    ToolCall::ArtifactRead {
                        artifact_ref: "artifact://.opensks/runtime/report.json".to_string(),
                    },
                    ToolCall::ArtifactWrite {
                        artifact_ref: "artifact://.opensks/runtime/report.json".to_string(),
                        content: "{\"ok\":true}".to_string(),
                    },
                ]
            ),
            other => panic!("expected tools, got {other:?}"),
        }
    }

    #[test]
    fn parse_step_maps_canonical_image_generate_tool_call() {
        let resp = tool_call_response(
            "image__generate",
            serde_json::json!({
                "prompt": "render a product screenshot",
                "asset_id": "hero",
                "width": 1536,
                "height": 864
            }),
        );
        match parse_step(&resp) {
            AgentStep::Tools(calls) => assert_eq!(
                calls,
                vec![ToolCall::ImageGenerate {
                    prompt: "render a product screenshot".to_string(),
                    asset_id: Some("hero".to_string()),
                    width: Some(1536),
                    height: Some(864),
                }]
            ),
            other => panic!("expected tools, got {other:?}"),
        }
    }

    #[test]
    fn parse_step_maps_canonical_image_inspect_tool_call() {
        let resp = tool_call_response(
            "image__inspect",
            serde_json::json!({
                "artifact_ref": "artifact://.opensks/assets/candidates/hero.png",
                "prompt": "Describe the product UI"
            }),
        );
        match parse_step(&resp) {
            AgentStep::Tools(calls) => assert_eq!(
                calls,
                vec![ToolCall::ImageInspect {
                    artifact_ref: Some(
                        "artifact://.opensks/assets/candidates/hero.png".to_string()
                    ),
                    asset_id: None,
                    prompt: Some("Describe the product UI".to_string()),
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
    fn provider_reasoning_effort_maps_turn_settings_to_reasoning_body() {
        assert_eq!(
            openrouter_reasoning_effort_value(ReasoningEffort::Quick),
            "low"
        );
        assert_eq!(
            openrouter_reasoning_effort_value(ReasoningEffort::Standard),
            "medium"
        );
        assert_eq!(
            openrouter_reasoning_effort_value(ReasoningEffort::Deep),
            "high"
        );
        assert_eq!(
            openrouter_reasoning_effort_value(ReasoningEffort::Maximum),
            "xhigh"
        );

        let driver = OpenRouterToolDriver::new(
            "openrouter/test",
            256,
            ScriptedCompleter::new(vec![]),
            "system",
            "goal",
        )
        .with_openrouter_reasoning_effort(ReasoningEffort::Maximum);
        let body = driver.request_body();

        assert_eq!(body["reasoning"]["effort"], "xhigh");
        assert_eq!(body["model"], "openrouter/test");
        assert_eq!(body["max_tokens"], 256);

        let openai_driver = OpenRouterToolDriver::new(
            "openai/test",
            256,
            ScriptedCompleter::new(vec![]),
            "system",
            "goal",
        )
        .with_openai_reasoning_effort(ReasoningEffort::Deep);
        let openai_body = openai_driver.request_body();

        assert_eq!(openai_body["reasoning_effort"], "high");
        assert!(openai_body.get("reasoning").is_none());
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
            patch_lease: None,
            cancellation_token: None,
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
            patch_lease: None,
            cancellation_token: None,
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
