use thiserror::Error;

#[derive(Debug, Error)]
pub enum CalendarError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Date is fully booked: {0}")]
    FullyBooked(String),

    #[error("Booking not found: {0}")]
    NotFound(String),

    #[error("Invalid input: {0}")]
    Validation(String),
}

impl From<CalendarError> for aust_core::Error {
    fn from(err: CalendarError) -> Self {
        match err {
            CalendarError::Database(e) => aust_core::Error::Database(e.to_string()),
            CalendarError::FullyBooked(msg) => aust_core::Error::Validation(msg),
            CalendarError::NotFound(msg) => aust_core::Error::NotFound(msg),
            CalendarError::Validation(msg) => aust_core::Error::Validation(msg),
        }
    }
}
