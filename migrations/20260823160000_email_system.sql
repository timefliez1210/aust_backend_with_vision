-- Turn the email tables from an inquiry-ingest side effect into a real mailbox.
--
-- Three problems this addresses:
--   1. Inbound mail was only persisted if the inquiry parser could resolve a
--      customer. When it could not, `find_or_create_thread` returned NULL and the
--      message was dropped -- while IMAP \Seen was set anyway, so it also vanished
--      from the UNSEEN search. Threads therefore have to be able to exist without
--      a customer.
--   2. There was no read/handled state at all, so the "unanswered email" nag in
--      assistant/hooks/reminders.rs filtered on `status = 'received'` -- a value
--      nothing ever wrote (inbound inserts omit `status` and take the 'sent'
--      default). It has never fired once in production.
--   3. Inbound threading ignored In-Reply-To/References entirely and matched on
--      customer + a 30-day window.

-- 1. Threads without a known customer ------------------------------------------------
ALTER TABLE email_threads ALTER COLUMN customer_id DROP NOT NULL;

-- The counterparty address, for threads we cannot (yet) attach to a customer.
-- Readers use COALESCE(c.email, et.contact_address).
ALTER TABLE email_threads ADD COLUMN IF NOT EXISTS contact_address VARCHAR(255);

-- Per-thread mute: suppresses the Telegram nag without marking mail handled.
ALTER TABLE email_threads ADD COLUMN IF NOT EXISTS muted BOOLEAN NOT NULL DEFAULT FALSE;

-- 2. Read / handled state -------------------------------------------------------------
ALTER TABLE email_messages ADD COLUMN IF NOT EXISTS read_at TIMESTAMPTZ;
ALTER TABLE email_messages ADD COLUMN IF NOT EXISTS handled_at TIMESTAMPTZ;

-- 3. RFC threading headers ------------------------------------------------------------
ALTER TABLE email_messages ADD COLUMN IF NOT EXISTS in_reply_to VARCHAR(998);
ALTER TABLE email_messages ADD COLUMN IF NOT EXISTS reference_ids TEXT[] NOT NULL DEFAULT '{}';

-- Backfill ----------------------------------------------------------------------------
-- Every historical inbound row took the 'sent' column default; 'received' is what the
-- code always meant.
UPDATE email_messages SET status = 'received'
WHERE direction = 'inbound' AND status = 'sent';

-- Historical inbound mail predates read/handled tracking. Backfill it as both read and
-- handled: switching the nag on with 81 production rows suddenly "unhandled" would fire
-- 81 Telegram reminders at once. Only mail arriving from here on counts as new.
UPDATE email_messages SET read_at = created_at, handled_at = created_at
WHERE direction = 'inbound' AND handled_at IS NULL;

-- Indexes -----------------------------------------------------------------------------
-- Thread lookup by RFC Message-ID (In-Reply-To / References resolution).
CREATE INDEX IF NOT EXISTS idx_email_messages_message_id
    ON email_messages (message_id) WHERE message_id IS NOT NULL;

-- Drives both the unread badge and the reminder reconciliation.
CREATE INDEX IF NOT EXISTS idx_email_messages_unhandled
    ON email_messages (created_at) WHERE direction = 'inbound' AND handled_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_email_messages_thread_created
    ON email_messages (thread_id, created_at);

-- 4. Real outbound composition ---------------------------------------------------------
-- Sending was to-address-only, plain text, with the offer PDF as the sole possible
-- attachment (hardcoded). Threads with several participants had no way to keep everyone
-- on the conversation.
ALTER TABLE email_messages ADD COLUMN IF NOT EXISTS cc_addresses TEXT[] NOT NULL DEFAULT '{}';
ALTER TABLE email_messages ADD COLUMN IF NOT EXISTS bcc_addresses TEXT[] NOT NULL DEFAULT '{}';

-- Attachment display names, positionally paired with `attachment_keys`. Inbound rows
-- kept only the S3 key, whose basename is "{idx}.{ext}" -- the sender's filename was
-- discarded. Left empty for existing rows; readers fall back to the key's basename.
ALTER TABLE email_messages ADD COLUMN IF NOT EXISTS attachment_names TEXT[] NOT NULL DEFAULT '{}';
