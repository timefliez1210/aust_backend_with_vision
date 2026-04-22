-- Rename legacy status value 'sent' → 'offer_sent' in inquiries table.
-- 'sent' was the old value before the status enum was standardised.
UPDATE inquiries SET status = 'offer_sent' WHERE status = 'sent';
