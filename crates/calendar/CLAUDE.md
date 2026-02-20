# crates/calendar — Booking & Capacity Management

Moving schedule management with capacity tracking, booking creation, and availability queries.

## Key Files

- `src/service.rs` - Business logic (CalendarService)
- `src/repository.rs` - SQL queries (CalendarRepository)
- `src/models.rs` - Domain types
- `src/error.rs` - CalendarError enum

## Architecture

Service/Repository pattern:
- **CalendarService** — high-level operations, business rules (capacity checks, Sunday skipping, alternatives)
- **CalendarRepository** — raw SQLx queries against PostgreSQL

## Key Types

- `Booking` — full DB record (id, booking_date, quote_id, customer info, volume, distance, status)
- `NewBooking` — input for creation
- `CapacityOverride` — date-specific capacity limit
- `DateAvailability` — (date, available, capacity, booked, remaining)
- `AvailabilityResult` — requested date info + nearest alternatives
- `ScheduleEntry` — date + availability + active bookings for that date

## Service Methods

| Method | Description |
|--------|-------------|
| `check_availability(date)` | Returns availability + N nearest alternatives |
| `create_booking(booking)` | Creates if capacity allows, else error |
| `force_create_booking(booking)` | Admin override, ignores capacity limits |
| `cancel_booking(id)` | Sets status to cancelled |
| `confirm_booking(id)` | Sets status to confirmed |
| `set_capacity(date, capacity)` | Override default capacity for a date |
| `get_schedule(from, to)` | All bookings + availability for date range |
| `find_nearest_available(around, count)` | Fuzzy date search (skips Sundays, past dates) |

## Database Tables

- `calendar_bookings` — booking records (see `migrations/20260219000000_calendar.sql`)
- `calendar_capacity_overrides` — per-date capacity limits

## Configuration

Uses `CalendarConfig` from core:
- `default_capacity` — daily booking limit (default: 3)
- `alternatives_count` — how many alternatives to suggest (default: 3)
- `search_window_days` — how far to search for alternatives (default: 30)
