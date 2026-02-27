use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// A confirmed or tentative moving job booking for a specific calendar date.
///
/// Created by `CalendarService::create_booking` (respects capacity) or
/// `CalendarService::force_create_booking` (admin override). Status values are
/// `"confirmed"`, `"tentative"`, and `"cancelled"`.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Booking {
    /// UUID v7 primary key.
    pub id: Uuid,
    /// The calendar date on which the move is scheduled.
    pub booking_date: NaiveDate,
    /// The quote that this booking is associated with, if any.
    pub quote_id: Option<Uuid>,
    /// Customer's full name (denormalised for quick display in the schedule view).
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub departure_address: Option<String>,
    pub arrival_address: Option<String>,
    /// Estimated moving volume in cubic metres; used for crew planning.
    pub volume_m3: Option<f64>,
    /// Total route distance in kilometres; used for truck logistics.
    pub distance_km: Option<f64>,
    /// Free-text description or special instructions for this booking.
    pub description: Option<String>,
    /// Booking lifecycle status: `"confirmed"`, `"tentative"`, or `"cancelled"`.
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Data required to create a new booking record.
///
/// **Caller**: `POST /api/v1/calendar/bookings` deserialises the request body
/// into this struct, which is then passed to `CalendarService::create_booking`.
#[derive(Debug, Clone, Deserialize)]
pub struct NewBooking {
    /// The date on which the move should take place.
    pub booking_date: NaiveDate,
    pub quote_id: Option<Uuid>,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub departure_address: Option<String>,
    pub arrival_address: Option<String>,
    pub volume_m3: Option<f64>,
    pub distance_km: Option<f64>,
    pub description: Option<String>,
    /// Defaults to `"confirmed"` when omitted from the JSON body.
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "confirmed".to_string()
}

/// A per-date capacity override stored in `calendar_capacity_overrides`.
///
/// When an override exists for a date, `CalendarService` uses its `capacity`
/// value instead of `CalendarConfig::default_capacity`. This lets Alex block
/// dates (capacity 0) or allow extra bookings (capacity > default).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CapacityOverride {
    pub id: Uuid,
    /// The specific calendar date this override applies to.
    pub override_date: NaiveDate,
    /// The effective capacity limit for that date (0 = fully blocked).
    pub capacity: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Availability information for a single calendar date.
///
/// **Caller**: Returned by `CalendarService::check_availability` and embedded
/// in `AvailabilityResult` and `ScheduleEntry`. The API response exposes this
/// to the admin dashboard and customer booking flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DateAvailability {
    pub date: NaiveDate,
    /// `true` when at least one slot is still available (`remaining > 0`).
    pub available: bool,
    /// Effective capacity for this date (override or default).
    pub capacity: i32,
    /// Number of active (non-cancelled) bookings for this date.
    pub booked: i32,
    /// `capacity - booked`, clamped to zero.
    pub remaining: i32,
}

/// Result of checking availability for a customer's requested moving date.
///
/// **Caller**: `GET /api/v1/calendar/availability?date=YYYY-MM-DD` returns this
/// as JSON. When the requested date is unavailable, `alternatives` contains up
/// to `CalendarConfig::alternatives_count` nearby open dates sorted by proximity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailabilityResult {
    pub requested_date: NaiveDate,
    /// `true` when the requested date still has capacity.
    pub requested_date_available: bool,
    /// Full slot details for the requested date.
    pub requested_date_info: DateAvailability,
    /// Up to 3 nearest available dates (only populated when requested date is unavailable).
    pub alternatives: Vec<DateAvailability>,
}

/// A single day entry in the admin schedule view, combining availability
/// metadata with the list of active bookings for that day.
///
/// **Caller**: `GET /api/v1/calendar/schedule?from=…&to=…` returns a
/// `Vec<ScheduleEntry>`, one per calendar day in the requested range.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleEntry {
    pub date: NaiveDate,
    /// Capacity and booking counts for this date.
    pub availability: DateAvailability,
    /// All non-cancelled bookings for this date (may be empty).
    pub bookings: Vec<Booking>,
}
