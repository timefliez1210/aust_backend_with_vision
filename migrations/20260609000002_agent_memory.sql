-- Durable structured memory for the assistant.
--
-- Append-only: facts are never DELETEd. To update a fact, insert a new row and
-- set `superseded_by` on the old row. To retire a fact without replacement, set
-- `retired_at`.  Both the old and new rows remain visible for audit purposes.
--
-- `kind`:   preference | fact | rule | pattern
-- `scope`:  global | customer:<uuid> | employee:<uuid> | inquiry:<uuid>

CREATE TABLE agent_memory (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind            TEXT NOT NULL CHECK (kind IN ('preference', 'fact', 'rule', 'pattern')),
    scope           TEXT NOT NULL DEFAULT 'global',
    key             TEXT NOT NULL,
    value           JSONB NOT NULL,
    -- Where did this memory come from? e.g. "post_action_hook", "user_explicit", "consolidation"
    source          TEXT NOT NULL DEFAULT 'unknown',
    -- 0.0–1.0; memories below 0.7 are held for nightly review before auto-storing.
    confidence      DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    -- UUID of the row that supersedes this one (NULL = still current).
    superseded_by   UUID REFERENCES agent_memory(id),
    -- Set when the fact is withdrawn without replacement.
    retired_at      TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Fast recall by scope + kind (primary access pattern).
CREATE INDEX idx_agent_memory_scope_kind ON agent_memory(scope, kind);
-- Fetch all active (non-superseded, non-retired) memories quickly.
CREATE INDEX idx_agent_memory_active ON agent_memory(scope, kind)
    WHERE superseded_by IS NULL AND retired_at IS NULL;
CREATE INDEX idx_agent_memory_key ON agent_memory(key);
