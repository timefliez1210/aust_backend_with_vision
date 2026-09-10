# crates/flash-contact — Callback Request Domain Logic

DB-backed library, no HTTP. Backs the public "call me back" landing-page form
(ultra-quick: name, phone, time window) and the reminder cron/bot that follow up.

## What's here (`src/lib.rs`)

- `TimePreference` — `Gleich` (call ASAP, no reminder) | `Vormittag` (09:00) |
  `Nachmittag` (13:00). `serde` renames (`gleich`/`vormittag`/`nachmittag`) match
  the landing-page UI values exactly and the `flash_contacts.time_preference` column.
- `FlashContact` / `CreateFlashContact` — row struct and insert input.
- Repo functions: `insert`, `fetch_pending_reminders`, `mark_reminder_sent`,
  `mark_handled`, `mark_dismissed`, `schedule_snooze`. All raw `sqlx::query` (no
  `sqlx::query!` macro, so no compile-time schema check against this table).
- `reminder_time()` — when the next reminder should fire for a contact (wall-clock
  Europe/Berlin, DST-aware via `chrono_tz`). Snoozed contacts (`next_remind_at` set)
  short-circuit to that timestamp.
- `next_snooze()` — escalation ladder after "Nochmal erinnern": Vormittag cycles
  08→11, Nachmittag cycles 13→16, rolling to the next day once both slots today
  have passed.
- `format_immediate_message` / `format_reminder_message` — German Telegram text.

## Who calls this

- `crates/api::routes::flash_contact` — public `POST /api/v1/flash-contact`,
  unauthenticated, rate-limited (10 req/60s, its own limiter instance). Inserts
  the contact and sends the immediate Telegram ping via `telegram_service` on the
  **main** bot token.
- `crates/api::services::flash_contact_service::run_reminder_check` — polled
  periodically (see caller in `main.rs`/scheduler), sends the delayed reminder
  with an inline keyboard (✅ Erreicht / 🔁 Nochmal erinnern / 🗑 Verwerfen) via a
  **separate** bot token (`flash_contact_bot_token`) so its `getUpdates` polling
  doesn't collide with the main email-agent bot's poller.
- `crates/flash-contact-bot` (separate binary) — long-polls that second bot's
  callback_query updates and dispatches `fc_reached:<id>` / `fc_snooze:<id>` /
  `fc_dismiss:<id>` back into this crate's `mark_handled`/`schedule_snooze`/`mark_dismissed`.

## Testing

Pure functions (`reminder_time`, `next_snooze`) are unit-tested inline with fixed
`NaiveDate`s — no DB needed. `flash_contact_service`'s cron-cycle tests are
`#[sqlx::test]` (spin up a real migrated DB) plus an in-process mock Telegram
server; those live in `crates/api`, not here.
