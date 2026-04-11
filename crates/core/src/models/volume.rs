use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How the volume of a moving job was estimated.
///
/// Determines which pipeline was used and therefore which fields in
/// `VolumeEstimation::result_data` are populated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EstimationMethod {
    /// LLM vision analysis of customer-supplied photos (Claude / OpenAI).
    Vision,
    /// Manual item list submitted via the "Gegenstände" form field.
    Inventory,
    /// 3D depth-sensor pipeline (Grounding DINO + SAM 2 + Depth Anything V2).
    DepthSensor,
    /// AR phone scan pipeline (per-item structured 3D reconstruction).
    Ar,
    /// 3D video reconstruction pipeline (MASt3R + SAM 2 temporal tracking).
    Video,
    /// Volume entered manually by an admin without any automated pipeline.
    Manual,
}

impl EstimationMethod {
    /// Returns the lowercase snake_case string stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Vision => "vision",
            Self::Inventory => "inventory",
            Self::DepthSensor => "depth_sensor",
            Self::Ar => "ar",
            Self::Video => "video",
            Self::Manual => "manual",
        }
    }
}

impl std::fmt::Display for EstimationMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for EstimationMethod {
    type Err = String;

    /// Parse the lowercase snake_case database string back into an enum variant.
    ///
    /// **Caller**: DB row-to-model converters (`From<VolumeEstimationRow>`).
    /// **Why**: Replaces duplicated manual `match row.method.as_str()` blocks.
    ///
    /// # Errors
    /// Returns `Err(String)` for any unrecognised method string.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "vision" => Ok(Self::Vision),
            "inventory" => Ok(Self::Inventory),
            "depth_sensor" => Ok(Self::DepthSensor),
            "ar" => Ok(Self::Ar),
            "video" => Ok(Self::Video),
            "manual" => Ok(Self::Manual),
            other => Err(format!("unknown EstimationMethod: {other}")),
        }
    }
}

/// Stored volume estimation record linked to a quote.
///
/// The `source_data` JSONB column captures what was sent to the pipeline
/// (e.g., image descriptions or item lists). `result_data` stores the raw
/// pipeline response, and `total_volume_m3` is the final agreed-upon value
/// used in offer pricing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeEstimation {
    /// UUID v7 primary key.
    pub id: Uuid,
    /// The quote this estimation belongs to.
    pub inquiry_id: Uuid,
    /// Pipeline used to produce this estimation.
    pub method: EstimationMethod,
    /// Processing status: `"pending"`, `"completed"`, or `"failed"`.
    pub status: String,
    /// Raw input passed to the pipeline (e.g., base64 images or item strings).
    pub source_data: serde_json::Value,
    /// Raw output from the pipeline (provider-specific shape).
    pub result_data: Option<serde_json::Value>,
    /// Final aggregated volume in cubic metres; `None` while `status = "pending"`.
    pub total_volume_m3: Option<f64>,
    /// Pipeline confidence score in the range [0, 1]; `None` for manual entries.
    pub confidence_score: Option<f64>,
    pub created_at: DateTime<Utc>,
}

/// Input for creating a new volume estimation record.
///
/// **Caller**: API route handlers for `/estimates/vision`, `/estimates/depth-sensor`,
/// `/estimates/video`, and `/estimates/inventory` create one of these before
/// dispatching to the relevant pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateVolumeEstimation {
    pub inquiry_id: Uuid,
    pub method: EstimationMethod,
    /// Serialised input data to be stored for auditing (e.g., image metadata or
    /// the raw items list string).
    pub source_data: serde_json::Value,
}

/// A single item in a manually entered inventory list.
///
/// **Caller**: `InventoryForm` submissions from the API or from parsed email
/// Gegenstände fields. Items are summed to produce `total_volume_m3`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryItem {
    /// Human-readable item name in German (e.g., `"Sofa, Couch"`).
    pub name: String,
    /// Number of identical items.
    pub quantity: u32,
    /// Volume per single item in cubic metres.
    pub volume_m3: f64,
    /// Optional category for grouping (e.g., `"Wohnzimmer"`, `"Schlafzimmer"`).
    pub category: Option<String>,
}

/// A complete manual inventory form submission.
///
/// **Caller**: `POST /api/v1/estimates/inventory` deserialises the request body
/// into this struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryForm {
    pub items: Vec<InventoryItem>,
    /// Free-text notes from the customer (e.g., fragile items, disassembly needed).
    pub additional_notes: Option<String>,
}

/// LLM vision analysis output for a set of room photos.
///
/// **Caller**: The `estimates/vision` route handler returns this in the API
/// response after the LLM has analysed the uploaded images.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionAnalysisResult {
    pub detected_items: Vec<DetectedItem>,
    /// Sum of all detected item volumes in cubic metres.
    pub total_volume_m3: f64,
    /// Overall confidence score in the range [0, 1].
    pub confidence_score: f64,
    /// Inferred room type (e.g., `"Wohnzimmer"`, `"Schlafzimmer"`); `None` when ambiguous.
    pub room_type: Option<String>,
    /// Qualitative notes from the LLM about the analysis (e.g., unusual items).
    pub analysis_notes: Option<String>,
}

