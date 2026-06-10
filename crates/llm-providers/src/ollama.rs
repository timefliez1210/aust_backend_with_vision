use async_trait::async_trait;
use crate::{LlmError, LlmMessage, LlmProvider, LlmRole};
use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::instrument;

pub struct OllamaProvider {
    client: Client,
    base_url: String,
    model: String,
    api_key: Option<String>,
}

impl OllamaProvider {
    pub fn new(base_url: String, model: String) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("Failed to create HTTP client"),
            base_url,
            model,
            api_key: None,
        }
    }

    pub fn with_api_key(base_url: String, model: String, api_key: String) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("Failed to create HTTP client"),
            base_url,
            model,
            api_key: Some(api_key),
        }
    }
}

#[derive(Debug, Serialize)]
struct OllamaRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct OllamaMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    message: OllamaResponseMessage,
}

#[derive(Debug, Deserialize)]
struct OllamaResponseMessage {
    content: String,
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    #[instrument(skip(self, messages))]
    async fn complete(&self, messages: &[LlmMessage]) -> Result<String, LlmError> {
        let ollama_messages: Vec<OllamaMessage> = messages
            .iter()
            .map(|m| OllamaMessage {
                role: match m.role {
                    LlmRole::System => "system".to_string(),
                    LlmRole::User => "user".to_string(),
                    LlmRole::Assistant => "assistant".to_string(),
                },
                content: m.content.clone(),
                images: if m.images.is_empty() {
                    None
                } else {
                    Some(m.images.clone())
                },
            })
            .collect();

        let request = OllamaRequest {
            model: self.model.clone(),
            messages: ollama_messages,
            stream: false,
            options: None,
        };

        self.send_request(&request).await
    }

    #[instrument(skip(self, image_data))]
    async fn analyze_image(
        &self,
        image_data: &[u8],
        _mime_type: &str,
        prompt: &str,
    ) -> Result<String, LlmError> {
        let base64_image = base64::engine::general_purpose::STANDARD.encode(image_data);

        let request = OllamaRequest {
            model: self.model.clone(),
            messages: vec![OllamaMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
                images: Some(vec![base64_image]),
            }],
            stream: false,
            options: None,
        };

        self.send_request(&request).await
    }
}

impl OllamaProvider {
    /// Multi-turn completion with NDJSON streaming accumulate and a per-request
    /// wall-clock timeout, deterministic (`temperature: 0`).
    ///
    /// **Caller**: `aust_volume_estimator::VlmEstimator` for catalogue-based
    /// volume estimation over full apartment photo sets.
    ///
    /// **Why streaming**: thinking-heavy cloud models (minimax-m3 needs ~14 min
    /// on a 59-photo set) stall on `stream: false` — the idle connection gets
    /// killed upstream and the client never receives a byte. With streaming the
    /// connection stays active for the whole generation, so only the overall
    /// `timeout` matters (it overrides the client's 60 s default).
    pub async fn complete_streaming(
        &self,
        messages: &[LlmMessage],
        timeout: std::time::Duration,
    ) -> Result<String, LlmError> {
        use futures::StreamExt;

        let ollama_messages: Vec<OllamaMessage> = messages
            .iter()
            .map(|m| OllamaMessage {
                role: match m.role {
                    LlmRole::System => "system".to_string(),
                    LlmRole::User => "user".to_string(),
                    LlmRole::Assistant => "assistant".to_string(),
                },
                content: m.content.clone(),
                images: if m.images.is_empty() {
                    None
                } else {
                    Some(m.images.clone())
                },
            })
            .collect();

        let request = OllamaRequest {
            model: self.model.clone(),
            messages: ollama_messages,
            stream: true,
            options: Some(serde_json::json!({"temperature": 0})),
        };

        let url = format!("{}/api/chat", self.base_url);
        let mut req = self
            .client
            .post(&url)
            .timeout(timeout)
            .header("Content-Type", "application/json");
        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }

        let response = req.json(&request).send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!(
                "Ollama API error {status}: {error_text}"
            )));
        }

        let mut stream = response.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut content = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            buf.extend_from_slice(&chunk);
            // NDJSON: one JSON object per newline-terminated line.
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line);
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let v: serde_json::Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("Unparseable Ollama stream line ({e}): {line:.200}");
                        continue;
                    }
                };
                if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
                    return Err(LlmError::Api(format!("Ollama stream error: {err}")));
                }
                if let Some(c) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                {
                    content.push_str(c);
                }
            }
        }

        Ok(content)
    }

    async fn send_request(&self, request: &OllamaRequest) -> Result<String, LlmError> {
        let url = format!("{}/api/chat", self.base_url);

        let mut req = self
            .client
            .post(&url)
            .header("Content-Type", "application/json");

        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }

        let response = req.json(request).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!(
                "Ollama API error {status}: {error_text}"
            )));
        }

        let ollama_response: OllamaResponse = response.json().await?;
        Ok(ollama_response.message.content)
    }
}
