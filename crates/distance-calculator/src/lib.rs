/// Distance and route calculation using OpenRouteService.
///
/// Public API:
/// - [`RouteCalculator`] — high-level orchestrator; geocodes addresses then
///   chains them into a multi-stop driving route.
/// - [`RouteRequest`] — input: ordered list of address strings (minimum 2).
/// - [`Geocoder`] — address string → WGS-84 coordinates via ORS Geocode API.
/// - [`DistanceError`] — all failure modes (geocoding, routing, API, network).
///
/// The calculated route is returned as a [`aust_core::models::RouteResult`]
/// with per-leg breakdowns and a total price at €1.00 per km.
pub mod error;

mod geocoder;
mod route;
mod router;

pub use error::DistanceError;
pub use geocoder::Geocoder;
pub use route::{RouteCalculator, RouteRequest};
