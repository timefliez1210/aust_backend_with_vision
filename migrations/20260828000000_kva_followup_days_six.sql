-- Lower the default KVA follow-up threshold from 21 to 6 days (Alex, 2026-08-28).
--
-- 21 was the observed median time from KVA to decision, which turned out to be the
-- wrong thing to wait for: by the time the median has passed the customer has usually
-- already booked someone else. Chasing after 6 days still lands inside the decision
-- window.
--
-- Only touches the value seeded by 20260823140000. A threshold somebody has since
-- changed by hand is left alone.
UPDATE settings
   SET value = '6'::jsonb, updated_at = NOW()
 WHERE key = 'kva_followup_days'
   AND value = '21'::jsonb;
