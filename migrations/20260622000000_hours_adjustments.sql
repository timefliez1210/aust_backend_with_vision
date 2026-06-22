-- Payroll hour adjustments (Stundenkonto / Gehaltsabrechnung).
--
-- Some employees are on a fixed monthly hour base (e.g. 64h) but regularly work
-- 100h+. At month-end Alex reduces the *paid-out* hours on the Stundenzettel and
-- credits the surplus elsewhere (holidays, boni). This table is a SEPARATE
-- override layer: the recorded *worked* hours on inquiry_employees /
-- calendar_item_employees (clock_in/clock_out/break_minutes) are never touched.
--
-- Each row maps 1:1 to an assignment-day, keyed by the source id + job_date,
-- mirroring the frontend's row keys `inq:{id}:{date}` / `ci:{id}:{date}`.
--
-- Effective PAID hours for a day:
--   deactivated                       -> 0
--   paid_clock_in & paid_clock_out    -> derived (minus paid_break_minutes)
--   else                              -> recorded worked hours (no override)
--
-- hour_account (per month) = SUM(worked) - SUM(paid).

CREATE TABLE IF NOT EXISTS hours_adjustments (
    id                 UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    employee_id        UUID        NOT NULL REFERENCES employees(id) ON DELETE CASCADE,
    entry_type         TEXT        NOT NULL CHECK (entry_type IN ('inquiry', 'calendar_item')),
    inquiry_id         UUID        REFERENCES inquiries(id) ON DELETE CASCADE,
    calendar_item_id   UUID        REFERENCES calendar_items(id) ON DELETE CASCADE,
    job_date           DATE        NOT NULL,
    deactivated        BOOLEAN     NOT NULL DEFAULT FALSE,
    -- NULL paid_* => fall back to the recorded worked time/break for that day.
    paid_clock_in      TIME,
    paid_clock_out     TIME,
    paid_break_minutes INTEGER,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Exactly one source FK must be set, matching entry_type.
    CONSTRAINT hours_adjustments_one_source CHECK (
        (entry_type = 'inquiry'       AND inquiry_id IS NOT NULL AND calendar_item_id IS NULL)
        OR
        (entry_type = 'calendar_item' AND calendar_item_id IS NOT NULL AND inquiry_id IS NULL)
    )
);

-- One adjustment per assignment-day, per source kind.
CREATE UNIQUE INDEX IF NOT EXISTS hours_adjustments_inquiry_uniq
    ON hours_adjustments (employee_id, inquiry_id, job_date)
    WHERE inquiry_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS hours_adjustments_item_uniq
    ON hours_adjustments (employee_id, calendar_item_id, job_date)
    WHERE calendar_item_id IS NOT NULL;

-- Fast monthly lookup for one employee.
CREATE INDEX IF NOT EXISTS idx_hours_adjustments_employee_date
    ON hours_adjustments (employee_id, job_date);

CREATE TRIGGER update_hours_adjustments_updated_at
    BEFORE UPDATE ON hours_adjustments FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
