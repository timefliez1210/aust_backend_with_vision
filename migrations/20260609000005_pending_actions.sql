-- Confirmation queue for Write/Confirm-safety tools.
--
-- When the assistant proposes a destructive or high-impact action, a pending_action
-- row is inserted and the Telegram message ID is stored so the bot can edit the
-- original inline-keyboard message when Alex confirms, edits, or cancels.
--
-- `status`: pending | confirmed | edited | canceled | expired

CREATE TABLE pending_actions (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id          UUID NOT NULL REFERENCES agent_sessions(id),
    tool_name           TEXT NOT NULL,
    -- Arguments as the LLM proposed them.
    proposed_args       JSONB NOT NULL,
    -- Arguments that will actually be used (may differ if Alex edited the request).
    final_args          JSONB,
    status              TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'confirmed', 'edited', 'canceled', 'expired')),
    -- Telegram message ID so the bot can edit the keyboard message on resolution.
    telegram_message_id BIGINT,
    -- When the pending action expires if Alex does not respond.
    expires_at          TIMESTAMPTZ NOT NULL DEFAULT (NOW() + INTERVAL '1 hour'),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at         TIMESTAMPTZ
);

CREATE INDEX idx_pending_actions_session_id ON pending_actions(session_id);
CREATE INDEX idx_pending_actions_status     ON pending_actions(status)
    WHERE status = 'pending';
CREATE INDEX idx_pending_actions_expires_at ON pending_actions(expires_at)
    WHERE status = 'pending';
