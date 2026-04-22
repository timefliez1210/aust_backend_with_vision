-- Collapse inquiry_days / inquiry_day_employees into inquiry_employees with a job_date column.
-- Same for calendar_item_days / calendar_item_day_employees → calendar_item_employees.
-- After this migration there is one row per (inquiry, employee, job_date) in the flat tables.
-- Multi-day spans are expressed via inquiries.end_date (NULL = same as scheduled_date).

-- ── Add end_date to parent tables ────────────────────────────────────────────
ALTER TABLE inquiries     ADD COLUMN IF NOT EXISTS end_date DATE;
ALTER TABLE calendar_items ADD COLUMN IF NOT EXISTS end_date DATE;

-- Populate end_date from the last day in inquiry_days
UPDATE inquiries i
SET end_date = (
    SELECT MAX(day_date) FROM inquiry_days WHERE inquiry_id = i.id
)
WHERE EXISTS (SELECT 1 FROM inquiry_days WHERE inquiry_id = i.id);

UPDATE calendar_items ci
SET end_date = (
    SELECT MAX(day_date) FROM calendar_item_days WHERE calendar_item_id = ci.id
)
WHERE EXISTS (SELECT 1 FROM calendar_item_days WHERE calendar_item_id = ci.id);

-- ── Add job_date to flat employee tables ──────────────────────────────────────
ALTER TABLE inquiry_employees      ADD COLUMN IF NOT EXISTS job_date DATE;
ALTER TABLE calendar_item_employees ADD COLUMN IF NOT EXISTS job_date DATE;

-- ── Rebuild inquiry_employees rows for multi-day inquiries ───────────────────
-- For inquiries that have day rows, the flat table contains stale aggregates.
-- Delete the aggregate rows, then re-insert one row per (employee, day) from
-- inquiry_day_employees.

DELETE FROM inquiry_employees ie
WHERE EXISTS (SELECT 1 FROM inquiry_days WHERE inquiry_id = ie.inquiry_id);

INSERT INTO inquiry_employees (
    id, inquiry_id, employee_id, job_date,
    planned_hours, notes,
    start_time, end_time, clock_in, clock_out,
    break_minutes, actual_hours
)
SELECT
    gen_random_uuid(),
    iday.inquiry_id,
    ide.employee_id,
    iday.day_date,
    COALESCE(ide.planned_hours, 0),
    ide.notes,
    COALESCE(ide.start_time, i.start_time),
    COALESCE(ide.end_time,   i.end_time),
    ide.clock_in,
    ide.clock_out,
    COALESCE(ide.break_minutes, 0),
    ide.actual_hours
FROM inquiry_day_employees ide
JOIN inquiry_days iday ON ide.inquiry_day_id = iday.id
JOIN inquiries i ON iday.inquiry_id = i.id
ON CONFLICT DO NOTHING;

-- ── Rebuild calendar_item_employees rows for multi-day items ─────────────────
DELETE FROM calendar_item_employees cie
WHERE EXISTS (SELECT 1 FROM calendar_item_days WHERE calendar_item_id = cie.calendar_item_id);

INSERT INTO calendar_item_employees (
    id, calendar_item_id, employee_id, job_date,
    planned_hours,
    start_time, end_time, clock_in, clock_out,
    break_minutes, actual_hours
)
SELECT
    gen_random_uuid(),
    cday.calendar_item_id,
    cdde.employee_id,
    cday.day_date,
    COALESCE(cdde.planned_hours, 0),
    COALESCE(cdde.start_time, ci.start_time),
    COALESCE(cdde.end_time,   ci.end_time),
    cdde.clock_in,
    cdde.clock_out,
    COALESCE(cdde.break_minutes, 0),
    cdde.actual_hours
FROM calendar_item_day_employees cdde
JOIN calendar_item_days cday ON cdde.calendar_item_day_id = cday.id
JOIN calendar_items ci ON cday.calendar_item_id = ci.id
ON CONFLICT DO NOTHING;

-- ── Backfill job_date for single-day rows (no day table rows existed) ─────────
UPDATE inquiry_employees ie
SET job_date = COALESCE(
    (SELECT scheduled_date FROM inquiries WHERE id = ie.inquiry_id),
    NOW()::date
)
WHERE job_date IS NULL;

UPDATE calendar_item_employees cie
SET job_date = COALESCE(
    (SELECT scheduled_date FROM calendar_items WHERE id = cie.calendar_item_id),
    NOW()::date
)
WHERE job_date IS NULL;

-- ── Make job_date NOT NULL ────────────────────────────────────────────────────
ALTER TABLE inquiry_employees      ALTER COLUMN job_date SET NOT NULL;
ALTER TABLE calendar_item_employees ALTER COLUMN job_date SET NOT NULL;

-- ── Replace unique constraints to include job_date ────────────────────────────
ALTER TABLE inquiry_employees DROP CONSTRAINT IF EXISTS inquiry_employees_inquiry_id_employee_id_key;
ALTER TABLE inquiry_employees ADD CONSTRAINT inquiry_employees_inquiry_id_employee_id_job_date_key
    UNIQUE (inquiry_id, employee_id, job_date);

ALTER TABLE calendar_item_employees DROP CONSTRAINT IF EXISTS calendar_item_employees_calendar_item_id_employee_id_key;
ALTER TABLE calendar_item_employees ADD CONSTRAINT calendar_item_employees_calendar_item_id_employee_id_job_date_key
    UNIQUE (calendar_item_id, employee_id, job_date);

-- ── Indexes on job_date for range lookups ─────────────────────────────────────
CREATE INDEX IF NOT EXISTS idx_inquiry_employees_job_date     ON inquiry_employees(job_date);
CREATE INDEX IF NOT EXISTS idx_calendar_item_employees_job_date ON calendar_item_employees(job_date);

-- ── Drop the day tables (cascade removes their FK-dependent rows/indexes) ─────
DROP TABLE IF EXISTS inquiry_day_employees;
DROP TABLE IF EXISTS inquiry_days;
DROP TABLE IF EXISTS calendar_item_day_employees;
DROP TABLE IF EXISTS calendar_item_days;
