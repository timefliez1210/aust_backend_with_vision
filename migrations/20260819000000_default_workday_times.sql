-- Standard-Auftragszeit: 08:00–16:30 statt 09:00–17:00.
--
-- Migration 20260319100000 added these columns as NOT NULL DEFAULT '09:00:00' /
-- '17:00:00'. Because they are NOT NULL, every INSERT that omits them writes a
-- literal 09:00/17:00 into the row — and the COALESCE(start_time, '08:00')
-- fallbacks scattered through the read queries can never fire. An earlier
-- attempt to fix this only touched those fallbacks, which is why it changed
-- nothing (feedback be449a19).
--
-- Only the DEFAULT changes. Existing rows keep their stored times: they are
-- real recorded appointments, and rewriting them would corrupt history.
ALTER TABLE inquiries
    ALTER COLUMN start_time SET DEFAULT '08:00:00',
    ALTER COLUMN end_time   SET DEFAULT '16:30:00';

ALTER TABLE calendar_items
    ALTER COLUMN start_time SET DEFAULT '08:00:00';
