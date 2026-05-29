-- Telegram chat ID → user mapping with role assignment.
--
-- Every Telegram chat that interacts with the assistant must have a binding.
-- Unbound chats are rejected immediately by the assistant driver. The `role`
-- field determines which tools are available in that session.
--
-- `role`: owner | operator

CREATE TABLE telegram_chat_bindings (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    chat_id     BIGINT NOT NULL UNIQUE,
    -- Internal user identifier (links to admin_users or employees table).
    user_id     UUID NOT NULL,
    role        TEXT NOT NULL CHECK (role IN ('owner', 'operator')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- chat_id is the sole lookup key; the UNIQUE constraint already creates an index.
CREATE INDEX idx_telegram_chat_bindings_user_id ON telegram_chat_bindings(user_id);
