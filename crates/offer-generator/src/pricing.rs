use aust_core::models::{PricingBreakdown, PricingInput, PricingResult};
use chrono::Datelike;

/// Stateless engine that derives labor cost, worker count, and estimated hours
/// from volume, floor, elevator, and date inputs.
///
/// **Caller**: `crates/api/src/routes/offers.rs` (via `build_line_items`),
///             `crates/email-agent` (via the orchestrator)
/// **Why**: Centralises all pricing logic in one place so changes to rates or
/// formulas only need to happen here, and are testable in isolation.
///
/// The engine does NOT handle distance-based costs; those are expressed as
/// a flat `Fahrkostenpauschale` line item built separately in the route handler.
pub struct PricingEngine {
    /// Labor rate stored as integer cents to avoid floating-point drift.
    /// Loaded from `CompanyConfig::rate_per_person_hour_cents`.
    /// Default: 3000 cents = €30.00 per person-hour.
    rate_per_person_hour: i64,
    /// Saturday surcharge in cents. Default: 5000 = €50.00.
    saturday_surcharge_cents: i64,
}

impl PricingEngine {
    /// Create a `PricingEngine` with a specific per-person-hour rate and Saturday surcharge.
    ///
    /// **Caller**: `crates/api/src/routes/offers.rs`, passing config values.
    pub fn with_rate(rate_cents: i64, saturday_surcharge_cents: i64) -> Self {
        Self {
            rate_per_person_hour: rate_cents,
            saturday_surcharge_cents,
        }
    }

    /// Create a `PricingEngine` with the default rate of €30.00 per person-hour.
    ///
    /// **Caller**: Tests and legacy call sites that don't have config available.
    pub fn new() -> Self {
        Self {
            rate_per_person_hour: 3000,
            saturday_surcharge_cents: 5000,
        }
    }

    /// Calculate the full pricing result from a set of moving-job inputs.
    ///
    /// **Caller**: `crates/api/src/routes/offers.rs` (directly) and
    ///             the Telegram edit flow (to recompute after overrides)
    /// **Why**: Encapsulates the complete pricing formula so callers receive
    /// ready-to-use worker counts, hours, and cost breakdowns without
    /// reimplementing the arithmetic.
    ///
    /// # Parameters
    /// - `input` — volume, optional floor numbers, elevator availability at each
    ///   stop, and optional preferred moving date
    ///
    /// # Returns
    /// A `PricingResult` containing:
    /// - `estimated_helpers` — number of workers to send
    /// - `estimated_hours` — expected job duration in hours
    /// - `total_price_cents` — total labor cost including any date surcharge
    /// - `breakdown` — itemised cost breakdown for transparency
    ///
    /// # Math
    /// ```text
    /// persons_base    = max(2, ceil(volume_m3 / 5.0))
    /// highest_floor   = max floor without elevator across origin, dest, stop
    /// extra_workers   = max(0, highest_floor - 1)   // floor 1 = no extra
    /// total_persons   = persons_base + extra_workers
    /// hours           = max(1.0, volume_m3 / (total_persons × 0.625))
    /// base_labor      = total_persons × hours × rate_per_person_hour (cents)
    /// total_price     = base_labor + date_adjustment
    /// ```
    pub fn calculate(&self, input: &PricingInput) -> PricingResult {
        // Base workers: ceil(volume_m3 / 5.0), minimum 2
        let persons_base = ((input.volume_m3 / 5.0).ceil() as u32).max(2);

        // Find the highest floor without elevator (origin, destination, intermediate stop)
        let mut floors_without_elevator: Vec<u32> = Vec::new();
        if !input.has_elevator_origin.unwrap_or(false)
            && let Some(f) = input.floor_origin {
                floors_without_elevator.push(f);
            }
        if !input.has_elevator_destination.unwrap_or(false)
            && let Some(f) = input.floor_destination {
                floors_without_elevator.push(f);
            }
        if !input.has_elevator_stop.unwrap_or(false)
            && let Some(f) = input.floor_stop {
                floors_without_elevator.push(f);
            }

        let highest_floor = floors_without_elevator.into_iter().max().unwrap_or(0);
        // Extra workers for floors above 1st: floor 1 = 0 extra, floor 2 = 1 extra, etc.
        let extra_workers = highest_floor.saturating_sub(1);

        let total_persons = persons_base + extra_workers;

        // Hours = volume / (workers × 0.625 m³/worker/hr)
        // 0.625 m³/worker/hr = 5 m³ per 8h per worker
        let hours = (input.volume_m3 / (total_persons as f64 * 0.625)).max(1.0);

        let base_labor_cents =
            (total_persons as f64 * hours * self.rate_per_person_hour as f64).round() as i64;

        let date_adjustment_cents = self.calculate_date_adjustment(input);

        let total_price_cents = base_labor_cents + date_adjustment_cents;

        PricingResult {
            total_price_cents,
            breakdown: PricingBreakdown {
                base_labor_cents,
                distance_cents: 0, // no longer part of pricing (Fahrkostenpauschale is flat)
                floor_surcharge_cents: 0, // floors add persons, not a separate surcharge
                date_adjustment_cents,
            },
            estimated_helpers: total_persons,
            estimated_hours: hours,
        }
    }

