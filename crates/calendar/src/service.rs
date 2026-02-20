use chrono::{Datelike, NaiveDate, Utc};
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::info;
use uuid::Uuid;

use crate::error::CalendarError;
use crate::models::*;
use crate::repository;

/// Calendar service handling availability checking, booking management,
/// and capacity overrides.
pub struct CalendarService {
    pool: PgPool,
    default_capacity: i32,
    alternatives_count: usize,
    search_window_days: i64,
}

impl CalendarService {
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

    /// Get the effective capacity for a date (override or default).
    async fn effective_capacity(&self, date: NaiveDate) -> Result<i32, CalendarError> {
        if let Some(ovr) = repository::get_capacity_override(&self.pool, date).await? {
            Ok(ovr.capacity)
        } else {
            Ok(self.default_capacity)
        }
    }

    /// Build a DateAvailability for a single date.
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

    /// Check availability for a specific date.
    /// If unavailable, also returns up to N nearest alternative dates.
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
    /// Searches outward (+1, -1, +2, -2, ...), skipping Sundays and past dates.
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
    pub async fn get_bookings_for_date(
        &self,
        date: NaiveDate,
    ) -> Result<Vec<Booking>, CalendarError> {
        repository::get_bookings_for_date(&self.pool, date).await
    }

    /// Create a booking, respecting capacity limits.
    /// Returns an error if the date is fully booked.
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

    /// Force-create a booking, ignoring capacity limits (admin override).
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

    /// Cancel a booking.
    pub async fn cancel_booking(&self, id: Uuid) -> Result<Booking, CalendarError> {
        let booking = repository::update_booking_status(&self.pool, id, "cancelled")
            .await?
            .ok_or_else(|| CalendarError::NotFound(format!("Booking {id} not found")))?;

        info!("Cancelled booking {}", id);
        Ok(booking)
    }

    /// Confirm a tentative booking.
    pub async fn confirm_booking(&self, id: Uuid) -> Result<Booking, CalendarError> {
        let booking = repository::update_booking_status(&self.pool, id, "confirmed")
            .await?
            .ok_or_else(|| CalendarError::NotFound(format!("Booking {id} not found")))?;

        info!("Confirmed booking {}", id);
        Ok(booking)
    }

    /// Set a capacity override for a specific date.
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

    /// Get the schedule (availability + bookings) for a date range.
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

    /// Get a single booking by ID.
    pub async fn get_booking(&self, id: Uuid) -> Result<Booking, CalendarError> {
        repository::get_booking_by_id(&self.pool, id)
            .await?
            .ok_or_else(|| CalendarError::NotFound(format!("Booking {id} not found")))
    }
}
