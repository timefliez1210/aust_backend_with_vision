-- Guard against silent row-vanishing: generate_series(scheduled_date, end_date) on the
-- calendar fetch path produces an empty set when end_date < scheduled_date, which silently
-- drops items from the schedule. A drag-induced inversion already had a frontend fix, but
-- any other path that writes a bad end_date would re-introduce the bug. Fail loud at the
-- DB layer instead.

ALTER TABLE calendar_items
  ADD CONSTRAINT calendar_items_end_after_start
  CHECK (end_date IS NULL OR end_date >= scheduled_date) NOT VALID;

ALTER TABLE calendar_items VALIDATE CONSTRAINT calendar_items_end_after_start;

ALTER TABLE inquiries
  ADD CONSTRAINT inquiries_end_after_start
  CHECK (end_date IS NULL OR end_date >= scheduled_date) NOT VALID;

ALTER TABLE inquiries VALIDATE CONSTRAINT inquiries_end_after_start;
