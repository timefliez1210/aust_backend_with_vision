-- Per-Telegram-chat agent sessions with rolling context and summarization.
--
-- One row per Telegram chat_id. Turns are stored as a JSONB array of
-- {role, content, ts} objects. When the turn count exceeds the token budget
-- the background summarizer collapses old turns into `last_summary` and resets
-- `turns` to only recent turns.

CREATE TABLE agent_sessions (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    chat_id         BIGINT NOT NULL UNIQUE,
    -- Latest rolling summary from the cheap LLM tier (null until first summarization pass).
    last_summary    TEXT,
    -- Recent turns that have not yet been summarized, ordered oldest-first.
    turns           JSONB NOT NULL DEFAULT '[]',
    -- Total number of turns ever appended (for statistics / overflow detection).
    turn_count      INTEGER NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_agent_sessions_chat_id ON agent_sessions(chat_id);
