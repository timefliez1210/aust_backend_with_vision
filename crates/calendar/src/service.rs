use chrono::{Datelike, NaiveDate, Utc};
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::info;
use uuid::Uuid;

use crate::error::CalendarError;
use crate::models::*;
use crate::repository;

/// Business logic layer for moving job calendar management.
///
/// Wraps the raw SQL repository with capacity checks, Sunday skipping, and
/// alternative date suggestions. Instantiated once at startup and shared via
/// `Arc<CalendarService>`.
///
/// **Caller**: API route handlers in `crates/api/src/routes/calendar.rs` and
/// the Telegram approval flow in `crates/email-agent`.
pub struct CalendarService {
    pool: PgPool,
    /// Maximum simultaneous bookings per day (from `CalendarConfig::default_capacity`).
    default_capacity: i32,
    /// How many alternative dates to suggest when a requested date is full.
    alternatives_count: usize,
    /// How far forward/backward to search for alternatives (in calendar days).
    search_window_days: i64,
}

impl CalendarService {
    /// Creates a new `CalendarService`.
    ///
    /// # Parameters
    /// - `pool` — Shared PostgreSQL connection pool.
    /// - `default_capacity` — Maximum bookings per day when no override exists.
    /// - `alternatives_count` — Number of alternative dates returned when the
    ///   requested date is fully booked.
    /// - `search_window_days` — Maximum offset (days) to search for alternatives.
    pub fn new(
        pool: PgPool,
        default_capacity: i32,
        alternatives_count: usize,
        search_window_days: i64,
    ) -> Self {
        Self {
            pool,
            default_capacity,
            alternatives_count,
            search_window_days,
        }
    }

    /// Get the effective booking capacity for a date.
    ///
    /// Returns the `calendar_capacity_overrides` value when one exists for the
    /// date, otherwise falls back to `default_capacity`.
    async fn effective_capacity(&self, date: NaiveDate) -> Result<i32, CalendarError> {
        if let Some(ovr) = repository::get_capacity_override(&self.pool, date).await? {
            Ok(ovr.capacity)
        } else {
            Ok(self.default_capacity)
        }
    }

    /// Build a `DateAvailability` snapshot for a single date.
    ///
    /// Queries active booking count and compares it to the effective capacity.
    async fn date_availability(&self, date: NaiveDate) -> Result<DateAvailability, CalendarError> {
        let capacity = self.effective_capacity(date).await?;
        let booked = repository::count_bookings_for_date(&self.pool, date).await?;
        let remaining = (capacity - booked).max(0);

        Ok(DateAvailability {
            date,
            available: remaining > 0,
            capacity,
            booked,
            remaining,
        })
    }

    /// Check availability for a specific date, returning alternative dates when
    /// the requested date is fully booked.
    ///
    /// **Caller**: `GET /api/v1/calendar/availability?date=YYYY-MM-DD`.
    ///
    /// # Parameters
    /// - `date` — The customer's preferred moving date.
    ///
    /// # Returns
    /// An `AvailabilityResult` with the requested date's slot info and, when the
    /// date is unavailable, up to `alternatives_count` nearby open dates.
    pub async fn check_availability(
        &self,
        date: NaiveDate,
    ) -> Result<AvailabilityResult, CalendarError> {
        let info = self.date_availability(date).await?;
        let available = info.available;

        let alternatives = if !available {
            self.find_nearest_available(date, self.alternatives_count)
                .await?
        } else {
            Vec::new()
        };

        Ok(AvailabilityResult {
            requested_date: date,
            requested_date_available: available,
            requested_date_info: info,
            alternatives,
        })
    }

