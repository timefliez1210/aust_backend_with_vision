# crates/flash-contact-bot — Flash-Contact Telegram Sidecar

Standalone binary, separate from `aust-email-agent`'s Telegram bot. Long-polls
its own bot token so the two pollers don't fight over `getUpdates` (Telegram
allows only one long-poller per token).

## Files

- `src/main.rs` — reads env, connects to Postgres, calls `bot::run`.
- `src/bot.rs` — the poll loop and callback dispatch.

## Behavior

`bot::run(db, bot_token, admin_chat_id)` polls `getUpdates` (30s long-poll,
exponential backoff on error, capped at 60s) filtered to `callback_query` only.
For each callback:

- Authorization: accepts clicks where `from.id == admin_chat_id` OR the
  message's chat id equals `admin_chat_id` (covers both a private chat with
  Alex and a group chat). Anything else gets "Nicht autorisiert." and is dropped.
- `fc_reached:<uuid>` → `aust_flash_contact::mark_handled`
- `fc_snooze:<uuid>` → looks up the contact's `time_preference` directly via SQL
  (`fetch_preference`, ad-hoc query, not a repo function), computes
  `next_snooze()`, calls `schedule_snooze`
- `fc_dismiss:<uuid>` → `aust_flash_contact::mark_dismissed`

Every branch answers the callback query (toast text) so the Telegram client
clears its loading spinner, including on parse/DB failure.

## Env

- `AUST__DATABASE__URL` (or `DATABASE_URL`) — Postgres connection.
- `AUST__TELEGRAM__FLASH_CONTACT_BOT_TOKEN` — dedicated bot token, distinct from
  the main bot's `AUST__TELEGRAM__BOT_TOKEN`.
- `AUST__TELEGRAM__ADMIN_CHAT_ID` — same admin chat id used by the main bot.

## Relationship to `crates/flash-contact` and `crates/api`

This binary only *consumes* callback button presses. The reminder messages
those buttons are attached to are sent by
`crates/api::services::flash_contact_service::run_reminder_check`, which runs
inside the main `aust_backend` process on its own schedule — not from here.
See `crates/flash-contact/AGENTS.md` for the shared domain logic and the full
message flow.
