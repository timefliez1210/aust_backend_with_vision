-- KVA follow-up (Nachfassen) state.
--
-- The KVA-Buch nags Alex about Kostenvoranschläge that have gone quiet, but only
-- when chasing them can still earn money: the KVA must be older than the
-- follow-up threshold AND the move must still lie in the future. A KVA whose
-- Umzugsdatum has passed is dead — pinging about it is pure noise.
--
-- followup_last_pinged_on dedupes the 60-second tick to at most one ping per
-- calendar day (Europe/Berlin) per offer, exactly as vehicle_reminders does.
-- followup_muted lets Alex silence a single KVA without moving the threshold.

ALTER TABLE offers
    ADD COLUMN IF NOT EXISTS followup_last_pinged_on DATE;

ALTER TABLE offers
    ADD COLUMN IF NOT EXISTS followup_muted BOOLEAN NOT NULL DEFAULT FALSE;

-- Fast scan for the fire loop: candidates are never muted and never superseded.
CREATE INDEX IF NOT EXISTS idx_offers_followup_candidates
    ON offers (created_at)
    WHERE NOT followup_muted AND status <> 'superseded';

-- Default follow-up threshold in days. Seeded at 21, the observed median time
-- from KVA to decision on production (2026-08). Editable in the settings page.
INSERT INTO settings (key, value)
VALUES ('kva_followup_days', '21')
ON CONFLICT (key) DO NOTHING;
