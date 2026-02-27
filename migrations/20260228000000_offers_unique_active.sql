-- Prevent duplicate active offers for the same quote.
-- Allows multiple rejected offers (re-generation after rejection is valid).
CREATE UNIQUE INDEX offers_quote_active_unique
    ON offers(quote_id)
    WHERE status NOT IN ('rejected', 'cancelled');