    /// Find the N nearest available dates around a target date.
    ///
    /// Searches outward from the target (+1 day, -1 day, +2 days, -2 days, …),
    /// skipping Sundays and dates in the past.
    ///
    /// **Caller**: `check_availability` (when the requested date is full) and
    /// the Telegram approval flow (to suggest rescheduling options).
    ///
    /// # Parameters
    /// - `around` — The date to search from.
    /// - `count` — Maximum number of alternatives to return.
    ///
    /// # Returns
    /// Up to `count` available `DateAvailability` entries sorted by proximity
    /// to `around`.
    pub async fn find_nearest_available(
        &self,
        around: NaiveDate,
        count: usize,
    ) -> Result<Vec<DateAvailability>, CalendarError> {
        let today = Utc::now().date_naive();
        let mut results = Vec::new();
        let mut offset = 1i64;

        while results.len() < count && offset <= self.search_window_days {
            // Check future date
            let future = around + chrono::Days::new(offset as u64);
            if future.weekday() != chrono::Weekday::Sun {
                let avail = self.date_availability(future).await?;
                if avail.available {
                    results.push(avail);
                    if results.len() >= count {
                        break;
                    }
                }
            }

            // Check past date (only if not in the past)
            let past = around - chrono::Days::new(offset as u64);
            if past >= today && past.weekday() != chrono::Weekday::Sun {
                let avail = self.date_availability(past).await?;
                if avail.available {
                    results.push(avail);
                }
            }

            offset += 1;
        }

        // Sort by proximity to the requested date
        results.sort_by_key(|a| {
            let diff = (a.date - around).num_days().unsigned_abs();
            diff
        });

        results.truncate(count);
        Ok(results)
    }

    /// Get all active bookings for a specific date.
    ///
    /// **Caller**: Admin dashboard day-detail view.
    pub async fn get_bookings_for_date(
        &self,
        date: NaiveDate,
    ) -> Result<Vec<Booking>, CalendarError> {
        repository::get_bookings_for_date(&self.pool, date).await
    }

    /// Create a booking for a date, respecting the effective capacity limit.
    ///
    /// **Caller**: `POST /api/v1/calendar/bookings` and the email agent when a
    /// customer's preferred date is available.
    ///
    /// # Returns
    /// The newly created `Booking` record with its generated UUID and timestamps.
    ///
    /// # Errors
    /// - `CalendarError::FullyBooked` — the date has reached its capacity limit.
    ///   The error message includes slot counts so the Telegram flow can present
    ///   a force-book option to Alex.
    pub async fn create_booking(
        &self,
        booking: NewBooking,
    ) -> Result<Booking, CalendarError> {
        let avail = self.date_availability(booking.booking_date).await?;
        if !avail.available {
            return Err(CalendarError::FullyBooked(format!(
                "{} is fully booked ({}/{} slots used)",
                booking.booking_date, avail.booked, avail.capacity
            )));
        }

        let id = Uuid::now_v7();
        let result = repository::insert_booking(&self.pool, id, &booking).await?;
        info!(
            "Created booking {} for {} (customer: {})",
            result.id,
            result.booking_date,
            result.customer_name.as_deref().unwrap_or("unknown")
        );
        Ok(result)
    }

    /// Force-create a booking, ignoring the capacity limit (admin override).
    ///
    /// **Caller**: Telegram approval flow when Alex explicitly approves overbooking
    /// after receiving a `CalendarError::FullyBooked` from `create_booking`.
    ///
    /// # Returns
    /// The newly created `Booking` record.
    pub async fn force_create_booking(
        &self,
        booking: NewBooking,
    ) -> Result<Booking, CalendarError> {
        let id = Uuid::now_v7();
        let result = repository::insert_booking(&self.pool, id, &booking).await?;
        info!(
            "Force-created booking {} for {} (admin override)",
            result.id, result.booking_date
        );
        Ok(result)
    }

    /// Set a booking's status to `"cancelled"`.
    ///
    /// **Caller**: `PATCH /api/v1/calendar/bookings/{id}` with `status: "cancelled"`.
    ///
    /// # Errors
    /// - `CalendarError::NotFound` — booking ID does not exist.
    pub async fn cancel_booking(&self, id: Uuid) -> Result<Booking, CalendarError> {
        let booking = repository::update_booking_status(&self.pool, id, "cancelled")
            .await?
            .ok_or_else(|| CalendarError::NotFound(format!("Booking {id} not found")))?;

        info!("Cancelled booking {}", id);
        Ok(booking)
    }

    /// Set a booking's status to `"confirmed"`.
    ///
    /// **Caller**: `PATCH /api/v1/calendar/bookings/{id}` with `status: "confirmed"`,
    /// or the email agent when a customer accepts a tentative booking.
    ///
    /// # Errors
    /// - `CalendarError::NotFound` — booking ID does not exist.
    pub async fn confirm_booking(&self, id: Uuid) -> Result<Booking, CalendarError> {
        let booking = repository::update_booking_status(&self.pool, id, "confirmed")
            .await?
            .ok_or_else(|| CalendarError::NotFound(format!("Booking {id} not found")))?;

        info!("Confirmed booking {}", id);
        Ok(booking)
    }

