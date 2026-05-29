-- Pending memory proposals queue.
--
-- Holds memory candidates produced by the post-action reflection hook and the
-- nightly consolidation pass when their confidence is below the auto-store
-- threshold (0.7 for reflection, 0.8 for consolidation). Alex reviews and
-- approves / rejects in batches via Telegram (out of scope for this migration).
--
-- Append-only semantics: rows are never DELETEd. Status transitions:
--   pending -> approved (also inserts a row into agent_memory)
--   pending -> rejected (terminal)
--   pending -> superseded (when a newer proposal makes this one stale)

CREATE TABLE pending_memory_proposals (
    id              UUID PRIMARY KEY,
    session_id      UUID REFERENCES agent_sessions(id) ON DELETE SET NULL,
    proposed_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    kind            TEXT NOT NULL CHECK (kind IN ('preference', 'fact', 'rule', 'pattern')),
    scope           TEXT NOT NULL,
    key             TEXT NOT NULL,
    value           JSONB NOT NULL,
    confidence      REAL NOT NULL,
    source_episodes UUID[] NOT NULL DEFAULT '{}',
    rationale       TEXT,
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'approved', 'rejected', 'superseded')),
    resolved_at     TIMESTAMPTZ,
    resolved_by     UUID,
    resolution_note TEXT
);

-- Fast lookup of unresolved proposals (the typical "what needs approval" query).
CREATE INDEX idx_pmp_status ON pending_memory_proposals(status) WHERE status = 'pending';

-- Group proposals by the session that produced them.
CREATE INDEX idx_pmp_session ON pending_memory_proposals(session_id);
