use crate::error::LlmError;
use crate::traits::{LlmMessage, LlmProvider};
use async_trait::async_trait;

/// Mock LLM provider for testing. Returns preconfigured responses.
pub struct MockLlmProvider {
    pub response: String,
}

impl MockLlmProvider {
    pub fn new(response: impl Into<String>) -> Self {
        Self {
            response: response.into(),
        }
    }
}

#[async_trait]
impl LlmProvider for MockLlmProvider {
    async fn complete(&self, _messages: &[LlmMessage]) -> Result<String, LlmError> {
        Ok(self.response.clone())
    }

    async fn analyze_image(
        &self,
        _image_data: &[u8],
        _mime_type: &str,
        _prompt: &str,
    ) -> Result<String, LlmError> {
        Ok(self.response.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_provider_returns_configured_response() {
        let provider = MockLlmProvider::new("test response");
        let result = provider.complete(&[]).await.unwrap();
        assert_eq!(result, "test response");
    }

    #[tokio::test]
    async fn mock_provider_analyze_image_returns_configured_response() {
        let provider = MockLlmProvider::new("{\"items\": []}");
        let result = provider
            .analyze_image(b"fake_image", "image/jpeg", "describe")
            .await
            .unwrap();
        assert_eq!(result, "{\"items\": []}");
    }
}
