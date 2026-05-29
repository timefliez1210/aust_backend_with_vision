-- Audit log: every tool call executed by the assistant is recorded here.
--
-- Immutable append-only. Rows are never updated or deleted. Used for:
-- - Debugging unexpected assistant behaviour
-- - Compliance / data access audit trail
-- - Training data for the offline learning pipeline (Phase 5)

CREATE TABLE agent_actions (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id          UUID NOT NULL REFERENCES agent_sessions(id),
    tool_name           TEXT NOT NULL,
    -- JSON-serialised tool arguments exactly as the LLM produced them.
    args                JSONB NOT NULL,
    -- JSON-serialised tool result (null on error).
    result              JSONB,
    -- Error message when the tool raised an error (null on success).
    error_message       TEXT,
    -- Wall-clock milliseconds from tool dispatch to return.
    duration_ms         INTEGER,
    -- Link to a pending_action row when this tool required confirmation.
    confirmed_action_id UUID,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_agent_actions_session_id ON agent_actions(session_id);
CREATE INDEX idx_agent_actions_tool_name  ON agent_actions(tool_name);
CREATE INDEX idx_agent_actions_created_at ON agent_actions(created_at DESC);