    /// Return any date-based price surcharge in cents.
    ///
    /// **Why**: Saturday moves require extra crew coordination and are priced at
    /// a premium. No surcharge applies on any other day, including Sunday (moves
    /// on Sundays are not typically offered). The surcharge amount is configurable
    /// via `CompanyConfig::saturday_surcharge_cents`.
    ///
    /// # Parameters
    /// - `input` — pricing input; only `scheduled_date` is examined here
    ///
    /// # Returns
    /// `saturday_surcharge_cents` if the scheduled date falls on a Saturday, `0` otherwise.
    fn calculate_date_adjustment(&self, input: &PricingInput) -> i64 {
        if let Some(date) = input.scheduled_date
            && date.weekday() == chrono::Weekday::Sat {
                return self.saturday_surcharge_cents;
            }
        0
    }
}

/// Parse a stored floor string to a numeric floor number.
///
/// **Caller**: `offer_builder::run_offer_computation`, on `addresses.floor`.
/// **Why**: Every extra floor without a lift adds a helper to the crew, so a floor
/// this function fails to read is a KVA priced as if the flat were on the ground
/// floor. Three UIs write this column in three different shapes and all of them
/// have to be understood here:
///
/// | Written by | Looks like |
/// |---|---|
/// | public quote form (`kostenloses-angebot`) | `"Erdgeschoss"`, `"3. Stock"`, `"Höher"` |
/// | admin address editor | `"-1"`, `"0"`, `"1"` … `"5"` |
/// | admin "Anfrage anlegen" modal | `"EG"`, `"1. OG"` … `"5. OG"`, `"DG"`, `"UG"` |
///
/// Only the first shape used to parse; the other two silently read as 0 and
/// undercharged every lift-less upper-floor job booked through the dashboard.
///
/// # Returns
/// A `u32` floor index where 0 = ground floor (and anything below it, since a
/// cellar is one flight either way), 1 = first floor above ground. Unknown strings
/// return `0` — never panics.
///
/// # Examples
/// ```
/// # use aust_offer_generator::parse_floor;
/// assert_eq!(parse_floor("Erdgeschoss"), 0);
/// assert_eq!(parse_floor("Hochparterre"), 0);
/// assert_eq!(parse_floor("3. Stock"), 3);
/// assert_eq!(parse_floor("3"), 3);
/// assert_eq!(parse_floor("2. OG"), 2);
/// assert_eq!(parse_floor("EG"), 0);
/// assert_eq!(parse_floor("Höher"), 7);
/// assert_eq!(parse_floor("Höher als 6. Stock"), 7);
/// ```
pub fn parse_floor(floor_str: &str) -> u32 {
    let s = floor_str.trim();
    match s {
        "Erdgeschoss" | "EG" | "Hochparterre" | "Parterre" => return 0,
        // A cellar is a flight of stairs like any other, but the crew formula only
        // counts floors *above* the first, so it stays at 0.
        "Keller" | "UG" | "Untergeschoss" => return 0,
        // Dachgeschoss has no number attached to it; treat it as the worst case the
        // dropdown can express, one above its highest numbered floor.
        "DG" | "Dachgeschoss" => return 6,
        // The public form's select sends the *value* "Höher", not the label.
        "Höher" | "Höher als 6. Stock" => return 7,
        _ => {}
    }

    // "3", "-1" — the admin address editor stores the bare number.
    if let Ok(n) = s.parse::<i64>() {
        return n.max(0) as u32;
    }

    // "3. Stock", "3. OG", "3.OG", "3 OG" — anything that starts with the number.
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        return digits.parse::<u32>().unwrap_or(0);
    }

    0
}

