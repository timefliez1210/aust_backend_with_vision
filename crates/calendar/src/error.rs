use thiserror::Error;

/// All failure modes for the calendar service.
#[derive(Debug, Error)]
pub enum CalendarError {
    /// An underlying SQLx / PostgreSQL error.
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    /// The requested booking date has reached its capacity limit.
    /// The payload string contains a human-readable explanation including
    /// the slot counts, so it can be forwarded to the Telegram approval flow
    /// for Alex to decide whether to force-book.
    #[error("Date is fully booked: {0}")]
    FullyBooked(String),

    /// A booking or capacity override with the given ID does not exist.
    #[error("Booking not found: {0}")]
    NotFound(String),

    /// The caller supplied an invalid value (e.g., negative capacity).
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
