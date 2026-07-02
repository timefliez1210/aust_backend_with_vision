-- Attachments on inbound/outbound email messages, stored in S3 the same way
-- feedback_reports.attachment_keys works: an ordered array of storage keys.
ALTER TABLE email_messages ADD COLUMN attachment_keys TEXT[] NOT NULL DEFAULT '{}';
