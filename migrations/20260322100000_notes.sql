-- General-purpose admin notepad
CREATE TABLE notes (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title       VARCHAR(255) NOT NULL DEFAULT '',
    content     TEXT NOT NULL DEFAULT '',
    color       VARCHAR(20) NOT NULL DEFAULT 'default',
    pinned      BOOLEAN NOT NULL DEFAULT FALSE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TRIGGER update_notes_updated_at
    BEFORE UPDATE ON notes FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

CREATE INDEX idx_notes_pinned_created ON notes(pinned DESC, created_at DESC);
