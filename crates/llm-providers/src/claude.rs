use crate::{LlmError, LlmMessage, LlmProvider, LlmRole};
use async_trait::async_trait;
use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::instrument;

const CLAUDE_API_URL: &str = "https://api.anthropic.com/v1/messages";

pub struct ClaudeProvider {
    client: Client,
    api_key: String,
    model: String,
}

impl ClaudeProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
        }
    }
}

#[derive(Debug, Serialize)]
struct ClaudeRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<ClaudeMessage>,
}

#[derive(Debug, Serialize)]
struct ClaudeMessage {
    role: String,
    content: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct ClaudeResponse {
    content: Vec<ClaudeContent>,
}

#[derive(Debug, Deserialize)]
struct ClaudeContent {
    text: Option<String>,
}

#[async_trait]
impl LlmProvider for ClaudeProvider {
    fn name(&self) -> &str {
        "claude"
    }

    #[instrument(skip(self, messages))]
    async fn complete(&self, messages: &[LlmMessage]) -> Result<String, LlmError> {
        let (system, messages) = extract_system_message(messages);

        let claude_messages: Vec<ClaudeMessage> = messages
            .iter()
            .map(|m| ClaudeMessage {
                role: match m.role {
                    LlmRole::User => "user".to_string(),
                    LlmRole::Assistant => "assistant".to_string(),
                    LlmRole::System => "user".to_string(),
                },
                content: serde_json::Value::String(m.content.clone()),
            })
            .collect();

        let request = ClaudeRequest {
            model: self.model.clone(),
            max_tokens: 4096,
            system,
            messages: claude_messages,
        };

        let response = self
            .client
            .post(CLAUDE_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!(
                "Claude API error {status}: {error_text}"
            )));
        }

        let claude_response: ClaudeResponse = response.json().await?;

        claude_response
            .content
            .first()
            .and_then(|c| c.text.clone())
            .ok_or_else(|| LlmError::InvalidResponse("No text content in response".into()))
    }

    #[instrument(skip(self, image_data))]
    async fn analyze_image(
        &self,
        image_data: &[u8],
        mime_type: &str,
        prompt: &str,
    ) -> Result<String, LlmError> {
        let base64_image = base64::engine::general_purpose::STANDARD.encode(image_data);

        let content = serde_json::json!([
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": mime_type,
                    "data": base64_image
                }
            },
            {
                "type": "text",
                "text": prompt
            }
        ]);

        let request = ClaudeRequest {
            model: self.model.clone(),
            max_tokens: 4096,
            system: None,
            messages: vec![ClaudeMessage {
                role: "user".to_string(),
                content,
            }],
        };

        let response = self
            .client
            .post(CLAUDE_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!(
                "Claude API error {status}: {error_text}"
            )));
        }

        let claude_response: ClaudeResponse = response.json().await?;

        claude_response
            .content
            .first()
            .and_then(|c| c.text.clone())
            .ok_or_else(|| LlmError::InvalidResponse("No text content in response".into()))
    }
}

fn extract_system_message(messages: &[LlmMessage]) -> (Option<String>, Vec<&LlmMessage>) {
    let system = messages
        .iter()
        .find(|m| m.role == LlmRole::System)
        .map(|m| m.content.clone());

    let non_system: Vec<&LlmMessage> = messages
        .iter()
        .filter(|m| m.role != LlmRole::System)
        .collect();

    (system, non_system)
}
