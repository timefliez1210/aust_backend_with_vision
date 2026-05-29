-- Add response_draft columns to review_requests.
--
-- These allow the assistant to persist a draft reply to a customer review
-- without immediately publishing or sending it.

ALTER TABLE review_requests ADD COLUMN response_draft text;
ALTER TABLE review_requests ADD COLUMN response_draft_updated_at timestamptz;

-- aust_assistant already has SELECT on review_requests (migration 20260609000008).
-- Grant UPDATE so the assistant can persist drafts.
GRANT UPDATE ON review_requests TO aust_assistant;
