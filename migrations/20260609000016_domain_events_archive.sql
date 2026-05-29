-- Archive table for domain events that have been consumed by all known consumers
-- and are older than 30 days. Rows are moved here from `domain_events` by the
-- retention sweeper and are never deleted from this table.

CREATE TABLE domain_events_archive (
    id          uuid PRIMARY KEY,
    kind        text NOT NULL,
    aggregate   text NOT NULL,
    payload     jsonb NOT NULL,
    created_at  timestamptz NOT NULL,
    consumed_by jsonb NOT NULL DEFAULT '{}'::jsonb,
    archived_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX idx_domain_events_archive_kind       ON domain_events_archive(kind);
CREATE INDEX idx_domain_events_archive_created    ON domain_events_archive(created_at);
CREATE INDEX idx_domain_events_archive_archived   ON domain_events_archive(archived_at DESC);
