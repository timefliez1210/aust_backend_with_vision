-- Add pricing metadata to offers so admin dashboard can display/edit them
ALTER TABLE offers ADD COLUMN offer_number TEXT;
ALTER TABLE offers ADD COLUMN persons INTEGER;
ALTER TABLE offers ADD COLUMN hours_estimated DOUBLE PRECISION;
ALTER TABLE offers ADD COLUMN rate_per_hour_cents BIGINT;
ALTER TABLE offers ADD COLUMN line_items_json JSONB;
