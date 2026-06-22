//! LLM routing layer for the assistant.
//!
//! Wraps `crates/llm-providers` with a two-tier model selection:
//! - [`ModelTier::Main`] → `kimi-k2.6` (conversational + tool-calling)
//! - [`ModelTier::Cheap`] → `deepseek-v4-flash` (background tasks: reflection, summarisation,
//!   consolidation)
//!
//! The `AssistantLlm` struct holds two pre-configured provider instances. All callers
//! go through this facade rather than directly referencing the LLM provider.

use async_trait::async_trait;
use aust_llm_providers::LlmMessage;
use serde_json::Value;

use crate::error::{AssistantError, Result};

/// Which LLM tier to use for a given request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTier {
    /// Full conversational model with tool-calling support (kimi-k2.6).
    Main,
    /// Cheap background model for reflection, summarisation, consolidation (deepseek-v4-flash).
    Cheap,
}

/// Tool schema descriptor passed to the LLM for tool-calling.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolSchema {
    /// Machine-readable tool name.
    pub name: String,
    /// German description shown to the model.
    pub description: String,
    /// JSON Schema object describing the tool's parameters.
    pub parameters: Value,
}

/// A tool call parsed from the LLM response.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ToolCall {
    /// Name of the tool the model wants to invoke.
    pub name: String,
    /// Arguments as a JSON object.
    pub arguments: Value,
}

/// Response from a tool-calling LLM request.
#[derive(Debug, Clone)]
pub enum ChatResponse {
    /// The model produced a plain text reply.
    Text(String),
    /// The model wants to call one or more tools.
    ToolCalls(Vec<ToolCall>),
}

/// Pluggable LLM interface used by the assistant.
///
/// Separate from `LlmProvider` because the assistant needs tool-calling and
/// embedding operations that the base trait does not expose.
#[async_trait]
pub trait AssistantLlmProvider: Send + Sync {
    /// Generate a plain text completion.
    async fn chat(&self, tier: ModelTier, messages: &[LlmMessage]) -> Result<String>;

    /// Generate a completion with optional tool schemas; returns text or tool calls.
    async fn chat_with_tools(
        &self,
        tier: ModelTier,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
    ) -> Result<ChatResponse>;

    /// Produce a 768-dimensional embedding vector for the given text.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
}

/// Production implementation backed by two Ollama Cloud endpoints.
///
/// Every chat operation — plain completion (`chat`), tool-calling
/// (`chat_with_tools`) and embeddings — goes through the single retrying HTTP
/// path (`post_json_with_retry`). There is deliberately no fallback to
/// `OllamaProvider::complete()`: that path is non-retrying with a fixed 60 s
/// timeout, which is the failure mode behind the email auto-responder's
/// "Network error: error sending request for url (…/api/chat)" — a long
/// generation on Ollama Cloud whose idle connection gets killed mid-flight.
pub struct OllamaAssistantLlm {
    /// Ollama base URL for the raw `/api/chat` and `/api/embeddings` endpoints.
    base_url: String,
    /// API key for the raw `/api/chat` (tool-calling) and `/api/embeddings` requests.
    /// Ollama Cloud requires `Authorization: Bearer <key>`; without it requests 401.
    api_key: Option<String>,
    http: reqwest::Client,
}

