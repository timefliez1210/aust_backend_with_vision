-- agent_briefing_log: allow the evening preview and the nightly reflection slots.
--
-- The 21:00 "Vorschau auf morgen" (commit 6c91e9f) claims slot 'evening', but the
-- CHECK only ever allowed 'morning' and 'afternoon': every claim failed with a
-- check violation, so the preview was never sent (prod logs, 2026-10-13 21:00+).
-- 'reflection' is the once-a-night slot for Josie's learning pass.
-- Widening only — every existing row satisfies the new constraint.

ALTER TABLE agent_briefing_log DROP CONSTRAINT IF EXISTS agent_briefing_log_slot_check;
ALTER TABLE agent_briefing_log
    ADD CONSTRAINT agent_briefing_log_slot_check
    CHECK (slot IN ('morning', 'afternoon', 'evening', 'reflection'));
