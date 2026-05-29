-- Database role for the assistant crate.
--
-- PURPOSE:
--   The aust_assistant role is used by the crates/assistant process to interact
--   with the database. It has carefully scoped access:
--     - READ (SELECT) on all tables it needs to answer questions.
--     - WRITE (INSERT, UPDATE) only on assistant-owned tables.
--     - NO DELETE on any table — the assistant must never destroy data.
--     - NO ACCESS to authentication / OTP / customer-session tables (privacy boundary).
--     - NO schema-modification privileges (no CREATE, ALTER, DROP).
--
--   This follows the principle of least privilege: if the assistant process is
--   compromised, an attacker cannot exfiltrate auth secrets or permanently destroy
--   business data.
--
-- USAGE:
--   In production, create the actual PG user separately and assign this role:
--     CREATE USER aust_assistant_user WITH PASSWORD '...';
--     GRANT aust_assistant TO aust_assistant_user;

-- Create the role (idempotent via DO block).
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'aust_assistant') THEN
        CREATE ROLE aust_assistant;
    END IF;
END
$$;

-- ── READ access (SELECT) ─────────────────────────────────────────────────────

-- Core business tables — read-only.
GRANT SELECT ON inquiries           TO aust_assistant;
GRANT SELECT ON customers           TO aust_assistant;
GRANT SELECT ON offers              TO aust_assistant;
GRANT SELECT ON addresses           TO aust_assistant;
GRANT SELECT ON calendar_items             TO aust_assistant;
GRANT SELECT ON calendar_item_employees    TO aust_assistant;
GRANT SELECT ON calendar_capacity_overrides TO aust_assistant;
GRANT SELECT ON employees                  TO aust_assistant;
GRANT SELECT ON inquiry_employees          TO aust_assistant;
GRANT SELECT ON email_threads              TO aust_assistant;
GRANT SELECT ON email_messages             TO aust_assistant;
GRANT SELECT ON volume_estimations         TO aust_assistant;
GRANT SELECT ON invoices                   TO aust_assistant;
GRANT SELECT ON invoice_reminders          TO aust_assistant;
GRANT SELECT ON settings                   TO aust_assistant;
GRANT SELECT ON notes                      TO aust_assistant;
GRANT SELECT ON feedback_reports           TO aust_assistant;
GRANT SELECT ON review_requests            TO aust_assistant;
GRANT SELECT ON flash_contacts             TO aust_assistant;

-- ── WRITE access (INSERT + UPDATE) on assistant-owned tables ─────────────────

GRANT SELECT, INSERT, UPDATE ON agent_sessions         TO aust_assistant;
GRANT SELECT, INSERT, UPDATE ON agent_memory           TO aust_assistant;
GRANT SELECT, INSERT         ON agent_episodes         TO aust_assistant;
GRANT SELECT, INSERT         ON agent_actions          TO aust_assistant;
GRANT SELECT, INSERT, UPDATE ON pending_actions        TO aust_assistant;
GRANT SELECT, INSERT, UPDATE ON telegram_chat_bindings TO aust_assistant;
GRANT SELECT, INSERT, UPDATE ON offer_observations     TO aust_assistant;

-- ── EXPLICITLY DENIED tables (authentication / privacy boundary) ─────────────
-- No GRANT = no access by default in Postgres; listing here for documentation.
--
-- DENIED (not granted):
--   users              — admin credentials
--   admin_password_resets
--   customer_otps      — one-time passwords
--   customer_sessions  — customer auth tokens
--   employee_otps
--   employee_sessions
--
-- Assistant tables use UUID v7 primary keys (no sequences), so no
-- GRANT USAGE ON SEQUENCE is required.
