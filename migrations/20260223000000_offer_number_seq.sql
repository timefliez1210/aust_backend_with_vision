-- Add a proper sequential offer number using a PostgreSQL sequence.
-- Format: "YYYY-NNNN" where NNNN is a zero-padded sequential counter.

CREATE SEQUENCE offer_number_seq START WITH 1001;

-- Backfill existing offers with unique sequential numbers based on creation order.
WITH numbered AS (
    SELECT id, created_at,
           nextval('offer_number_seq') AS seq_num
    FROM offers
    ORDER BY created_at ASC
)
UPDATE offers
SET offer_number = TO_CHAR(numbered.created_at, 'YYYY') || '-' || LPAD(numbered.seq_num::text, 4, '0')
FROM numbered
WHERE offers.id = numbered.id;
