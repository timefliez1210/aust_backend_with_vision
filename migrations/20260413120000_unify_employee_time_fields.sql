-- Unify time tracking fields across all 4 employee-assignment tables.
--
-- Goal: every table gets the same 6 admin-editable time fields:
--   start_time    TIME  - planned start
--   end_time      TIME  - planned end
--   clock_in      TIME  - actual arrival   (was TIMESTAMP, preserve time portion)
--   clock_out     TIME  - actual departure (was TIMESTAMP, preserve time portion)
--   break_minutes INT   - break deduction (new)
--   actual_hours  NUMERIC(5,2) - NULL = auto-derived, non-NULL = manual override (new)
--
-- employee_clock_in / employee_clock_out (worker-app self-reports) are left untouched.

-- ── inquiry_employees ──────────────────────────────────────────────────────────
ALTER TABLE inquiry_employees
    ADD COLUMN IF NOT EXISTS start_time    TIME,
    ADD COLUMN IF NOT EXISTS end_time      TIME,
    ADD COLUMN IF NOT EXISTS break_minutes INT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS actual_hours  NUMERIC(5,2);

-- Convert admin clock_in / clock_out from TIMESTAMP to TIME
ALTER TABLE inquiry_employees
    ADD COLUMN IF NOT EXISTS clock_in_t  TIME,
    ADD COLUMN IF NOT EXISTS clock_out_t TIME;

UPDATE inquiry_employees
   SET clock_in_t  = (clock_in  AT TIME ZONE 'UTC')::TIME
 WHERE clock_in  IS NOT NULL;
UPDATE inquiry_employees
   SET clock_out_t = (clock_out AT TIME ZONE 'UTC')::TIME
 WHERE clock_out IS NOT NULL;

ALTER TABLE inquiry_employees DROP COLUMN IF EXISTS clock_in;
ALTER TABLE inquiry_employees DROP COLUMN IF EXISTS clock_out;
ALTER TABLE inquiry_employees RENAME COLUMN clock_in_t  TO clock_in;
ALTER TABLE inquiry_employees RENAME COLUMN clock_out_t TO clock_out;

-- ── inquiry_day_employees ─────────────────────────────────────────────────────
ALTER TABLE inquiry_day_employees
    ADD COLUMN IF NOT EXISTS break_minutes INT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS actual_hours  NUMERIC(5,2);

-- start_time / end_time already added in migration 20260413100000
-- Convert clock_in / clock_out from TIMESTAMP to TIME
ALTER TABLE inquiry_day_employees
    ADD COLUMN IF NOT EXISTS clock_in_t  TIME,
    ADD COLUMN IF NOT EXISTS clock_out_t TIME;

UPDATE inquiry_day_employees
   SET clock_in_t  = (clock_in  AT TIME ZONE 'UTC')::TIME
 WHERE clock_in  IS NOT NULL;
UPDATE inquiry_day_employees
   SET clock_out_t = (clock_out AT TIME ZONE 'UTC')::TIME
 WHERE clock_out IS NOT NULL;

ALTER TABLE inquiry_day_employees DROP COLUMN IF EXISTS clock_in;
ALTER TABLE inquiry_day_employees DROP COLUMN IF EXISTS clock_out;
ALTER TABLE inquiry_day_employees RENAME COLUMN clock_in_t  TO clock_in;
ALTER TABLE inquiry_day_employees RENAME COLUMN clock_out_t TO clock_out;

-- ── calendar_item_employees ───────────────────────────────────────────────────
ALTER TABLE calendar_item_employees
    ADD COLUMN IF NOT EXISTS start_time    TIME,
    ADD COLUMN IF NOT EXISTS end_time      TIME,
    ADD COLUMN IF NOT EXISTS break_minutes INT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS actual_hours  NUMERIC(5,2);

ALTER TABLE calendar_item_employees
    ADD COLUMN IF NOT EXISTS clock_in_t  TIME,
    ADD COLUMN IF NOT EXISTS clock_out_t TIME;

UPDATE calendar_item_employees
   SET clock_in_t  = (clock_in  AT TIME ZONE 'UTC')::TIME
 WHERE clock_in  IS NOT NULL;
UPDATE calendar_item_employees
   SET clock_out_t = (clock_out AT TIME ZONE 'UTC')::TIME
 WHERE clock_out IS NOT NULL;

ALTER TABLE calendar_item_employees DROP COLUMN IF EXISTS clock_in;
ALTER TABLE calendar_item_employees DROP COLUMN IF EXISTS clock_out;
ALTER TABLE calendar_item_employees RENAME COLUMN clock_in_t  TO clock_in;
ALTER TABLE calendar_item_employees RENAME COLUMN clock_out_t TO clock_out;

-- ── calendar_item_day_employees ───────────────────────────────────────────────
ALTER TABLE calendar_item_day_employees
    ADD COLUMN IF NOT EXISTS break_minutes INT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS actual_hours  NUMERIC(5,2);

-- start_time / end_time already added in migration 20260413100000
ALTER TABLE calendar_item_day_employees
    ADD COLUMN IF NOT EXISTS clock_in_t  TIME,
    ADD COLUMN IF NOT EXISTS clock_out_t TIME;

UPDATE calendar_item_day_employees
   SET clock_in_t  = (clock_in  AT TIME ZONE 'UTC')::TIME
 WHERE clock_in  IS NOT NULL;
UPDATE calendar_item_day_employees
   SET clock_out_t = (clock_out AT TIME ZONE 'UTC')::TIME
 WHERE clock_out IS NOT NULL;

ALTER TABLE calendar_item_day_employees DROP COLUMN IF EXISTS clock_in;
ALTER TABLE calendar_item_day_employees DROP COLUMN IF EXISTS clock_out;
ALTER TABLE calendar_item_day_employees RENAME COLUMN clock_in_t  TO clock_in;
ALTER TABLE calendar_item_day_employees RENAME COLUMN clock_out_t TO clock_out;
