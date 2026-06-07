-- The offers table never had an updated_at column, but three code paths write it:
--   offer_service_impl.rs accept/reject (UPDATE offers SET status=..., updated_at=NOW())
--   offer_builder.rs supersede-on-regenerate
-- Without the column these UPDATEs fail with
--   `column "updated_at" of relation "offers" does not exist`,
-- which broke the assistant's "set offer to accepted" flow (2026-06-07).
--
-- Additive only: DEFAULT NOW() backfills existing rows, NOT NULL stays satisfied.
ALTER TABLE offers
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();
