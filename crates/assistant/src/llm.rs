//! LLM routing layer for the assistant.
//!
//! Wraps `crates/llm-providers` with a two-tier model selection:
//! - [`ModelTier::Main`] → conversational + tool-calling (default `gpt-oss:120b`)
//! - [`ModelTier::Cheap`] → background tasks: reflection, summarisation, consolidation
//!   (default `gemma4:31b`)
//!
//! Both names are configurable (`AUST__LLM__OLLAMA__ASSISTANT_MODEL` /
//! `…__ASSISTANT_CHEAP_MODEL`) because Ollama Cloud gates models by plan: the
//! former defaults (`kimi-k2.6`, `deepseek-v4-flash`) answer
//! `402 "this model is not included in your free usage"`. The defaults here are
//! free-plan models that still tool-call reliably and write clean German.
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
    /// Full conversational model with tool-calling support.
    Main,
    /// Cheap background model for reflection, summarisation, consolidation.
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
/// Default [`ModelTier::Main`] model: free-plan usable on Ollama Cloud, reliable
/// tool-caller and the cleanest German of the free models we benchmarked.
pub const DEFAULT_MAIN_MODEL: &str = "gpt-oss:120b";

/// Default [`ModelTier::Cheap`] model: free-plan usable, sub-second on short
/// summarisation prompts.
pub const DEFAULT_CHEAP_MODEL: &str = "gemma4:31b";

/// Default model for turns that carry images (Telegram photos, rasterized PDF
/// pages). The text models above are **not** multimodal — Ollama Cloud rejects
/// an image-bearing request to them outright with
/// `"this model does not support image input"` — so any turn with images is
/// routed here regardless of tier.
pub const DEFAULT_VISION_MODEL: &str = "gemma4:31b";

pub struct OllamaAssistantLlm {
    /// Ollama base URL for the raw `/api/chat` and `/api/embeddings` endpoints.
    base_url: String,
    /// API key for the raw `/api/chat` (tool-calling) and `/api/embeddings` requests.
    /// Ollama Cloud requires `Authorization: Bearer <key>`; without it requests 401.
    api_key: Option<String>,
    /// Model used for [`ModelTier::Main`].
    main_model: String,
    /// Model used for [`ModelTier::Cheap`].
    cheap_model: String,
    /// Model used whenever a turn carries images, overriding the tier model.
    vision_model: String,
    http: reqwest::Client,
}

impl OllamaAssistantLlm {
    /// Construct from explicit base URL, using the default free-plan models.
    /// Both tiers hit the same Ollama instance but select a different model
    /// name per [`ModelTier`]; use [`Self::with_models`] to override them.
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
            main_model: DEFAULT_MAIN_MODEL.to_string(),
            cheap_model: DEFAULT_CHEAP_MODEL.to_string(),
            vision_model: DEFAULT_VISION_MODEL.to_string(),
            http,
        }
    }

    /// Override the per-tier model names (empty strings keep the default).
    #[must_use]
    pub fn with_models(
        mut self,
        main: impl Into<String>,
        cheap: impl Into<String>,
    ) -> Self {
        let main = main.into();
        let cheap = cheap.into();
        if !main.is_empty() {
            self.main_model = main;
        }
        if !cheap.is_empty() {
            self.cheap_model = cheap;
        }
        self
    }

    /// Override the model used for image-bearing turns (empty keeps the default).
    #[must_use]
    pub fn with_vision_model(mut self, vision: impl Into<String>) -> Self {
        let vision = vision.into();
        if !vision.is_empty() {
            self.vision_model = vision;
        }
        self
    }

    /// Pick the model for a request: image-bearing turns must go to a
    /// multimodal model, because the text-only tiers reject them outright with
    /// "this model does not support image input" — which would surface as
    /// Josie going silent on a photo.
    fn select_model(&self, tier: ModelTier, messages: &[LlmMessage]) -> &str {
        if messages.iter().any(|m| !m.images.is_empty()) {
            &self.vision_model
        } else {
            self.model_name(tier)
        }
    }

    fn model_name(&self, tier: ModelTier) -> &str {
        match tier {
            ModelTier::Main => &self.main_model,
            ModelTier::Cheap => &self.cheap_model,
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
        let model = self.select_model(tier, messages).to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(content: &str, images: Vec<String>) -> LlmMessage {
        LlmMessage {
            role: aust_llm_providers::LlmRole::User,
            content: content.to_string(),
            images,
        }
    }

    #[test]
    fn defaults_are_free_plan_models() {
        let llm = OllamaAssistantLlm::new("https://ollama.com", None);
        assert_eq!(llm.model_name(ModelTier::Main), DEFAULT_MAIN_MODEL);
        assert_eq!(llm.model_name(ModelTier::Cheap), DEFAULT_CHEAP_MODEL);
        assert_eq!(llm.vision_model, DEFAULT_VISION_MODEL);
    }

    #[test]
    fn with_models_overrides_both_tiers() {
        let llm = OllamaAssistantLlm::new("https://ollama.com", None)
            .with_models("model-a", "model-b")
            .with_vision_model("model-c");
        assert_eq!(llm.model_name(ModelTier::Main), "model-a");
        assert_eq!(llm.model_name(ModelTier::Cheap), "model-b");
        assert_eq!(llm.vision_model, "model-c");
    }

    #[test]
    fn empty_override_keeps_the_default() {
        let llm = OllamaAssistantLlm::new("https://ollama.com", None)
            .with_models("", "")
            .with_vision_model("");
        assert_eq!(llm.model_name(ModelTier::Main), DEFAULT_MAIN_MODEL);
        assert_eq!(llm.model_name(ModelTier::Cheap), DEFAULT_CHEAP_MODEL);
        assert_eq!(llm.vision_model, DEFAULT_VISION_MODEL);
    }

    /// The text tiers reject image payloads outright, so a turn carrying an
    /// image must be sent to the vision model even on the Main tier.
    #[test]
    fn images_select_the_vision_model() {
        let llm = OllamaAssistantLlm::new("https://ollama.com", None)
            .with_models("text-main", "text-cheap")
            .with_vision_model("sees-pictures");

        let with_image = [msg("Was ist das?", vec!["base64".to_string()])];
        let text_only = [msg("Moin", vec![])];

        assert_eq!(llm.select_model(ModelTier::Main, &with_image), "sees-pictures");
        assert_eq!(llm.select_model(ModelTier::Cheap, &with_image), "sees-pictures");
        assert_eq!(llm.select_model(ModelTier::Main, &text_only), "text-main");
        assert_eq!(llm.select_model(ModelTier::Cheap, &text_only), "text-cheap");
    }
}
