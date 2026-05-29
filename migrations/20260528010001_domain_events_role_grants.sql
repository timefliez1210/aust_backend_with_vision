-- Grant SELECT, INSERT, UPDATE on domain_events to the aust_assistant role.
--
-- INSERT: the assistant consumer marks events consumed (UPDATE) and may also
--         emit its own events (INSERT). The API layer also inserts events.
-- SELECT: the event consumer loop reads pending events.
-- UPDATE: mark_consumed updates the consumed_by JSONB column.
-- No DELETE: events are append-only; deletion is never needed from either role.

GRANT SELECT, INSERT, UPDATE ON domain_events TO aust_assistant;
