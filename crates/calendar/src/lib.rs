/// Moving job calendar: booking management, capacity tracking, and availability queries.
///
/// Public surface:
/// - [`CalendarService`] — business logic layer; use this from API route handlers
///   and the email agent.
/// - [`CalendarError`] — all failure modes (not found, fully booked, validation, DB).
/// - Re-exports from `models`: [`Booking`], [`NewBooking`], [`CapacityOverride`],
///   [`DateAvailability`], [`AvailabilityResult`], [`ScheduleEntry`].
///
/// # Usage
/// ```ignore
/// let service = CalendarService::new(pool, default_capacity, alternatives_count, search_window_days);
/// let avail = service.check_availability(date).await?;
/// let booking = service.create_booking(new_booking).await?;
/// ```
pub mod error;
pub mod models;
pub mod repository;
pub mod service;

pub use error::CalendarError;
pub use models::*;
pub use service::CalendarService;
