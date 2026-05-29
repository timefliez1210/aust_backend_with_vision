-- Allow 'superseded' as an offer status so that commit_offer_draft can transition
-- the previous active draft before inserting a new one without violating the
-- offers_inquiry_active_unique partial index.
--
-- The partial index excludes status IN ('rejected','cancelled') from uniqueness;
-- we need 'superseded' excluded too. Recreate the index to include it.

DROP INDEX IF EXISTS offers_inquiry_active_unique;

CREATE UNIQUE INDEX offers_inquiry_active_unique
    ON offers(inquiry_id)
    WHERE status NOT IN ('rejected', 'cancelled', 'superseded');
