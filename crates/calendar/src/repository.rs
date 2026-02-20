use chrono::NaiveDate;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::CalendarError;
use crate::models::{Booking, CapacityOverride};

/// Count active (non-cancelled) bookings for a specific date.
pub async fn count_bookings_for_date(
    pool: &PgPool,
    date: NaiveDate,
) -> Result<i32, CalendarError> {
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM calendar_bookings WHERE booking_date = $1 AND status != 'cancelled'",
    )
    .bind(date)
    .fetch_one(pool)
    .await?;

    Ok(row.0 as i32)
}

/// Get the capacity override for a specific date, if any.
pub async fn get_capacity_override(
    pool: &PgPool,
    date: NaiveDate,
) -> Result<Option<CapacityOverride>, CalendarError> {
    let row = sqlx::query_as::<_, CapacityOverride>(
        "SELECT id, override_date, capacity, created_at FROM calendar_capacity_overrides WHERE override_date = $1",
    )
    .bind(date)
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

/// Get all active bookings for a specific date.
pub async fn get_bookings_for_date(
    pool: &PgPool,
    date: NaiveDate,
) -> Result<Vec<Booking>, CalendarError> {
    let rows = sqlx::query_as::<_, Booking>(
        r#"
        SELECT id, booking_date, quote_id, customer_name, customer_email,
               departure_address, arrival_address, volume_m3, distance_km,
               description, status, created_at, updated_at
        FROM calendar_bookings
        WHERE booking_date = $1 AND status != 'cancelled'
        ORDER BY created_at
        "#,
    )
    .bind(date)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Get all bookings (including cancelled) for a date range.
pub async fn get_bookings_in_range(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<Booking>, CalendarError> {
    let rows = sqlx::query_as::<_, Booking>(
        r#"
        SELECT id, booking_date, quote_id, customer_name, customer_email,
               departure_address, arrival_address, volume_m3, distance_km,
               description, status, created_at, updated_at
        FROM calendar_bookings
        WHERE booking_date >= $1 AND booking_date <= $2
        ORDER BY booking_date, created_at
        "#,
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Get capacity overrides for a date range.
pub async fn get_capacity_overrides_in_range(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<CapacityOverride>, CalendarError> {
    let rows = sqlx::query_as::<_, CapacityOverride>(
        r#"
        SELECT id, override_date, capacity, created_at
        FROM calendar_capacity_overrides
        WHERE override_date >= $1 AND override_date <= $2
        ORDER BY override_date
        "#,
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Insert a new booking.
pub async fn insert_booking(
    pool: &PgPool,
    id: Uuid,
    booking: &crate::models::NewBooking,
) -> Result<Booking, CalendarError> {
    let row = sqlx::query_as::<_, Booking>(
        r#"
        INSERT INTO calendar_bookings
            (id, booking_date, quote_id, customer_name, customer_email,
             departure_address, arrival_address, volume_m3, distance_km,
             description, status)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        RETURNING id, booking_date, quote_id, customer_name, customer_email,
                  departure_address, arrival_address, volume_m3, distance_km,
                  description, status, created_at, updated_at
        "#,
    )
    .bind(id)
    .bind(booking.booking_date)
    .bind(booking.quote_id)
    .bind(&booking.customer_name)
    .bind(&booking.customer_email)
    .bind(&booking.departure_address)
    .bind(&booking.arrival_address)
    .bind(booking.volume_m3)
    .bind(booking.distance_km)
    .bind(&booking.description)
    .bind(&booking.status)
    .fetch_one(pool)
    .await?;

    Ok(row)
}

/// Update a booking's status.
pub async fn update_booking_status(
    pool: &PgPool,
    id: Uuid,
    status: &str,
) -> Result<Option<Booking>, CalendarError> {
    let row = sqlx::query_as::<_, Booking>(
        r#"
        UPDATE calendar_bookings SET status = $2, updated_at = NOW()
        WHERE id = $1
        RETURNING id, booking_date, quote_id, customer_name, customer_email,
                  departure_address, arrival_address, volume_m3, distance_km,
                  description, status, created_at, updated_at
        "#,
    )
    .bind(id)
    .bind(status)
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

/// Upsert a capacity override for a specific date.
pub async fn upsert_capacity_override(
    pool: &PgPool,
    date: NaiveDate,
    capacity: i32,
) -> Result<CapacityOverride, CalendarError> {
    let row = sqlx::query_as::<_, CapacityOverride>(
        r#"
        INSERT INTO calendar_capacity_overrides (id, override_date, capacity)
        VALUES (gen_random_uuid(), $1, $2)
        ON CONFLICT (override_date)
        DO UPDATE SET capacity = EXCLUDED.capacity
        RETURNING id, override_date, capacity, created_at
        "#,
    )
    .bind(date)
    .bind(capacity)
    .fetch_one(pool)
    .await?;

    Ok(row)
}

/// Get a single booking by ID.
pub async fn get_booking_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<Booking>, CalendarError> {
    let row = sqlx::query_as::<_, Booking>(
        r#"
        SELECT id, booking_date, quote_id, customer_name, customer_email,
               departure_address, arrival_address, volume_m3, distance_km,
               description, status, created_at, updated_at
        FROM calendar_bookings
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row)
}
