-- Add the `agent_owns_approval` setting with default false.
--
-- When this flag is true (set via the admin settings panel), the Telegram
-- approval message post for auto-generated offers should be skipped; the
-- assistant's event consumer (Wave 2 / Phase 3) handles approval routing instead.
--
-- Default is false so the existing approval flow is unchanged until Alex
-- explicitly enables the new agent-driven path.
--
-- NOTE: this migration's date prefix sorts BEFORE the migration that creates
-- the settings table (20260607000000_settings.sql). On a fresh DB the table
-- does not exist yet at this point. Guard the insert so the migration sequence
-- works in both fresh and incremental scenarios.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.tables
        WHERE table_name = 'settings'
    ) THEN
        INSERT INTO settings (key, value, updated_at)
        VALUES ('agent_owns_approval', 'false'::jsonb, NOW())
        ON CONFLICT (key) DO NOTHING;
    END IF;
END$$;
