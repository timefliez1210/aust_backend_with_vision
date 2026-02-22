use crate::VolumeError;
use aust_core::models::VisionAnalysisResult;
use aust_llm_providers::LlmProvider;
use std::sync::Arc;

/// Extract JSON object from LLM response that may contain markdown code blocks or extra text.
fn extract_json(text: &str) -> Option<&str> {
    // Try to find JSON in a code block first
    if let Some(start) = text.find("```json") {
        let json_start = start + 7;
        if let Some(end) = text[json_start..].find("```") {
            return Some(text[json_start..json_start + end].trim());
        }
    }
    if let Some(start) = text.find("```") {
        let json_start = start + 3;
        // Skip optional language tag on same line
        let json_start = text[json_start..]
            .find('\n')
            .map(|n| json_start + n + 1)
            .unwrap_or(json_start);
        if let Some(end) = text[json_start..].find("```") {
            return Some(text[json_start..json_start + end].trim());
        }
    }
    // Try to find a bare JSON object
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end > start {
        Some(&text[start..=end])
    } else {
        None
    }
}

pub struct VisionAnalyzer {
    llm: Arc<dyn LlmProvider>,
}

impl VisionAnalyzer {
    pub fn new(llm: Arc<dyn LlmProvider>) -> Self {
        Self { llm }
    }

    pub async fn analyze_image(
        &self,
        image_data: &[u8],
        mime_type: &str,
    ) -> Result<VisionAnalysisResult, VolumeError> {
        let prompt = r#"
Analysiere dieses Bild eines Zimmers und identifiziere alle Gegenstände, die bei einem Umzug transportiert werden müssten.

Für jeden Gegenstand, schätze das Volumen in Kubikmetern (m³).

Antworte im folgenden JSON-Format:
{
  "detected_items": [
    {"name": "Gegenstandsname", "estimated_volume_m3": 0.5, "confidence": 0.8}
  ],
  "total_volume_m3": 5.0,
  "confidence_score": 0.75,
  "room_type": "Wohnzimmer",
  "analysis_notes": "Zusätzliche Beobachtungen"
}
"#;

        let response = self
            .llm
            .analyze_image(image_data, mime_type, prompt)
            .await
            .map_err(|e| VolumeError::Llm(e.to_string()))?;

        // Extract JSON from the response — LLMs may wrap it in markdown code blocks
        // or include thinking tags
        let json_str = extract_json(&response).unwrap_or(&response);

        let result: VisionAnalysisResult =
            serde_json::from_str(json_str).map_err(|e| {
                tracing::warn!("Failed to parse LLM vision response as JSON: {e}\nResponse: {response}");
                VolumeError::Vision(e.to_string())
            })?;

        Ok(result)
    }
}
