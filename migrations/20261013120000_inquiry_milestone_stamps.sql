-- Conversion tab: record WHEN an inquiry was sent a KVA and when it was confirmed.
--
-- `inquiries.offer_sent_at` and `accepted_at` existed but almost no code path filled
-- them (1 of 117 won jobs had accepted_at): Alex moves jobs along by changing the
-- status, and that write never touched the timestamps. The Conversion tab needs them
-- for "Tage bis KVA" / "Tage bis Zusage".
--
-- Additive only:
-- 1. backfill from the status.changed event log (exists since 2026-09-14),
-- 2. a trigger that stamps the first time a status is reached, on every write path
--    (dashboard, Josie, mail bridge) — never overwriting an existing stamp.

SELECT set_config('app.tenant_bypass', 'on', true);

WITH events AS (
    SELECT (payload->>'inquiry_id')::uuid AS inquiry_id,
           payload->>'new_status' AS new_status,
           created_at
    FROM domain_events
    WHERE kind = 'status.changed'
    UNION ALL
    SELECT (payload->>'inquiry_id')::uuid, payload->>'new_status', created_at
    FROM domain_events_archive
    WHERE kind = 'status.changed'
),
firsts AS (
    SELECT inquiry_id,
           MIN(created_at) FILTER (WHERE new_status = 'offer_sent') AS sent_at,
           MIN(created_at) FILTER (WHERE new_status IN ('accepted', 'scheduled')) AS accepted_at
    FROM events
    GROUP BY inquiry_id
)
UPDATE inquiries q
SET offer_sent_at = COALESCE(q.offer_sent_at, f.sent_at),
    accepted_at   = COALESCE(q.accepted_at, f.accepted_at)
FROM firsts f
WHERE f.inquiry_id = q.id
  AND ((q.offer_sent_at IS NULL AND f.sent_at IS NOT NULL)
    OR (q.accepted_at IS NULL AND f.accepted_at IS NOT NULL));

CREATE FUNCTION stamp_inquiry_milestones() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.status IS DISTINCT FROM OLD.status THEN
        IF NEW.status = 'offer_sent' AND NEW.offer_sent_at IS NULL THEN
            NEW.offer_sent_at := NOW();
        END IF;
        -- Only a real Zusage: a jump straight to paid/invoiced (bulk imports, repairs)
        -- would date the confirmation weeks too late.
        IF NEW.status IN ('accepted', 'scheduled') AND NEW.accepted_at IS NULL THEN
            NEW.accepted_at := NOW();
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER inquiries_stamp_milestones
BEFORE UPDATE OF status ON inquiries
FOR EACH ROW EXECUTE FUNCTION stamp_inquiry_milestones();
