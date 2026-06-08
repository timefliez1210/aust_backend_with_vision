-- Grant the aust_assistant role read/write access to agent_reminders.
-- Matches pattern of other agent_* role-grant migrations.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'aust_assistant') THEN
        GRANT SELECT, INSERT, UPDATE ON agent_reminders TO aust_assistant;
    END IF;
END $$;
