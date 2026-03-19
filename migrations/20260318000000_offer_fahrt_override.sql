-- Add persisted Fahrkostenpauschale override to offers.
-- When non-NULL, subsequent regenerations use this value instead of recalculating via ORS.
ALTER TABLE offers ADD COLUMN fahrt_override_cents INTEGER;
