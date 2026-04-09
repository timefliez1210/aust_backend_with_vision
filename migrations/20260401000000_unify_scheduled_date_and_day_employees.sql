-- Phase 1: Unify scheduled_date as the single date field.
-- Backfill scheduled_date from preferred_date where scheduled_date is NULL.
-- preferred_date column is kept but no longer read by application code.

-- 1. Backfill scheduled_date from preferred_date where missing
UPDATE inquiries
SET scheduled_date = preferred_date::date
WHERE scheduled_date IS NULL
  AND preferred_date IS NOT NULL;

-- 2. Add clock_in/clock_out to per-day employee tables so they can replace
--    the flat inquiry_employees / calendar_item_employees tables.
ALTER TABLE inquiry_day_employees
    ADD COLUMN IF NOT EXISTS clock_in  TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS clock_out TIMESTAMPTZ;

ALTER TABLE calendar_item_day_employees
    ADD COLUMN IF NOT EXISTS clock_in  TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS clock_out TIMESTAMPTZ;

-- 3. Backfill inquiry_days for single-day inquiries that have flat employee assignments
--    but no inquiry_days rows yet. This ensures the per-day tables are the single source
--    of truth going forward.

-- 3a. Create inquiry_days rows for single-day inquiries missing them
INSERT INTO inquiry_days (inquiry_id, day_date, day_number, start_time, end_time)
SELECT
    i.id,
    COALESCE(i.scheduled_date, i.preferred_date::date, i.created_at::date),
    1,
    i.start_time,
    i.end_time
FROM inquiries i
WHERE NOT EXISTS (
    SELECT 1 FROM inquiry_days id2 WHERE id2.inquiry_id = i.id
)
AND EXISTS (
    SELECT 1 FROM inquiry_employees ie WHERE ie.inquiry_id = i.id
);

-- 3b. Migrate flat inquiry_employees into inquiry_day_employees
INSERT INTO inquiry_day_employees (inquiry_day_id, employee_id, planned_hours, notes, clock_in, clock_out)
SELECT
    iday.id,
    ie.employee_id,
    ie.planned_hours,
    ie.notes,
    ie.clock_in,
    ie.clock_out
FROM inquiry_employees ie
JOIN inquiry_days iday ON iday.inquiry_id = ie.inquiry_id AND iday.day_number = 1
WHERE NOT EXISTS (
    SELECT 1 FROM inquiry_day_employees ide
    WHERE ide.inquiry_day_id = iday.id AND ide.employee_id = ie.employee_id
);

-- 4. Same for calendar items: create calendar_item_days rows for items missing them
INSERT INTO calendar_item_days (calendar_item_id, day_date, day_number, start_time, end_time)
SELECT
    ci.id,
    COALESCE(ci.scheduled_date, ci.created_at::date),
    1,
    ci.start_time,
    ci.end_time
FROM calendar_items ci
WHERE NOT EXISTS (
    SELECT 1 FROM calendar_item_days cid WHERE cid.calendar_item_id = ci.id
)
AND EXISTS (
    SELECT 1 FROM calendar_item_employees cie WHERE cie.calendar_item_id = ci.id
);

-- 4b. Migrate flat calendar_item_employees into calendar_item_day_employees
INSERT INTO calendar_item_day_employees (calendar_item_day_id, employee_id, planned_hours, notes, clock_in, clock_out)
SELECT
    cday.id,
    cie.employee_id,
    cie.planned_hours,
    cie.notes,
    cie.clock_in,
    cie.clock_out
FROM calendar_item_employees cie
JOIN calendar_item_days cday ON cday.calendar_item_id = cie.calendar_item_id AND cday.day_number = 1
WHERE NOT EXISTS (
    SELECT 1 FROM calendar_item_day_employees cdde
    WHERE cdde.calendar_item_day_id = cday.id AND cdde.employee_id = cie.employee_id
);