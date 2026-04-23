-- Add Verpflegungspauschale (travel daily allowance) support for multi-day appointments.
--
-- When has_pauschale = true on an inquiry/calendar_item, each assigned employee
-- can have a travel-expense (Reisekostenabrechnung) document generated.
--
-- German daily allowance rates (2024):
--   Small (klein)  = 14 €  — first/last day of trip (unless >8h worked)
--   Large (groß)   = 28 €  — full days in between

-- ── Parent-level toggle ───────────────────────────────────────────────────────
ALTER TABLE inquiries      ADD COLUMN IF NOT EXISTS has_pauschale BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE calendar_items ADD COLUMN IF NOT EXISTS has_pauschale BOOLEAN NOT NULL DEFAULT FALSE;

-- ── Per-employee travel-expense fields ─────────────────────────────────────
ALTER TABLE inquiry_employees
    ADD COLUMN IF NOT EXISTS transport_mode      VARCHAR(50)  DEFAULT NULL,
    ADD COLUMN IF NOT EXISTS travel_costs_cents  BIGINT       DEFAULT NULL,
    ADD COLUMN IF NOT EXISTS accommodation_cents BIGINT       DEFAULT NULL,
    ADD COLUMN IF NOT EXISTS meal_deduction      VARCHAR(50)  DEFAULT NULL;
    -- meal_deduction values: 'none', 'breakfast', 'lunch', 'dinner',
    --                        'breakfast_lunch', 'breakfast_dinner',
    --                        'lunch_dinner', 'all'

ALTER TABLE calendar_item_employees
    ADD COLUMN IF NOT EXISTS transport_mode      VARCHAR(50)  DEFAULT NULL,
    ADD COLUMN IF NOT EXISTS travel_costs_cents  BIGINT       DEFAULT NULL,
    ADD COLUMN IF NOT EXISTS accommodation_cents BIGINT       DEFAULT NULL,
    ADD COLUMN IF NOT EXISTS meal_deduction      VARCHAR(50)  DEFAULT NULL;
