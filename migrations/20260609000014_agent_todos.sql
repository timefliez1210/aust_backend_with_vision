-- Agent to-do items created via the assistant `create_todo` tool.
-- Stored per-session so each chat thread has its own working list.
CREATE TABLE IF NOT EXISTS agent_todos (
    id          UUID        PRIMARY KEY,
    session_id  UUID        NOT NULL,
    text        TEXT        NOT NULL,
    due         DATE,
    status      TEXT        NOT NULL DEFAULT 'open'
                            CHECK (status IN ('open', 'resolved', 'cancelled')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_agent_todos_session   ON agent_todos(session_id);
CREATE INDEX IF NOT EXISTS idx_agent_todos_open      ON agent_todos(status) WHERE status = 'open';
CREATE INDEX IF NOT EXISTS idx_agent_todos_due       ON agent_todos(due) WHERE due IS NOT NULL;
