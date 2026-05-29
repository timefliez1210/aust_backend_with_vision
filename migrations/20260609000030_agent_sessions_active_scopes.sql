-- Persist active entity scopes across session turns for scoped memory retrieval (S4).
--
-- When the assistant tool loop encounters a tool call referencing an inquiry_id,
-- customer_id, or employee_id, the corresponding scope string is appended here.
-- On the next turn, assemble_bundle includes these scopes when fetching agent_memory,
-- so entity-scoped memories (e.g. "inquiry:<uuid>" facts) are included in the context.
ALTER TABLE agent_sessions
    ADD COLUMN IF NOT EXISTS active_scopes JSONB NOT NULL DEFAULT '["global"]'::jsonb;
