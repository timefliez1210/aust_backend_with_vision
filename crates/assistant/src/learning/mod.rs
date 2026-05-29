//! Offline learning pipeline skeleton.
//!
//! Phase 5 will implement training and inference using `linfa` + `linfa-trees`.
//! For now the module exposes:
//! - [`observations`] — recording offer adjustment observations to the DB.
//! - [`features`] — feature extraction from inquiry data.
//! - [`predict`] — predictor trait + stub impls (NullPredictor, LinfaPredictor).

pub mod features;
pub mod observations;
pub mod predict;

pub use features::OfferFeatures;
pub use predict::{Adjustments, NullPredictor, OfferAdjustmentPredictor};