/// Delegates to [`PricingEngine::new`] so `PricingEngine` can be used with
/// `Default`-based construction patterns (e.g. `PricingEngine::default()`).
impl Default for PricingEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aust_core::models::PricingInput;
    

    fn base_input() -> PricingInput {
        PricingInput {
            volume_m3: 10.0,
            distance_km: 0.0,
            scheduled_date: None,
            floor_origin: None,
            floor_destination: None,
            has_elevator_origin: None,
            has_elevator_destination: None,
            floor_stop: None,
            has_elevator_stop: None,
        }
    }

    // New formula: 10 m³ → persons_base = ceil(10/5) = 2, extra = 0, total = 2
    // hours = 10/(2*0.625) = 8.0h
    #[test]
    fn new_formula_base_volume_10() {
        let engine = PricingEngine::new();
        let result = engine.calculate(&base_input());
        assert_eq!(result.estimated_helpers, 2);
        assert!((result.estimated_hours - 8.0).abs() < 0.01);
    }

    // 11 m³ → persons_base = ceil(11/5) = 3, extra = 0, total = 3
    // hours = 11/(3*0.625) = 5.8667h
    #[test]
    fn new_formula_base_volume_11() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.volume_m3 = 11.0;
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 3);
        assert!((result.estimated_hours - 5.867).abs() < 0.01);
    }

    // 15 m³ → persons_base = ceil(15/5) = 3, extra = 0, total = 3
    // hours = 15/(3*0.625) = 8.0h
    #[test]
    fn new_formula_base_volume_15() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.volume_m3 = 15.0;
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 3);
        assert!((result.estimated_hours - 8.0).abs() < 0.01);
    }

    // 16 m³ → persons_base = ceil(16/5) = 4, extra = 0, total = 4
    // hours = 16/(4*0.625) = 6.4h
    #[test]
    fn new_formula_base_volume_16() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.volume_m3 = 16.0;
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 4);
        assert!((result.estimated_hours - 6.4).abs() < 0.01);
    }

    // 10 m³, origin 3.OG no elev, dest 4.OG no elev
    // floors_without_elevator = [3, 4], highest = 4, extra = 4-1 = 3
    // total = 2+3 = 5, hours = 10/(5*0.625) = 3.2h
    #[test]
    fn floor_worst_case_no_elevator() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.floor_origin = Some(3);
        input.has_elevator_origin = Some(false);
        input.floor_destination = Some(4);
        input.has_elevator_destination = Some(false);
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 5);
        assert!((result.estimated_hours - 3.2).abs() < 0.01);
    }

    // 10 m³, origin 3.OG no elev, dest 4.OG has elev
    // floors_without_elevator = [3], highest = 3, extra = 3-1 = 2
    // total = 2+2 = 4, hours = 10/(4*0.625) = 4.0h
    #[test]
    fn floor_elevator_excludes_floor() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.floor_origin = Some(3);
        input.has_elevator_origin = Some(false);
        input.floor_destination = Some(4);
        input.has_elevator_destination = Some(true);
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 4);
        assert!((result.estimated_hours - 4.0).abs() < 0.01);
    }

    // 10 m³, both 3.OG + 4.OG have elevator
    // floors_without_elevator = [], extra = 0, total = 2, hours = 8.0h
    #[test]
    fn floor_all_elevators() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.floor_origin = Some(3);
        input.has_elevator_origin = Some(true);
        input.floor_destination = Some(4);
        input.has_elevator_destination = Some(true);
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 2);
        assert!((result.estimated_hours - 8.0).abs() < 0.01);
    }

    // EG (floor 0): highest_floor = 0, extra = max(0, 0-1) = 0
    #[test]
    fn floor_ground_level_no_extra() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.floor_origin = Some(0);
        input.has_elevator_origin = Some(false);
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 2);
    }

    // 1. Stock (floor 1): extra = max(0, 1-1) = 0
    #[test]
    fn floor_first_floor_no_extra() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.floor_origin = Some(1);
        input.has_elevator_origin = Some(false);
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 2);
    }

    // Intermediate stop is highest floor without elevator
    // 10 m³, origin EG, dest 2.OG no elev, stop 5.OG no elev
    // highest = 5, extra = 4, total = 6, hours = 10/(6*0.625) = 2.667h
    #[test]
    fn floor_stop_included() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.floor_destination = Some(2);
        input.has_elevator_destination = Some(false);
        input.floor_stop = Some(5);
        input.has_elevator_stop = Some(false);
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 6);
        assert!((result.estimated_hours - 2.667).abs() < 0.01);
    }

    // Stop has elevator, origin 3.OG no elev is the highest
    #[test]
    fn floor_stop_has_elevator_excluded() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.floor_origin = Some(3);
        input.has_elevator_origin = Some(false);
        input.floor_stop = Some(5);
        input.has_elevator_stop = Some(true); // elevator at stop → excluded
        let result = engine.calculate(&input);
        // highest = 3 (stop excluded), extra = 2, total = 4
        assert_eq!(result.estimated_helpers, 4);
    }

    // Saturday → +5000 cents surcharge
    #[test]
    fn saturday_surcharge_still_works() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        // Feb 28, 2026 is a Saturday
        input.scheduled_date = Some(chrono::NaiveDate::from_ymd_opt(2026, 2, 28).unwrap());
        let result = engine.calculate(&input);
        assert_eq!(result.breakdown.date_adjustment_cents, 5_000);
    }

    // Sunday → no surcharge
    #[test]
    fn sunday_no_surcharge() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.scheduled_date = Some(chrono::NaiveDate::from_ymd_opt(2026, 3, 1).unwrap());
        let result = engine.calculate(&input);
        assert_eq!(result.breakdown.date_adjustment_cents, 0);
    }

    // No date → no surcharge
    #[test]
    fn no_date_no_surcharge() {
        let engine = PricingEngine::new();
        let result = engine.calculate(&base_input());
        assert_eq!(result.breakdown.date_adjustment_cents, 0);
    }

    // Very small volume → minimum 2 workers (ceil(1/5)=1, but min 2)
    #[test]
    fn min_two_workers_for_tiny_volume() {
        let engine = PricingEngine::new();
        let mut input = base_input();
        input.volume_m3 = 1.0;
        let result = engine.calculate(&input);
        assert_eq!(result.estimated_helpers, 2);
        // hours = 1/(2*0.625) = 0.8, min 1.0
        assert!((result.estimated_hours - 1.0).abs() < 0.01);
    }

    // parse_floor tests
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

    /// The admin address editor stores the bare number. Reading these as 0 priced
    /// every lift-less upper-floor job booked through the dashboard as ground floor.
    #[test]
    fn parse_floor_admin_numeric_values() {
        assert_eq!(parse_floor("-1"), 0);
        for n in 0..=5u32 {
            assert_eq!(parse_floor(&n.to_string()), n, "numeric floor {n}");
        }
    }

    /// The "Anfrage anlegen" modal stores OG labels.
    #[test]
    fn parse_floor_og_labels() {
        assert_eq!(parse_floor("EG"), 0);
        assert_eq!(parse_floor("UG"), 0);
        assert_eq!(parse_floor("DG"), 6);
        for n in 1..=5u32 {
            assert_eq!(parse_floor(&format!("{n}. OG")), n, "{n}. OG");
            assert_eq!(parse_floor(&format!("{n}.OG")), n, "{n}.OG");
        }
    }

    /// The public form's select sends the *value* "Höher", never the label.
    #[test]
    fn parse_floor_hoeher_value_not_label() {
        assert_eq!(parse_floor("Höher"), 7);
        assert_eq!(parse_floor("Höher als 6. Stock"), 7);
    }

    /// A 3rd floor without a lift must add helpers regardless of which UI wrote it.
    #[test]
    fn every_stored_floor_shape_prices_the_same() {
        let engine = PricingEngine::new();
        let helpers_for = |floor: &str| {
            let mut input = base_input();
            input.floor_origin = Some(parse_floor(floor));
            input.has_elevator_origin = Some(false);
            engine.calculate(&input).estimated_helpers
        };
        // 10 m³ → base 2, third floor without a lift → +2 → 4.
        for shape in ["3. Stock", "3", "3. OG"] {
            assert_eq!(helpers_for(shape), 4, "floor written as {shape:?}");
        }
    }

    // Proptest: parse_floor and pricing never panic
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn parse_floor_never_panics(s in ".*") {
            let _ = parse_floor(&s);
        }

        #[test]
        fn pricing_never_panics(
            volume in 0.0f64..1000.0f64,
            floor_o in proptest::option::of(0u32..8u32),
            floor_d in proptest::option::of(0u32..8u32),
            floor_s in proptest::option::of(0u32..8u32),
            elev_o in proptest::option::of(proptest::bool::ANY),
            elev_d in proptest::option::of(proptest::bool::ANY),
            elev_s in proptest::option::of(proptest::bool::ANY),
        ) {
            let engine = PricingEngine::new();
            let input = PricingInput {
                volume_m3: volume,
                distance_km: 0.0,
                scheduled_date: None,
                floor_origin: floor_o,
                floor_destination: floor_d,
                floor_stop: floor_s,
                has_elevator_origin: elev_o,
                has_elevator_destination: elev_d,
                has_elevator_stop: elev_s,
            };
            let _ = engine.calculate(&input);
        }
    }
}
