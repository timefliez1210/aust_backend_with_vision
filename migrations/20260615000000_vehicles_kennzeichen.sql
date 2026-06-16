-- Add the permanent license plate (Kennzeichen) to vehicles.
--
-- Separate from the free-form `label` (e.g. "Mercedes Sprinter"): the plate is
-- the official identifier and stays with the vehicle. NOT NULL with a '' default
-- so the additive migration is safe against any pre-existing rows; the API
-- requires a non-empty value on create.
ALTER TABLE vehicles ADD COLUMN IF NOT EXISTS kennzeichen TEXT NOT NULL DEFAULT '';
