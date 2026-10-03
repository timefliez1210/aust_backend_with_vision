-- Stundensatz-Kalkulation: which cost categories are loaded onto the hourly rate.
-- A category that is billed or covered separately (fuel via the Fahrkostenpauschale)
-- stays out, so the rate doesn't charge the customer for it twice. Editable in the
-- Gewinn tab; Alex's call 2026-10-03: vehicles in, only fuel out.
ALTER TABLE expense_categories
    ADD COLUMN IF NOT EXISTS in_hourly_rate BOOLEAN NOT NULL DEFAULT TRUE;

UPDATE expense_categories SET in_hourly_rate = FALSE WHERE name = 'Kraftstoff';
