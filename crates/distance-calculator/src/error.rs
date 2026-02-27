use thiserror::Error;

/// All failure modes for the distance calculator.
#[derive(Debug, Error)]
pub enum DistanceError {
    /// The address could not be resolved to coordinates by OpenRouteService.
    /// Usually means the address string is too ambiguous or not in DE/AT.
    #[error("Geocoding error: {0}")]
    Geocoding(String),

    /// The routing API returned no valid driving route between two points.
    /// Can happen when fewer than 2 addresses are supplied, or when the
    /// points cannot be reached by road (e.g., different continents).
    #[error("Routing error: {0}")]
    Routing(String),

    /// OpenRouteService returned a non-2xx HTTP status or an unparseable body.
    #[error("API error: {0}")]
    Api(String),

    /// A low-level network or TLS failure when connecting to ORS.
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
}