    /// Set or update the capacity limit for a specific date.
    ///
    /// **Caller**: `PUT /api/v1/calendar/capacity/{date}` and the Telegram
    /// `/kapazitaet` command.
    ///
    /// Setting `capacity = 0` effectively blocks the date from new bookings.
    ///
    /// # Parameters
    /// - `date` — The calendar date to override.
    /// - `capacity` — New maximum booking count (must be ≥ 0).
    ///
    /// # Errors
    /// - `CalendarError::Validation` — `capacity` is negative.
    pub async fn set_capacity(
        &self,
        date: NaiveDate,
        capacity: i32,
    ) -> Result<CapacityOverride, CalendarError> {
        if capacity < 0 {
            return Err(CalendarError::Validation(
                "Capacity must be >= 0".to_string(),
            ));
        }

        let result = repository::upsert_capacity_override(&self.pool, date, capacity).await?;
        info!("Set capacity override for {}: {}", date, capacity);
        Ok(result)
    }

    /// Get the full schedule (availability + bookings) for a date range.
    ///
    /// **Caller**: `GET /api/v1/calendar/schedule?from=…&to=…`. The admin
    /// dashboard uses this to render the monthly calendar view.
    ///
    /// # Parameters
    /// - `from` — First date of the range (inclusive).
    /// - `to` — Last date of the range (inclusive). Capped to 90 days by the
    ///   API route handler.
    ///
    /// # Returns
    /// One `ScheduleEntry` per day from `from` to `to`, including days with no
    /// bookings (so the calendar always has a complete grid).
    pub async fn get_schedule(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<ScheduleEntry>, CalendarError> {
        let bookings = repository::get_bookings_in_range(&self.pool, from, to).await?;
        let overrides = repository::get_capacity_overrides_in_range(&self.pool, from, to).await?;

        // Index overrides by date
        let override_map: HashMap<NaiveDate, i32> = overrides
            .into_iter()
            .map(|o| (o.override_date, o.capacity))
            .collect();

        // Index bookings by date
        let mut booking_map: HashMap<NaiveDate, Vec<Booking>> = HashMap::new();
        for b in bookings {
            booking_map
                .entry(b.booking_date)
                .or_default()
                .push(b);
        }

        let mut entries = Vec::new();
        let mut current = from;
        while current <= to {
            let capacity = override_map
                .get(&current)
                .copied()
                .unwrap_or(self.default_capacity);
            let day_bookings = booking_map.remove(&current).unwrap_or_default();
            let active_count = day_bookings
                .iter()
                .filter(|b| b.status != "cancelled")
                .count() as i32;
            let remaining = (capacity - active_count).max(0);

            entries.push(ScheduleEntry {
                date: current,
                availability: DateAvailability {
                    date: current,
                    available: remaining > 0,
                    capacity,
                    booked: active_count,
                    remaining,
                },
                bookings: day_bookings,
            });

            current = current.succ_opt().unwrap();
        }

        Ok(entries)
    }

    /// Permanently delete a booking by ID.
    ///
    /// **Caller**: Admin-only `DELETE /api/v1/calendar/bookings/{id}`.
    /// Prefer `cancel_booking` for normal workflow; `delete_booking` is for
    /// data cleanup only.
    ///
    /// # Errors
    /// - `CalendarError::NotFound` — booking ID does not exist.
    pub async fn delete_booking(&self, id: Uuid) -> Result<(), CalendarError> {
        let deleted = repository::delete_booking(&self.pool, id).await?;
        if !deleted {
            return Err(CalendarError::NotFound(format!("Booking {id} not found")));
        }
        info!("Deleted booking {}", id);
        Ok(())
    }

    /// Fetch a single booking by its UUID.
    ///
    /// **Caller**: `GET /api/v1/calendar/bookings/{id}`.
    ///
    /// # Errors
    /// - `CalendarError::NotFound` — booking ID does not exist.
    pub async fn get_booking(&self, id: Uuid) -> Result<Booking, CalendarError> {
        repository::get_booking_by_id(&self.pool, id)
            .await?
            .ok_or_else(|| CalendarError::NotFound(format!("Booking {id} not found")))
    }
}
