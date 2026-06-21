-- Worker self-reported break minutes, mirroring employee_clock_in/out (migration 20260322).
-- Purely informational: the admin's authoritative `break_minutes` is untouched. The worker
-- reports their own break in the portal; the admin dashboard shows it read-only.
ALTER TABLE inquiry_employees
    ADD COLUMN IF NOT EXISTS employee_break_minutes INTEGER;

ALTER TABLE calendar_item_employees
    ADD COLUMN IF NOT EXISTS employee_break_minutes INTEGER;
