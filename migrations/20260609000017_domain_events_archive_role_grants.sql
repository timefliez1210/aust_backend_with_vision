-- Grant aust_assistant access to the archive table.
-- No DELETE — the archive is final.
GRANT SELECT, INSERT, UPDATE ON domain_events_archive TO aust_assistant;
