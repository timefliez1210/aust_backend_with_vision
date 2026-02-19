use crate::{LlmError, LlmMessage, LlmProvider, LlmRole};
use async_trait::async_trait;
use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::instrument;

const OPENAI_API_URL: &str = "https://api.openai.com/v1/chat/completions";

pub struct OpenAiProvider {
    client: Client,
    api_key: String,
    model: String,
}

impl OpenAiProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
        }
    }
}

#[derive(Debug, Serialize)]
struct OpenAiRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    max_tokens: u32,
}

#[derive(Debug, Serialize)]
struct OpenAiMessage {
    role: String,
    content: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiResponseMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponseMessage {
    content: Option<String>,
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai"
    }

    #[instrument(skip(self, messages))]
    async fn complete(&self, messages: &[LlmMessage]) -> Result<String, LlmError> {
        let openai_messages: Vec<OpenAiMessage> = messages
            .iter()
            .map(|m| OpenAiMessage {
                role: match m.role {
                    LlmRole::System => "system".to_string(),
                    LlmRole::User => "user".to_string(),
                    LlmRole::Assistant => "assistant".to_string(),
                },
                content: serde_json::Value::String(m.content.clone()),
            })
            .collect();

        let request = OpenAiRequest {
            model: self.model.clone(),
            messages: openai_messages,
            max_tokens: 4096,
        };

        let response = self
            .client
            .post(OPENAI_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!(
                "OpenAI API error {status}: {error_text}"
            )));
        }

        let openai_response: OpenAiResponse = response.json().await?;

        openai_response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .ok_or_else(|| LlmError::InvalidResponse("No content in response".into()))
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
                "type": "text",
                "text": prompt
            },
            {
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{};base64,{}", mime_type, base64_image)
                }
            }
        ]);

        let request = OpenAiRequest {
            model: self.model.clone(),
            messages: vec![OpenAiMessage {
                role: "user".to_string(),
                content,
            }],
            max_tokens: 4096,
        };

        let response = self
            .client
            .post(OPENAI_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!(
                "OpenAI API error {status}: {error_text}"
            )));
        }

        let openai_response: OpenAiResponse = response.json().await?;

        openai_response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .ok_or_else(|| LlmError::InvalidResponse("No content in response".into()))
    }
}
