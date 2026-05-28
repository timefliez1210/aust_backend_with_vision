-- Guard against silent row-vanishing: generate_series(scheduled_date, end_date) on the
-- calendar fetch path produces an empty set when end_date < scheduled_date, which silently
-- drops items from the schedule. Drag-induced inversions had a frontend fix shipped earlier;
-- this migration enforces the invariant at the DB layer so future regressions fail loud.

-- Heal pre-existing inverted rows first (data created before the frontend fix shipped).
-- These rows were the exact ones vanishing from the calendar UI. Clamp end_date up to
-- scheduled_date — treating them as single-day items is safe: the visual span was already
-- broken, and any legitimate multi-day span can be re-set via the side panel.
UPDATE calendar_items SET end_date = scheduled_date WHERE end_date IS NOT NULL AND end_date < scheduled_date;
UPDATE inquiries      SET end_date = scheduled_date WHERE end_date IS NOT NULL AND end_date < scheduled_date;

ALTER TABLE calendar_items
  ADD CONSTRAINT calendar_items_end_after_start
  CHECK (end_date IS NULL OR end_date >= scheduled_date);

ALTER TABLE inquiries
  ADD CONSTRAINT inquiries_end_after_start
  CHECK (end_date IS NULL OR end_date >= scheduled_date);
