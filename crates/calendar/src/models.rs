use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// A confirmed or tentative moving booking.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Booking {
    pub id: Uuid,
    pub booking_date: NaiveDate,
    pub quote_id: Option<Uuid>,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub departure_address: Option<String>,
    pub arrival_address: Option<String>,
    pub volume_m3: Option<f64>,
    pub distance_km: Option<f64>,
    pub description: Option<String>,
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Data required to create a new booking.
#[derive(Debug, Clone, Deserialize)]
pub struct NewBooking {
    pub booking_date: NaiveDate,
    pub quote_id: Option<Uuid>,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub departure_address: Option<String>,
    pub arrival_address: Option<String>,
    pub volume_m3: Option<f64>,
    pub distance_km: Option<f64>,
    pub description: Option<String>,
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "confirmed".to_string()
}

/// A capacity override for a specific date.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CapacityOverride {
    pub id: Uuid,
    pub override_date: NaiveDate,
    pub capacity: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Availability information for a single date.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DateAvailability {
    pub date: NaiveDate,
    pub available: bool,
    pub capacity: i32,
    pub booked: i32,
    pub remaining: i32,
}

/// Result of checking availability for a requested date.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailabilityResult {
    pub requested_date: NaiveDate,
    pub requested_date_available: bool,
    pub requested_date_info: DateAvailability,
    /// Up to 3 nearest available dates (only populated when requested date is unavailable).
    pub alternatives: Vec<DateAvailability>,
}

/// Schedule entry for a date range view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleEntry {
    pub date: NaiveDate,
    pub availability: DateAvailability,
    pub bookings: Vec<Booking>,
}
