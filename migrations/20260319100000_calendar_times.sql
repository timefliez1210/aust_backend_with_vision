-- Add start_time and end_time to inquiries.
-- Auftrage default to 09:00–17:00 for all existing and new rows.
ALTER TABLE inquiries
    ADD COLUMN start_time TIME NOT NULL DEFAULT '09:00:00',
    ADD COLUMN end_time   TIME NOT NULL DEFAULT '17:00:00';

-- Add start_time (mandatory, default 09:00 for existing rows) and end_time (optional) to calendar_items.
-- start_time is required for new Termine via the API; existing rows are backfilled to 09:00.
ALTER TABLE calendar_items
    ADD COLUMN start_time TIME NOT NULL DEFAULT '09:00:00',
    ADD COLUMN end_time   TIME;
