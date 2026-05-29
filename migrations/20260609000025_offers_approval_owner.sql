-- Record which path handled approval at draft time.
--
-- 'legacy'  → offer_pipeline.rs posted the Telegram message immediately.
-- 'agent'   → the assistant event consumer (handle_offer_drafted) will post it.
-- NULL      → pre-migration offers; consumers treat these the same as 'legacy'.
ALTER TABLE offers
    ADD COLUMN IF NOT EXISTS approval_owner TEXT
        CHECK (approval_owner IN ('legacy', 'agent'));