/// Unified detected item type for all estimation methods.
///
/// Previously split between `DetectedItem` (LLM vision) and `DepthSensorItem` (3D pipeline).
/// Accepts both `volume_m3` and `estimated_volume_m3` via serde alias for backward compatibility
/// with existing JSON in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedItem {
    /// German item name (e.g., `"Sofa, Couch"`).
    pub name: String,
    /// Estimated volume of this item in cubic metres.
    /// Also accepted as `estimated_volume_m3` for backward compatibility.
    #[serde(alias = "estimated_volume_m3")]
    pub volume_m3: f64,
    /// Confidence score for this individual item in the range [0, 1].
    pub confidence: f64,
    /// Physical dimensions measured or estimated by the pipeline.
    #[serde(default)]
    pub dimensions: Option<ItemDimensions>,
    /// Item category for grouping in the output report.
    #[serde(default)]
    pub category: Option<String>,
    /// Localised German display name (from the RE catalogue lookup).
    #[serde(default)]
    pub german_name: Option<String>,
    /// RE (Raumeinheit) value from the Alltransport 24 catalogue (1 RE = 0.1 m³).
    /// `None` when the item was not found in the catalogue and was measured geometrically.
    #[serde(default)]
    pub re_value: Option<f64>,
    /// How the volume was determined: `"re_catalog"` or `"geometric_obb"`.
    #[serde(default)]
    pub volume_source: Option<String>,
    /// Bounding box `[x1, y1, x2, y2]` in pixels within the source image.
    #[serde(default)]
    pub bbox: Option<Vec<f64>>,
    /// Index into the images array identifying which image this bbox belongs to.
    #[serde(default)]
    pub bbox_image_index: Option<usize>,
    /// S3 key for the cropped item image stored during vision processing.
    #[serde(default)]
    pub crop_s3_key: Option<String>,
    /// Indices of images in which this item was detected (for deduplication).
    #[serde(default)]
    pub seen_in_images: Option<Vec<usize>>,
}

/// Type alias for backward compatibility — `DepthSensorItem` was merged into `DetectedItem`.
pub type DepthSensorItem = DetectedItem;

/// 3D depth-sensor pipeline output for a set of images.
///
/// **Caller**: The vision sidecar service returns this JSON shape; the
/// `estimates/depth-sensor` route handler deserialises it and persists it as
/// `result_data` on the `VolumeEstimation` record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepthSensorResult {
    pub detected_items: Vec<DetectedItem>,
    /// Sum of all detected item volumes in cubic metres.
    pub total_volume_m3: f64,
    /// Overall pipeline confidence score in the range [0, 1].
    pub confidence_score: f64,
    /// Wall-clock processing time in milliseconds.
    pub processing_time_ms: u64,
}

/// Physical dimensions of a detected item, measured or estimated in metres.
///
/// **Why**: Stored alongside each `DetectedItem` so that RE-catalogue lookups
/// can be cross-validated against geometric measurements, and for display in
/// the "Erfasste Gegenstände" items sheet of the offer XLSX.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemDimensions {
    pub length_m: f64,
    pub width_m: f64,
    pub height_m: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimation_method_as_str_values() {
        assert_eq!(EstimationMethod::Vision.as_str(), "vision");
        assert_eq!(EstimationMethod::Inventory.as_str(), "inventory");
        assert_eq!(EstimationMethod::DepthSensor.as_str(), "depth_sensor");
        assert_eq!(EstimationMethod::Ar.as_str(), "ar");
        assert_eq!(EstimationMethod::Video.as_str(), "video");
        assert_eq!(EstimationMethod::Manual.as_str(), "manual");
    }

    #[test]
    fn estimation_method_from_str_roundtrip() {
        for (s, expected) in [
            ("vision", EstimationMethod::Vision),
            ("inventory", EstimationMethod::Inventory),
            ("depth_sensor", EstimationMethod::DepthSensor),
            ("ar", EstimationMethod::Ar),
            ("video", EstimationMethod::Video),
            ("manual", EstimationMethod::Manual),
        ] {
            let parsed: EstimationMethod = s.parse().unwrap();
            assert_eq!(parsed, expected);
            // Round-trip: as_str -> parse -> same enum
            assert_eq!(parsed.as_str(), s);
        }
    }

    #[test]
    fn estimation_method_rejects_unknown() {
        let result = "unknown_method".parse::<EstimationMethod>();
        assert!(result.is_err());
    }

    #[test]
    fn estimation_method_ar_is_distinct_from_depth_sensor() {
        assert_ne!(EstimationMethod::Ar, EstimationMethod::DepthSensor);
        assert_ne!(EstimationMethod::Ar.as_str(), EstimationMethod::DepthSensor.as_str());
    }
}
