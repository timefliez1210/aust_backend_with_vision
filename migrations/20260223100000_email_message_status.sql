-- Add status column to email_messages for draft tracking
ALTER TABLE email_messages ADD COLUMN status VARCHAR(20) NOT NULL DEFAULT 'sent';

-- Existing messages are all 'sent', new LLM drafts will be 'draft'
CREATE INDEX idx_email_messages_status ON email_messages(status);
