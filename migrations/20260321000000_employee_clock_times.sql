-- Replace actual_hours (manually entered number) with clock_in/clock_out timestamps.
-- Actual hours worked are now derived: EXTRACT(EPOCH FROM (clock_out - clock_in)) / 3600.0
-- planned_hours is kept for pre-job scheduling.

ALTER TABLE inquiry_employees
    DROP COLUMN actual_hours,
    ADD COLUMN clock_in  TIMESTAMPTZ,
    ADD COLUMN clock_out TIMESTAMPTZ;

ALTER TABLE calendar_item_employees
    DROP COLUMN actual_hours,
    ADD COLUMN clock_in  TIMESTAMPTZ,
    ADD COLUMN clock_out TIMESTAMPTZ;
