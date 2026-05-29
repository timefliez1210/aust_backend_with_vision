-- Backfill end_date for existing rows where it was not set on creation.
-- Phase 1: make all appointments single-day by default (end_date = scheduled_date).
--
-- NOTE: this migration's date prefix is earlier than the migration that creates
-- the end_date column (20260601000000_simplify_scheduling.sql) on fresh DBs.
-- Existing dev/prod DBs had this column added via earlier ad-hoc DDL when this
-- ran. Guard the UPDATEs so the migration sequence works in both scenarios; the
-- actual backfill is also performed defensively in 20260601000000_simplify_scheduling.sql.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'inquiries' AND column_name = 'end_date'
    ) THEN
        UPDATE inquiries SET end_date = scheduled_date
            WHERE end_date IS NULL AND scheduled_date IS NOT NULL;
    END IF;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'calendar_items' AND column_name = 'end_date'
    ) THEN
        UPDATE calendar_items SET end_date = scheduled_date
            WHERE end_date IS NULL AND scheduled_date IS NOT NULL;
    END IF;
END$$;
