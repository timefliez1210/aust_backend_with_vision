use crate::{LlmError, LlmMessage, LlmProvider, LlmRole};
use async_trait::async_trait;
use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::instrument;

pub struct OllamaProvider {
    client: Client,
    base_url: String,
    model: String,
}

impl OllamaProvider {
    pub fn new(base_url: String, model: String) -> Self {
        Self {
            client: Client::new(),
            base_url,
            model,
        }
    }
}

#[derive(Debug, Serialize)]
struct OllamaRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
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
    fn name(&self) -> &str {
        "ollama"
    }

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
                images: None,
            })
            .collect();

        let request = OllamaRequest {
            model: self.model.clone(),
            messages: ollama_messages,
            stream: false,
        };

        let url = format!("{}/api/chat", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

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
        };

        let url = format!("{}/api/chat", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

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
