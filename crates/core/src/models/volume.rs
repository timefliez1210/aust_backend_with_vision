use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EstimationMethod {
    Vision,
    Inventory,
    DepthSensor,
    Video,
    Manual,
}

impl EstimationMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Vision => "vision",
            Self::Inventory => "inventory",
            Self::DepthSensor => "depth_sensor",
            Self::Video => "video",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeEstimation {
    pub id: Uuid,
    pub quote_id: Uuid,
    pub method: EstimationMethod,
    pub status: String,
    pub source_data: serde_json::Value,
    pub result_data: Option<serde_json::Value>,
    pub total_volume_m3: Option<f64>,
    pub confidence_score: Option<f64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateVolumeEstimation {
    pub quote_id: Uuid,
    pub method: EstimationMethod,
    pub source_data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryItem {
    pub name: String,
    pub quantity: u32,
    pub volume_m3: f64,
    pub category: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryForm {
    pub items: Vec<InventoryItem>,
    pub additional_notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionAnalysisResult {
    pub detected_items: Vec<DetectedItem>,
    pub total_volume_m3: f64,
    pub confidence_score: f64,
    pub room_type: Option<String>,
    pub analysis_notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedItem {
    pub name: String,
    pub estimated_volume_m3: f64,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepthSensorResult {
    pub detected_items: Vec<DepthSensorItem>,
    pub total_volume_m3: f64,
    pub confidence_score: f64,
    pub processing_time_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepthSensorItem {
    pub name: String,
    pub volume_m3: f64,
    pub dimensions: Option<ItemDimensions>,
    pub confidence: f64,
    pub category: Option<String>,
    /// German name from RE catalog (e.g. "Kleiderschrank, 2-türig")
    #[serde(default)]
    pub german_name: Option<String>,
    /// RE value from Alltransport catalog (1 RE = 0.1 m³)
    #[serde(default)]
    pub re_value: Option<f64>,
    /// How volume was determined: "re_lookup" or "geometric"
    #[serde(default)]
    pub volume_source: Option<String>,
    #[serde(default)]
    pub bbox: Option<Vec<f64>>,
    #[serde(default)]
    pub bbox_image_index: Option<usize>,
    #[serde(default)]
    pub crop_s3_key: Option<String>,
    #[serde(default)]
    pub seen_in_images: Option<Vec<usize>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemDimensions {
    pub length_m: f64,
    pub width_m: f64,
    pub height_m: f64,
}
