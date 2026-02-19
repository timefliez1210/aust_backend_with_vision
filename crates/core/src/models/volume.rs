use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EstimationMethod {
    Vision,
    Inventory,
    DepthSensor,
    Manual,
}

impl EstimationMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Vision => "vision",
            Self::Inventory => "inventory",
            Self::DepthSensor => "depth_sensor",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeEstimation {
    pub id: Uuid,
    pub quote_id: Uuid,
    pub method: EstimationMethod,
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
