-- Gross hours between two clock times, correct across midnight.
--
-- clock_in and clock_out are TIME (migration 20260413120000). In Postgres,
-- TIME - TIME is an interval that goes NEGATIVE when the shift crosses midnight,
-- so a 22:00-06:00 Nachtumzug with a 30-minute break stored actual_hours = -16.5
-- and that figure flowed straight into the timesheets and the payroll sums.
--
-- Twenty-four inline copies of the same EXTRACT expression had the bug, which is
-- why the arithmetic lives here now instead of in each query. Callers keep doing
-- their own break subtraction and rounding; this only makes the span itself right.
--
-- Returns numeric, exactly like the EXTRACT it replaces, so no call site changes
-- type. NULL in, NULL out.

CREATE OR REPLACE FUNCTION aust_shift_hours(p_in TIME, p_out TIME)
RETURNS numeric
LANGUAGE sql
IMMUTABLE
AS $$
    SELECT EXTRACT(EPOCH FROM (
        p_out - p_in
        + CASE WHEN p_out < p_in THEN INTERVAL '24 hours' ELSE INTERVAL '0 hours' END
    )) / 3600.0
$$;

COMMENT ON FUNCTION aust_shift_hours(TIME, TIME) IS
'Gross hours from p_in to p_out, adding 24h when the shift crosses midnight. Break deduction and rounding stay with the caller.';
