//! Shared waypoint builder for the Umzugsroute (depot → Auszug → [Zwischenstopp] → Einzug → depot).
//!
//! **Why this exists**: the route was built inline in two places that disagreed with each
//! other — `offer_builder::build_fahrt_item` priced the full round trip from the depot,
//! while the admin detail page posted only `[origin, destination]` to
//! `/api/v1/distance/calculate` and drew that on the map. Alex saw a two-stop line on the
//! map and a round-trip figure on the KVA and could not reconcile them (report bce7d392).
//! Both callers now derive their waypoints here, so the map and the price can no longer
//! diverge.

use crate::repositories::AddressRow;
use crate::services::offer_builder::{format_city, format_street};

/// One stop on the route, with the label Alex sees in the UI.
#[derive(Debug, Clone)]
pub(crate) struct Waypoint {
    /// German role label: `"Lager"`, `"Auszug"`, `"Zwischenstopp"`, `"Einzug"`.
    pub(crate) label: String,
    /// Free-text address handed to the ORS geocoder.
    pub(crate) address: String,
}

/// Format an address for geocoding as `"Street Hausnummer, PLZ City"`.
///
/// **Caller**: [`build_waypoints`].
/// **Why**: reuses the KVA's `format_street` so the house number is included. The
/// inline version this replaced passed only `street`, which since the 2026-05
/// house-number split geocoded to the street centroid instead of the building.
///
/// Runs of whitespace are collapsed because legacy rows store the number inside
/// `street` with stray spacing (`"Marie-Wagenknecht-Str.  5"`), and these strings are
/// shown to Alex in the route breakdown, not just sent to the geocoder.
fn format_address(addr: &AddressRow) -> String {
    let joined = format!("{}, {}", format_street(addr), format_city(addr));
    joined.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Build the ordered waypoint list for an inquiry's move.
///
/// **Caller**: `offer_builder::build_fahrt_item` (pricing) and
/// `inquiries::get_inquiry_route` (map + per-leg display).
/// **Why**: the crew leaves from the depot and returns to it, so the driven distance is
/// the full loop, not the one-way customer distance stored on the inquiry.
///
/// # Parameters
/// - `depot` — `config.company.depot_address`, used as both first and last stop
/// - `origin` — Auszug address; `None` yields `None`
/// - `destination` — Einzug address; `None` yields `None`
/// - `stop` — optional Zwischenstopp (storage, Wertstoffhof, ZAH)
///
/// # Returns
/// `Some` with 4 waypoints (or 5 with a Zwischenstopp), or `None` when either end of the
/// move is missing — callers fall back to `distance_km × 2`.
pub(crate) fn build_waypoints(
    depot: &str,
    origin: Option<&AddressRow>,
    destination: Option<&AddressRow>,
    stop: Option<&AddressRow>,
) -> Option<Vec<Waypoint>> {
    let (origin, destination) = (origin?, destination?);

    let mut waypoints = vec![
        Waypoint { label: "Lager".into(), address: depot.to_string() },
        Waypoint { label: "Auszug".into(), address: format_address(origin) },
    ];
    if let Some(stop) = stop {
        waypoints.push(Waypoint { label: "Zwischenstopp".into(), address: format_address(stop) });
    }
    waypoints.push(Waypoint { label: "Einzug".into(), address: format_address(destination) });
    waypoints.push(Waypoint { label: "Lager".into(), address: depot.to_string() });

    Some(waypoints)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(street: &str, hn: Option<&str>, plz: Option<&str>, city: &str) -> AddressRow {
        AddressRow {
            street: street.into(),
            house_number: hn.map(Into::into),
            city: city.into(),
            postal_code: plz.map(Into::into),
            floor: None,
            elevator: None,
        }
    }

    const DEPOT: &str = "Borsigstr 6 31135 Hildesheim";

    #[test]
    fn round_trip_starts_and_ends_at_the_depot() {
        let origin = addr("Steinbergstr.", Some("4"), Some("31139"), "Hildesheim");
        let dest = addr("Kirchweg", Some("12"), Some("31137"), "Hildesheim");

        let wps = build_waypoints(DEPOT, Some(&origin), Some(&dest), None).unwrap();

        let labels: Vec<&str> = wps.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["Lager", "Auszug", "Einzug", "Lager"]);
        assert_eq!(wps[0].address, DEPOT);
        assert_eq!(wps[3].address, DEPOT);
    }

    #[test]
    fn house_number_and_plz_reach_the_geocoder() {
        let origin = addr("Steinbergstr.", Some("4"), Some("31139"), "Hildesheim");
        let dest = addr("Kirchweg", None, None, "Hildesheim");

        let wps = build_waypoints(DEPOT, Some(&origin), Some(&dest), None).unwrap();

        assert_eq!(wps[1].address, "Steinbergstr. 4, 31139 Hildesheim");
        // No house number and no PLZ — degrades to street + city, never to "…, ".
        assert_eq!(wps[2].address, "Kirchweg, Hildesheim");
    }

    #[test]
    fn legacy_double_spacing_is_collapsed() {
        // Real prod row: the house number lives in `street` with a stray double space.
        let origin = addr("Marie-Wagenknecht-Str.  5", None, Some("31134"), "Hildesheim");
        let dest = addr("Kirchweg", Some("12"), Some("31137"), "Hildesheim");

        let wps = build_waypoints(DEPOT, Some(&origin), Some(&dest), None).unwrap();

        assert_eq!(wps[1].address, "Marie-Wagenknecht-Str. 5, 31134 Hildesheim");
    }

    #[test]
    fn zwischenstopp_is_inserted_between_the_two_ends() {
        let origin = addr("Steinbergstr.", Some("4"), Some("31139"), "Hildesheim");
        let stop = addr("Am Wertstoffhof", Some("1"), Some("31135"), "Hildesheim");
        let dest = addr("Kirchweg", Some("12"), Some("31137"), "Hildesheim");

        let wps = build_waypoints(DEPOT, Some(&origin), Some(&dest), Some(&stop)).unwrap();

        let labels: Vec<&str> = wps.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["Lager", "Auszug", "Zwischenstopp", "Einzug", "Lager"]);
        assert_eq!(wps[2].address, "Am Wertstoffhof 1, 31135 Hildesheim");
    }

    #[test]
    fn missing_either_end_yields_no_route() {
        let origin = addr("Steinbergstr.", Some("4"), Some("31139"), "Hildesheim");

        assert!(build_waypoints(DEPOT, Some(&origin), None, None).is_none());
        assert!(build_waypoints(DEPOT, None, Some(&origin), None).is_none());
        assert!(build_waypoints(DEPOT, None, None, None).is_none());
    }
}
