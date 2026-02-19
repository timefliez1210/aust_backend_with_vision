use aust_core::models::VisionAnalysisResult;

pub struct VolumeCalculator;

impl VolumeCalculator {
    pub fn new() -> Self {
        Self
    }

    pub fn combine_estimates(&self, vision_results: &[VisionAnalysisResult]) -> f64 {
        if vision_results.is_empty() {
            return 0.0;
        }

        let total: f64 = vision_results
            .iter()
            .map(|r| r.total_volume_m3 * r.confidence_score)
            .sum();

        let total_confidence: f64 = vision_results.iter().map(|r| r.confidence_score).sum();

        if total_confidence > 0.0 {
            total / total_confidence * vision_results.len() as f64
        } else {
            total
        }
    }

    pub fn apply_packing_factor(&self, volume: f64) -> f64 {
        volume * 1.2
    }
}

impl Default for VolumeCalculator {
    fn default() -> Self {
        Self::new()
    }
}
