-- Vehicle management + per-vehicle reminders (TÜV, Ölwechsel, …).
--
-- Alex owns several cars/trucks/transporters. Each vehicle carries an open list
-- of free-form reminders, each with a due date. A background tick (see
-- crates/api/src/services/vehicle_reminder_service.rs) pings the admin Telegram
-- chat on an escalating cadence as the due date approaches:
--   21 days before → one ping
--   14 days before → one ping
--    7 days before → one ping, then DAILY from 7 days out until the date,
--                     and DAILY past the due date (ÜBERFÄLLIG) until the
--                     reminder is marked done/dismissed (active = FALSE).
--
-- last_pinged_on dedupes the 60-second tick to at most one ping per calendar
-- day (Europe/Berlin) per reminder.

CREATE TABLE IF NOT EXISTS vehicles (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    label      TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS vehicle_reminders (
    id             UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    vehicle_id     UUID        NOT NULL REFERENCES vehicles(id) ON DELETE CASCADE,
    label          TEXT        NOT NULL,
    due_date       DATE        NOT NULL,
    -- active = the reminder is still pending. Set FALSE to stop the nag once
    -- Alex has handled it (done) or no longer cares (dismissed).
    active         BOOLEAN     NOT NULL DEFAULT TRUE,
    completed_at   TIMESTAMPTZ,
    -- Calendar day (Europe/Berlin) the last Telegram ping was sent. NULL = never.
    last_pinged_on DATE,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Fast scan for the fire loop: active reminders ordered by due date.
CREATE INDEX IF NOT EXISTS idx_vehicle_reminders_active_due
    ON vehicle_reminders (due_date) WHERE active;

CREATE INDEX IF NOT EXISTS idx_vehicle_reminders_vehicle
    ON vehicle_reminders (vehicle_id);
