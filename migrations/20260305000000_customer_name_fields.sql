-- Add structured name fields to customers.
-- salutation: "Herr", "Frau", "D" (divers) — explicit, never guessed.
-- first_name / last_name: split for proper greeting generation.
-- The existing `name` column is kept for display and backwards compat.

ALTER TABLE customers
    ADD COLUMN IF NOT EXISTS salutation  TEXT,
    ADD COLUMN IF NOT EXISTS first_name  TEXT,
    ADD COLUMN IF NOT EXISTS last_name   TEXT;

-- Back-fill first_name / last_name from existing name where possible.
-- Splits on first space: everything before is first_name, rest is last_name.
UPDATE customers
SET
    first_name = CASE WHEN name LIKE '% %' THEN split_part(name, ' ', 1) ELSE name END,
    last_name  = CASE WHEN name LIKE '% %' THEN TRIM(SUBSTR(name, STRPOS(name, ' ') + 1)) ELSE NULL END
WHERE name IS NOT NULL AND first_name IS NULL;
