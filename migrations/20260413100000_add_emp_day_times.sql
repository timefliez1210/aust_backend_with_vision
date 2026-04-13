-- Add per-employee planned start/end times for each day in multi-day appointments.
-- Distinct from clock_in/clock_out (actual timestamps); these are planned times (TIME only).

ALTER TABLE inquiry_day_employees
    ADD COLUMN IF NOT EXISTS start_time TIME,
    ADD COLUMN IF NOT EXISTS end_time   TIME;

ALTER TABLE calendar_item_day_employees
    ADD COLUMN IF NOT EXISTS start_time TIME,
    ADD COLUMN IF NOT EXISTS end_time   TIME;
