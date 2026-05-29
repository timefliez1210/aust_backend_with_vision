-- Add chat_id to pending_actions so that callback resolution can validate
-- that the Telegram chat resolving the action is the same one that created it.
ALTER TABLE pending_actions
    ADD COLUMN IF NOT EXISTS chat_id BIGINT;
