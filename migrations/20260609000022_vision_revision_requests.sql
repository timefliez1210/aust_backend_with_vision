-- Vision revision request queue.
--
-- When the assistant calls `request_revision_from_vision`, a row is inserted here.
-- The actual re-run is handled by the vision worker (Phase 5 wiring) which polls
-- this table for rows in 'pending' status.
--
-- Status values:
--   pending    — queued, not yet picked up by the worker
--   processing — worker has started
--   completed  — revision finished; see volume_estimations for results
--   failed     — worker encountered an error (see error_message)

CREATE TABLE vision_revision_requests (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    inquiry_id      uuid        NOT NULL REFERENCES inquiries(id) ON DELETE CASCADE,
    requested_at    timestamptz NOT NULL DEFAULT now(),
    status          text        NOT NULL DEFAULT 'pending'
                                CHECK (status IN ('pending', 'processing', 'completed', 'failed')),
    error_message   text,
    completed_at    timestamptz
);

CREATE INDEX idx_vision_revision_inquiry ON vision_revision_requests(inquiry_id);
CREATE INDEX idx_vision_revision_pending  ON vision_revision_requests(status)
    WHERE status = 'pending';

GRANT SELECT, INSERT, UPDATE ON vision_revision_requests TO aust_assistant;
