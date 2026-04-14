-- Update calendar_items.category to match business service categories.
-- Replaces the old generic categories (internal, maintenance, training, other)
-- with the categories the business actually uses.
--
-- Old → New mapping:
--   internal      → intern (keep as-is, just German label)
--   maintenance   → intern (no direct equivalent; maintenance becomes internal)
--   training      → intern (no direct equivalent; training becomes internal)
--   other         → entrümpelung (closest match for misc jobs)
--
-- The column was already VARCHAR(50) with DEFAULT 'internal', so we only need
-- to migrate existing data and add a CHECK constraint.

-- Migrate existing rows to new category values
UPDATE calendar_items SET category = 'intern' WHERE category IN ('internal', 'maintenance', 'training');
UPDATE calendar_items SET category = 'entruempelung' WHERE category = 'other';

-- Add CHECK constraint for the new category values
ALTER TABLE calendar_items
    DROP CONSTRAINT IF EXISTS calendar_items_category_check,
    ADD CONSTRAINT calendar_items_category_check
        CHECK (category IN (
            'intern',
            'umzug',
            'entruempelung',
            'montage',
            'streichen',
            'kartons_auslieferung',
            'kartons_abholung'
        ));

-- Update default value
ALTER TABLE calendar_items
    ALTER COLUMN category SET DEFAULT 'intern';