impl OllamaAssistantLlm {
    /// Construct from explicit base URL. Both tiers hit the same Ollama instance
    /// but select a different model name per [`ModelTier`].
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        let url = base_url.into();
        // Generous per-request ceiling: conversational/email generations on the
        // Main tier can run well past the old 60 s. Embeddings still return in
        // milliseconds — the timeout is a cap, not a wait — so one client is fine.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .expect("reqwest client");
        Self {
            base_url: url,
            api_key: api_key.filter(|k| !k.is_empty()),
            http,
        }
    }

    fn model_name(&self, tier: ModelTier) -> &'static str {
        match tier {
            ModelTier::Main => "kimi-k2.6",
            ModelTier::Cheap => "deepseek-v4-flash",
        }
    }

    /// POST `body` to `url` with the Bearer header, retrying transient failures
    /// before giving up. Returns the parsed JSON body on success.
    ///
    /// Why: Ollama Cloud over the VPS link occasionally drops a single request
    /// (connection reset / gateway 5xx). A single conversational turn makes
    /// several of these calls, so one blip used to silently kill the whole
    /// reply (the bot just went quiet). We retry transient errors with a short
    /// backoff; 4xx (auth, bad request) fail fast since they won't self-heal.
    async fn post_json_with_retry(&self, url: &str, body: &Value) -> Result<Value> {
        const MAX_ATTEMPTS: usize = 3;
        let mut last_err: Option<AssistantError> = None;
        for attempt in 1..=MAX_ATTEMPTS {
            let mut req = self.http.post(url).json(body);
            if let Some(key) = &self.api_key {
                req = req.header("Authorization", format!("Bearer {key}"));
            }
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return resp.json().await.map_err(|e| {
                            AssistantError::Internal(format!("JSON parse error: {e}"))
                        });
                    }
                    let text = resp.text().await.unwrap_or_default();
                    let snippet: String = text.chars().take(200).collect();
                    let err = AssistantError::Internal(format!(
                        "Ollama {url} returned {status}: {snippet}"
                    ));
                    // 4xx won't fix itself (auth/bad request) — fail fast.
                    if !status.is_server_error() {
                        return Err(err);
                    }
                    last_err = Some(err);
                }
                Err(e) => {
                    last_err = Some(AssistantError::Internal(format!("HTTP error: {e}")));
                }
            }
            if attempt < MAX_ATTEMPTS {
                let backoff = std::time::Duration::from_millis(400 * attempt as u64);
                tokio::time::sleep(backoff).await;
            }
        }
        Err(last_err
            .unwrap_or_else(|| AssistantError::Internal("request failed".to_string())))
    }

    /// Core `/api/chat` request shared by `chat` and `chat_with_tools`. Builds the
    /// Ollama body (forwarding any base64 images), attaches `tools` when non-empty,
    /// posts through the retrying client and parses the response into either tool
    /// calls or plain text.
    async fn chat_core(
        &self,
        tier: ModelTier,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
    ) -> Result<ChatResponse> {
        let model = self.model_name(tier);
        let url = format!("{}/api/chat", self.base_url);

        let ollama_messages: Vec<Value> = messages
            .iter()
            .map(|m| {
                let mut msg = serde_json::json!({
                    "role": match m.role {
                        aust_llm_providers::LlmRole::System => "system",
                        aust_llm_providers::LlmRole::User => "user",
                        aust_llm_providers::LlmRole::Assistant => "assistant",
                    },
                    "content": m.content,
                });
                // Forward base64 images (photos / rasterized PDF pages) so the
                // vision-capable model can see them. Omitted when empty.
                if !m.images.is_empty() {
                    msg["images"] = serde_json::json!(m.images);
                }
                msg
            })
            .collect();

        let mut body = serde_json::json!({
            "model": model,
            "messages": ollama_messages,
            "stream": false,
        });
        if !tools.is_empty() {
            let ollama_tools: Vec<Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::json!(ollama_tools);
        }

        let json = self.post_json_with_retry(&url, &body).await?;

        // Parse Ollama response: either tool_calls or content.
        let message = &json["message"];
        if let Some(calls) = message["tool_calls"].as_array()
            && !calls.is_empty()
        {
            let tool_calls: Vec<ToolCall> = calls
                .iter()
                .filter_map(|c| {
                    let name = c["function"]["name"].as_str()?.to_string();
                    let arguments = c["function"]["arguments"].clone();
                    Some(ToolCall { name, arguments })
                })
                .collect();
            return Ok(ChatResponse::ToolCalls(tool_calls));
        }

        let text = message["content"].as_str().unwrap_or_default().to_string();
        Ok(ChatResponse::Text(text))
    }
}

#[async_trait]
impl AssistantLlmProvider for OllamaAssistantLlm {
    async fn chat(&self, tier: ModelTier, messages: &[LlmMessage]) -> Result<String> {
        match self.chat_core(tier, messages, &[]).await? {
            ChatResponse::Text(t) => Ok(t),
            // No tools were offered, so the model has nothing valid to call; treat
            // an unexpected tool-call response as empty text rather than erroring.
            ChatResponse::ToolCalls(_) => Ok(String::new()),
        }
    }

    /// Tool-calling via the Ollama `/api/chat` endpoint with `tools` parameter.
    ///
    /// The base `LlmProvider` trait does not expose tool-calling, so we issue a
    /// raw HTTP request here (through the shared retrying path) and parse the
    /// response ourselves.
    async fn chat_with_tools(
        &self,
        tier: ModelTier,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
    ) -> Result<ChatResponse> {
        self.chat_core(tier, messages, tools).await
    }

    /// Call the Ollama `/api/embeddings` endpoint with `embeddinggemma:300m`.
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let url = format!("{}/api/embeddings", self.base_url);
        let body = serde_json::json!({
            "model": "embeddinggemma:300m",
            "prompt": text,
        });

        let json = self.post_json_with_retry(&url, &body).await?;

        let embedding = json["embedding"]
            .as_array()
            .ok_or_else(|| AssistantError::Internal("No embedding in response".to_string()))?
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
            .collect();

        Ok(embedding)
    }
}

/// Mock LLM for unit tests — returns scripted responses without hitting Ollama Cloud.
pub struct MockAssistantLlm {
    /// Scripted responses returned in order (cycling if exhausted).
    pub responses: std::sync::Mutex<Vec<String>>,
}

impl MockAssistantLlm {
    /// Construct a mock that cycles through the given response strings.
    pub fn new(responses: Vec<impl Into<String>>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into_iter().map(|s| s.into()).collect()),
        }
    }

    /// Construct a mock that always returns the same response.
    pub fn always(response: impl Into<String>) -> Self {
        Self::new(vec![response.into()])
    }
}

#[async_trait]
impl AssistantLlmProvider for MockAssistantLlm {
    async fn chat(&self, _tier: ModelTier, _messages: &[LlmMessage]) -> Result<String> {
        let mut guard = self.responses.lock().unwrap();
        if guard.is_empty() {
            return Ok(String::new());
        }
        let resp = guard.remove(0);
        // Push a copy to the back so the mock cycles.
        guard.push(resp.clone());
        Ok(resp)
    }

    async fn chat_with_tools(
        &self,
        tier: ModelTier,
        messages: &[LlmMessage],
        _tools: &[ToolSchema],
    ) -> Result<ChatResponse> {
        let text = self.chat(tier, messages).await?;
        // Return as plain text — callers that need tool calls should set up
        // a mock that returns JSON and parse it themselves.
        Ok(ChatResponse::Text(text))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        // Deterministic 768-dim unit vector for tests.
        Ok(vec![0.001_f32; 768])
    }
}
