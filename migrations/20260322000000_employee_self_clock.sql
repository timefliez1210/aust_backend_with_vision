-- Separate employee self-reported clock times from admin-set clock times.
-- Admin sets clock_in/clock_out (existing). Employee reports their own via worker portal.
-- Admin interface shows both for discrepancy checking.

ALTER TABLE inquiry_employees
    ADD COLUMN employee_clock_in  TIMESTAMPTZ,
    ADD COLUMN employee_clock_out TIMESTAMPTZ;

ALTER TABLE calendar_item_employees
    ADD COLUMN employee_clock_in  TIMESTAMPTZ,
    ADD COLUMN employee_clock_out TIMESTAMPTZ;
