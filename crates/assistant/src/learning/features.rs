//! Feature extraction for the offer adjustment predictor.
//!
//! `OfferFeatures` is a flat numeric/boolean struct that the `LinfaPredictor`
//! will consume in Phase 5. The extractor reads from a minimal inquiry snapshot
//! (passed as individual fields to avoid a direct dependency on the full
//! inquiry domain model, which lives in `crates/api`).

use serde::{Deserialize, Serialize};

/// Flat feature vector for a moving job, extracted from inquiry + offer data.
///
/// All numeric values are in SI-adjacent units (m³, km) to ease model training.
/// Boolean flags are represented as `f64` (0.0 / 1.0) so the feature vector
/// can be directly flattened into an ndarray for linfa.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferFeatures {
    /// Estimated move volume in cubic metres.
    pub volume_m3: f64,
    /// Calculated driving distance in kilometres.
    pub distance_km: f64,
    /// Origin floor number (0 = ground floor).
    pub origin_floor: i32,
    /// Destination floor number.
    pub destination_floor: i32,
    /// Whether the origin building has an elevator.
    pub origin_has_elevator: f64,
    /// Whether the destination building has an elevator.
    pub destination_has_elevator: f64,
    /// Whether packing service was requested.
    pub has_packing: f64,
    /// Whether assembly / disassembly was requested.
    pub has_assembly: f64,
    /// Whether the move is scheduled on a weekend.
    pub is_weekend: f64,
    /// Days between inquiry creation and scheduled date (urgency proxy).
    pub days_until_move: f64,
    /// How many previous inquiries this customer has had (repeat customer flag).
    pub customer_repeat_count: i32,
    /// Baseline price the pricing engine proposed (in cents).
    pub proposed_price_cents: i64,
}

impl OfferFeatures {
    /// Flatten into a `Vec<f64>` for ndarray consumption in Phase 5.
    pub fn to_vec(&self) -> Vec<f64> {
        vec![
            self.volume_m3,
            self.distance_km,
            self.origin_floor as f64,
            self.destination_floor as f64,
            self.origin_has_elevator,
            self.destination_has_elevator,
            self.has_packing,
            self.has_assembly,
            self.is_weekend,
            self.days_until_move,
            self.customer_repeat_count as f64,
            self.proposed_price_cents as f64,
        ]
    }

    /// Number of features in the vector (used by the linfa dataset builder).
    pub const fn n_features() -> usize {
        12
    }
}

/// Input snapshot for feature extraction.
///
/// Callers populate this from the inquiry + offer rows; this decouples the
/// learning module from the full API domain types.
pub struct FeatureInput {
    pub volume_m3: Option<f64>,
    pub distance_km: Option<f64>,
    pub origin_floor: Option<i32>,
    pub destination_floor: Option<i32>,
    pub origin_has_elevator: Option<bool>,
    pub destination_has_elevator: Option<bool>,
    pub has_packing: bool,
    pub has_assembly: bool,
    pub scheduled_weekday: Option<u32>,  // 1=Mon … 7=Sun
    pub days_until_move: Option<i64>,
    pub customer_repeat_count: i32,
    pub proposed_price_cents: i64,
}

/// Extract features from a `FeatureInput`, applying sensible defaults for missing values.
pub fn extract(input: &FeatureInput) -> OfferFeatures {
    OfferFeatures {
        volume_m3: input.volume_m3.unwrap_or(0.0),
        distance_km: input.distance_km.unwrap_or(0.0),
        origin_floor: input.origin_floor.unwrap_or(0),
        destination_floor: input.destination_floor.unwrap_or(0),
        origin_has_elevator: bool_to_f64(input.origin_has_elevator),
        destination_has_elevator: bool_to_f64(input.destination_has_elevator),
        has_packing: if input.has_packing { 1.0 } else { 0.0 },
        has_assembly: if input.has_assembly { 1.0 } else { 0.0 },
        is_weekend: match input.scheduled_weekday {
            Some(d) if d >= 6 => 1.0,  // Saturday (6) or Sunday (7)
            _ => 0.0,
        },
        days_until_move: input.days_until_move.unwrap_or(0) as f64,
        customer_repeat_count: input.customer_repeat_count,
        proposed_price_cents: input.proposed_price_cents,
    }
}

fn bool_to_f64(v: Option<bool>) -> f64 {
    match v {
        Some(true) => 1.0,
        Some(false) => 0.0,
        None => 0.5,  // Unknown: assume neutral.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_input() -> FeatureInput {
        FeatureInput {
            volume_m3: Some(30.0),
            distance_km: Some(12.5),
            origin_floor: Some(2),
            destination_floor: Some(0),
            origin_has_elevator: Some(false),
            destination_has_elevator: Some(true),
            has_packing: true,
            has_assembly: false,
            scheduled_weekday: Some(6),
            days_until_move: Some(14),
            customer_repeat_count: 1,
            proposed_price_cents: 150000,
        }
    }

    #[test]
    fn extract_produces_correct_weekend_flag() {
        let input = make_input();
        let f = extract(&input);
        assert_eq!(f.is_weekend, 1.0);
    }

    #[test]
    fn extract_weekday_gives_zero_flag() {
        let mut input = make_input();
        input.scheduled_weekday = Some(3);
        let f = extract(&input);
        assert_eq!(f.is_weekend, 0.0);
    }

    #[test]
    fn to_vec_has_correct_length() {
        let f = extract(&make_input());
        assert_eq!(f.to_vec().len(), OfferFeatures::n_features());
    }
}
