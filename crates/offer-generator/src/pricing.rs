use aust_core::models::{PricingBreakdown, PricingInput, PricingResult};
use chrono::Datelike;

pub struct PricingEngine {
    rate_per_person_hour: i64,
    rate_per_km: i64,
    volume_per_person_hour: f64,
}

impl PricingEngine {
    pub fn new() -> Self {
        Self {
            rate_per_person_hour: 3000, // €30
            rate_per_km: 150,
            volume_per_person_hour: 2.0,
        }
    }

    pub fn with_rates(rate_per_person_hour: i64, rate_per_km: i64, volume_per_person_hour: f64) -> Self {
        Self {
            rate_per_person_hour,
            rate_per_km,
            volume_per_person_hour,
        }
    }

    pub fn calculate(&self, input: &PricingInput) -> PricingResult {
        let floor_extra_origin = self.floor_extra_persons(input.floor_origin, input.has_elevator_origin);
        let floor_extra_dest = self.floor_extra_persons(input.floor_destination, input.has_elevator_destination);

        let persons = 2u32.max(2 + floor_extra_origin + floor_extra_dest);

        let throughput = persons as f64 * self.volume_per_person_hour;
        let hours = (input.volume_m3 / throughput).ceil().max(1.0);

        let base_labor_cents = persons as i64 * hours as i64 * self.rate_per_person_hour;

        let distance_cents = (input.distance_km * self.rate_per_km as f64) as i64;

        let date_adjustment_cents = self.calculate_date_adjustment(input);

        let total_price_cents = base_labor_cents + distance_cents + date_adjustment_cents;

        PricingResult {
            total_price_cents,
            breakdown: PricingBreakdown {
                base_labor_cents,
                distance_cents,
                floor_surcharge_cents: 0, // floors add persons, not a separate surcharge
                date_adjustment_cents,
            },
            estimated_helpers: persons,
            estimated_hours: hours,
        }
    }

    fn floor_extra_persons(&self, floor: Option<u32>, has_elevator: Option<bool>) -> u32 {
        match floor {
            Some(f) if f > 0 && !has_elevator.unwrap_or(false) => f,
            _ => 0,
        }
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

/// Parse a German floor string to a numeric floor number.
///
/// "Erdgeschoss" → 0, "Hochparterre" → 0, "1. Stock" → 1, …, "Höher als 6. Stock" → 7
pub fn parse_floor(floor_str: &str) -> u32 {
    let s = floor_str.trim();
    match s {
        "Erdgeschoss" => 0,
        "Hochparterre" => 0,
        "Höher als 6. Stock" => 7,
        _ => {
            // Try "N. Stock" pattern
            if let Some(num_str) = s.strip_suffix(". Stock") {
                num_str.trim().parse::<u32>().unwrap_or(0)
            } else {
                0
            }
        }
    }
}

impl Default for PricingEngine {
    fn default() -> Self {
        Self::new()
    }
}
