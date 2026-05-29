//! Offer adjustment predictor.
//!
//! `OfferAdjustmentPredictor` is the polymorphism point between the null baseline
//! (always zero adjustments) and the trained `LinfaPredictor` (Phase 5).
//!
//! The wiring is real: at startup the application selects the predictor based on
//! whether a trained model file exists on disk. If no model file is found,
//! `NullPredictor` is used and a warning is logged.

use crate::learning::features::OfferFeatures;

/// Predicted price/time adjustments from the learning model.
#[derive(Debug, Clone, Default)]
pub struct Adjustments {
    /// Suggested absolute price adjustment in cents (positive = increase).
    pub price_delta_cents: i64,
    /// Confidence in this adjustment (0.0 = no confidence, 1.0 = certain).
    pub confidence: f64,
    /// Human-readable rationale (German), used in the offer drafting prompt.
    pub rationale: Option<String>,
}

/// Predicts offer adjustments from extracted features.
pub trait OfferAdjustmentPredictor: Send + Sync {
    /// Predict adjustments for the given feature vector.
    fn predict(&self, features: &OfferFeatures) -> Adjustments;
}

/// Null predictor — always returns zero adjustments.
///
/// Used until Phase 5 trains a model with sufficient data (target: ≥ 50 observations).
pub struct NullPredictor;

impl OfferAdjustmentPredictor for NullPredictor {
    fn predict(&self, _features: &OfferFeatures) -> Adjustments {
        Adjustments::default()
    }
}

/// Linfa-backed gradient-boosted decision tree predictor.
///
/// The `train` and `predict` methods are stubs — actual implementation is Phase 5.
///
/// At startup, call `LinfaPredictor::load(path)`. If the file does not exist,
/// fall back to `NullPredictor`. After enough observations accumulate in
/// `offer_observations`, call `LinfaPredictor::train(observations)` and save.
pub struct LinfaPredictor {
    /// Path to the serialised model file (bincode format, Phase 5).
    pub model_path: std::path::PathBuf,
    // Phase 5: add linfa model field here.
    // model: linfa_trees::GradientBoostedDecisionTree<f64, i64>,
}

impl LinfaPredictor {
    /// Attempt to load a trained model from disk.
    ///
    /// Returns `None` if the file does not exist or cannot be deserialized.
    pub fn load(path: impl Into<std::path::PathBuf>) -> Option<Self> {
        let path = path.into();
        if path.exists() {
            tracing::info!("LinfaPredictor: loading model from {}", path.display());
            // Phase 5: deserialize model here.
            Some(Self { model_path: path })
        } else {
            tracing::warn!(
                "LinfaPredictor: model file '{}' not found — using NullPredictor",
                path.display()
            );
            None
        }
    }

    /// Train a new model from observations and save it to disk.
    ///
    /// # Phase 5
    /// This will:
    /// 1. Load observation rows from `offer_observations`.
    /// 2. Build an ndarray dataset from `OfferFeatures::to_vec()`.
    /// 3. Train a gradient-boosted decision tree.
    /// 4. Serialise the model to `self.model_path`.
    #[allow(unused_variables)]
    pub fn train(&self, _observations: &[crate::learning::observations::ObservationRow]) {
        unimplemented!(
            "Phase 5: train a linfa gradient-boosted decision tree from observations"
        )
    }
}

impl OfferAdjustmentPredictor for LinfaPredictor {
    fn predict(&self, _features: &OfferFeatures) -> Adjustments {
        unimplemented!(
            "Phase 5: run inference on the trained linfa model"
        )
    }
}

/// Select the best available predictor at startup.
///
/// Returns a `Box<dyn OfferAdjustmentPredictor>` — either the trained model or
/// the null baseline. This is the only constructor callers should use.
pub fn select_predictor(model_path: &std::path::Path) -> Box<dyn OfferAdjustmentPredictor> {
    match LinfaPredictor::load(model_path) {
        Some(p) => Box::new(p),
        None => Box::new(NullPredictor),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learning::features::{extract, FeatureInput};

    fn test_features() -> OfferFeatures {
        extract(&FeatureInput {
            volume_m3: Some(25.0),
            distance_km: Some(10.0),
            origin_floor: Some(1),
            destination_floor: Some(2),
            origin_has_elevator: Some(true),
            destination_has_elevator: Some(false),
            has_packing: false,
            has_assembly: false,
            scheduled_weekday: Some(4),
            days_until_move: Some(30),
            customer_repeat_count: 0,
            proposed_price_cents: 120000,
        })
    }

    #[test]
    fn null_predictor_returns_zero_adjustments() {
        let predictor = NullPredictor;
        let adj = predictor.predict(&test_features());
        assert_eq!(adj.price_delta_cents, 0);
        assert_eq!(adj.confidence, 0.0);
        assert!(adj.rationale.is_none());
    }

    #[test]
    fn linfa_predictor_load_returns_none_for_nonexistent_file() {
        let result = LinfaPredictor::load("/tmp/nonexistent_model_12345.bin");
        assert!(result.is_none());
    }

    #[test]
    fn select_predictor_returns_null_when_no_model() {
        let predictor = select_predictor(std::path::Path::new("/tmp/no_model_here.bin"));
        // NullPredictor: zero adjustments.
        let adj = predictor.predict(&test_features());
        assert_eq!(adj.price_delta_cents, 0);
    }
}
