use thiserror::Error;

#[derive(Debug, Error)]
pub enum DistanceError {
    #[error("Geocoding error: {0}")]
    Geocoding(String),

    #[error("Routing error: {0}")]
    Routing(String),

    #[error("API error: {0}")]
    Api(String),

    #[error("Cache error: {0}")]
    Cache(String),

    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
}
