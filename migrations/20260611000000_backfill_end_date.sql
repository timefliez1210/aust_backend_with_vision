-- Backfill end_date for existing rows where it was not set on creation.
-- Phase 1: make all appointments single-day by default (end_date = scheduled_date).
UPDATE inquiries SET end_date = scheduled_date WHERE end_date IS NULL AND scheduled_date IS NOT NULL;
UPDATE calendar_items SET end_date = scheduled_date WHERE end_date IS NULL AND scheduled_date IS NOT NULL;
