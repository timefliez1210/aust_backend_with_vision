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

#[cfg(test)]
mod tests {
    use super::*;
    use aust_core::models::PricingInput;
    use chrono::TimeZone;

    fn base_input() -> PricingInput {
        PricingInput {
            volume_m3: 20.0,
            distance_km: 0.0,
            preferred_date: None,
            floor_origin: None,
            floor_destination: None,
            has_elevator_origin: None,
            has_elevator_destination: None,
        }
    }

    // Test 1: 20m³, 0km, no floors → 2 persons, hours = ceil(20/(2*2.0)) = 5h
    // labor = 2 * 5 * 3000 = 30000, total = 30000
    #[test]
    fn basic_pricing_two_persons() {
        let engine = PricingEngine::new();
        let result = engine.calculate(&base_input());
        assert_eq!(result.estimated_helpers, 2);
        assert_eq!(result.estimated_hours, 5.0);
        assert_eq!(result.breakdown.base_labor_cents, 30_000);
        assert_eq!(result.breakdown.distance_cents, 0);
        assert_eq!(result.breakdown.date_adjustment_cents, 0);
        assert_eq!(result.total_price_cents, 30_000);
    }

    // Test 2: origin floor=3, no elevator → +3 persons = 5 total
    // hours = ceil(20/(5*2.0)) = ceil(2.0) = 2h, labor = 5*2*3000 = 30000
    #[test]
    fn floor_adds_extra_persons_no_elevator() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.floor_origin = Some(3);
        input.has_elevator_origin = Some(false);
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 5);
        assert_eq!(result.estimated_hours, 2.0);
        assert_eq!(result.breakdown.base_labor_cents, 30_000);
        assert_eq!(result.total_price_cents, 30_000);
    }

    // Test 3: origin floor=3, has_elevator=true → +0 persons = 2
    #[test]
    fn elevator_negates_floor_surcharge() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.floor_origin = Some(3);
        input.has_elevator_origin = Some(true);
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 2);
        assert_eq!(result.estimated_hours, 5.0);
        assert_eq!(result.breakdown.base_labor_cents, 30_000);
    }

    // Test 4: floor=0 → +0 extra
    #[test]
    fn ground_floor_no_extra_persons() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.floor_origin = Some(0);
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 2);
    }

    // Test 5: origin=2nd (no elev), dest=3rd (no elev) → +2+3 = 7 persons
    // hours = ceil(20/(7*2.0)) = ceil(1.4286) = 2h
    // labor = 7 * 2 * 3000 = 42000
    #[test]
    fn both_floors_add_persons() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.floor_origin = Some(2);
        input.has_elevator_origin = Some(false);
        input.floor_destination = Some(3);
        input.has_elevator_destination = Some(false);
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 7);
        assert_eq!(result.estimated_hours, 2.0);
        assert_eq!(result.breakdown.base_labor_cents, 42_000);
        assert_eq!(result.total_price_cents, 42_000);
    }

    // Test 6: 100km * 150 = 15000 cents
    #[test]
    fn distance_pricing() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.distance_km = 100.0;
        let result = engine.calculate(&input);
        assert_eq!(result.breakdown.distance_cents, 15_000);
        assert_eq!(result.total_price_cents, 30_000 + 15_000);
    }

    // Test 7: Saturday → +5000 cents
    #[test]
    fn saturday_surcharge() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        // Feb 28, 2026 is a Saturday
        input.preferred_date = Some(chrono::Utc.with_ymd_and_hms(2026, 2, 28, 10, 0, 0).unwrap());
        let result = engine.calculate(&input);
        assert_eq!(result.breakdown.date_adjustment_cents, 5_000);
        assert_eq!(result.total_price_cents, 30_000 + 5_000);
    }

    // Test 8: Sunday → no surcharge (only Saturday is special)
    #[test]
    fn sunday_no_surcharge() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        // Mar 1, 2026 is a Sunday
        input.preferred_date = Some(chrono::Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap());
        let result = engine.calculate(&input);
        assert_eq!(result.breakdown.date_adjustment_cents, 0);
    }

    // Test 9: Weekday → no surcharge
    #[test]
    fn weekday_no_surcharge() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        // Feb 25, 2026 is a Wednesday
        input.preferred_date = Some(chrono::Utc.with_ymd_and_hms(2026, 2, 25, 10, 0, 0).unwrap());
        let result = engine.calculate(&input);
        assert_eq!(result.breakdown.date_adjustment_cents, 0);
    }

    // Test 10: No date → no surcharge
    #[test]
    fn no_date_no_surcharge() {
        let engine = PricingEngine::new();
        let input = base_input();
        let result = engine.calculate(&input);
        assert_eq!(result.breakdown.date_adjustment_cents, 0);
    }

    // Test 11: 0.5m³ → hours = ceil(0.5/(2*2.0)) = ceil(0.125) = 1
    #[test]
    fn small_volume_min_one_hour() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.volume_m3 = 0.5;
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_hours, 1.0);
        assert_eq!(result.breakdown.base_labor_cents, 2 * 1 * 3000);
    }

    // Test 12: 40m³, 2 persons → ceil(40/(2*2.0)) = ceil(10.0) = 10h
    #[test]
    fn large_volume_scales_hours() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.volume_m3 = 40.0;
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 2);
        assert_eq!(result.estimated_hours, 10.0);
        assert_eq!(result.breakdown.base_labor_cents, 2 * 10 * 3000);
    }

    // Test 13: custom rates with_rates(5000, 200, 3.0)
    // 20m³, 50km → 2 persons, hours = ceil(20/(2*3.0)) = ceil(3.33) = 4h
    // labor = 2*4*5000 = 40000, distance = 50*200 = 10000, total = 50000
    #[test]
    fn custom_rates() {
        let engine = PricingEngine::with_rates(5000, 200, 3.0);
        let mut input = base_input();
        input.distance_km = 50.0;
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 2);
        assert_eq!(result.estimated_hours, 4.0);
        assert_eq!(result.breakdown.base_labor_cents, 40_000);
        assert_eq!(result.breakdown.distance_cents, 10_000);
        assert_eq!(result.total_price_cents, 50_000);
    }

    // Tests 14-18: parse_floor tests
    #[test]
    fn parse_floor_erdgeschoss() {
        assert_eq!(parse_floor("Erdgeschoss"), 0);
    }

    #[test]
    fn parse_floor_numbered() {
        for n in 1..=6 {
            assert_eq!(parse_floor(&format!("{n}. Stock")), n);
        }
    }

    #[test]
    fn parse_floor_hochparterre() {
        assert_eq!(parse_floor("Hochparterre"), 0);
    }

    #[test]
    fn parse_floor_higher_than_6() {
        assert_eq!(parse_floor("Höher als 6. Stock"), 7);
    }

    #[test]
    fn parse_floor_unknown_string() {
        assert_eq!(parse_floor("random"), 0);
    }

    // Proptest: parse_floor never panics on arbitrary input
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn parse_floor_never_panics(s in ".*") {
            let _ = parse_floor(&s);
        }
    }
}
