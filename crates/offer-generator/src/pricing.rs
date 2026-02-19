use aust_core::models::{PricingBreakdown, PricingInput, PricingResult};
use chrono::Datelike;

pub struct PricingEngine {
    rate_per_helper_hour: i64,
    rate_per_km: i64,
    volume_per_helper: f64,
    hours_per_move: f64,
}

impl PricingEngine {
    pub fn new() -> Self {
        Self {
            rate_per_helper_hour: 3500,
            rate_per_km: 150,
            volume_per_helper: 10.0,
            hours_per_move: 8.0,
        }
    }

    pub fn with_rates(
        rate_per_helper_hour: i64,
        rate_per_km: i64,
        volume_per_helper: f64,
        hours_per_move: f64,
    ) -> Self {
        Self {
            rate_per_helper_hour,
            rate_per_km,
            volume_per_helper,
            hours_per_move,
        }
    }

    pub fn calculate(&self, input: &PricingInput) -> PricingResult {
        let helpers = (input.volume_m3 / self.volume_per_helper).ceil() as u32;
        let helpers = helpers.max(2);

        let base_labor_cents =
            helpers as i64 * self.rate_per_helper_hour * self.hours_per_move as i64;

        let distance_cents = (input.distance_km * self.rate_per_km as f64) as i64;

        let floor_surcharge_cents = self.calculate_floor_surcharge(input);

        let date_adjustment_cents = self.calculate_date_adjustment(input);

        let total_price_cents =
            base_labor_cents + distance_cents + floor_surcharge_cents + date_adjustment_cents;

        PricingResult {
            total_price_cents,
            breakdown: PricingBreakdown {
                base_labor_cents,
                distance_cents,
                floor_surcharge_cents,
                date_adjustment_cents,
            },
            estimated_helpers: helpers,
            estimated_hours: self.hours_per_move,
        }
    }

    fn calculate_floor_surcharge(&self, input: &PricingInput) -> i64 {
        let mut surcharge = 0i64;

        if let Some(floor) = input.floor_origin {
            if !input.has_elevator_origin.unwrap_or(false) && floor > 0 {
                surcharge += floor as i64 * 1000;
            }
        }

        if let Some(floor) = input.floor_destination {
            if !input.has_elevator_destination.unwrap_or(false) && floor > 0 {
                surcharge += floor as i64 * 1000;
            }
        }

        surcharge
    }

    fn calculate_date_adjustment(&self, input: &PricingInput) -> i64 {
        if let Some(date) = input.preferred_date {
            let weekday = date.weekday();
            if weekday == chrono::Weekday::Sat {
                return 5000;
            }
        }
        0
    }
}

impl Default for PricingEngine {
    fn default() -> Self {
        Self::new()
    }
}
