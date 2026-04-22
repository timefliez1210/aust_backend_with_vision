-- Backfill clock_in/clock_out from planned start_time/end_time for rows where
-- the admin set planned times but actual clock times were never recorded separately.
-- Then compute actual_hours from the now-populated clock times minus break.

UPDATE inquiry_employees
SET clock_in  = start_time,
    clock_out = end_time
WHERE clock_in IS NULL
  AND start_time IS NOT NULL
  AND end_time   IS NOT NULL;

UPDATE inquiry_employees
SET actual_hours = GREATEST(0,
    EXTRACT(EPOCH FROM (clock_out - clock_in)) / 3600.0
    - COALESCE(break_minutes, 0) / 60.0
)
WHERE actual_hours IS NULL
  AND clock_in  IS NOT NULL
  AND clock_out IS NOT NULL;

UPDATE calendar_item_employees
SET clock_in  = start_time,
    clock_out = end_time
WHERE clock_in IS NULL
  AND start_time IS NOT NULL
  AND end_time   IS NOT NULL;

UPDATE calendar_item_employees
SET actual_hours = GREATEST(0,
    EXTRACT(EPOCH FROM (clock_out - clock_in)) / 3600.0
    - COALESCE(break_minutes, 0) / 60.0
)
WHERE actual_hours IS NULL
  AND clock_in  IS NOT NULL
  AND clock_out IS NOT NULL;
