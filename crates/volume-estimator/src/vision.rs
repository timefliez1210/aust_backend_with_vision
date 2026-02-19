use crate::VolumeError;
use aust_core::models::VisionAnalysisResult;
use aust_llm_providers::LlmProvider;
use std::sync::Arc;

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

        let result: VisionAnalysisResult =
            serde_json::from_str(&response).map_err(|e| VolumeError::Vision(e.to_string()))?;

        Ok(result)
    }
}
