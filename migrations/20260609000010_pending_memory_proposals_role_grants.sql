-- Role grants for `pending_memory_proposals`.
--
-- The aust_assistant role needs SELECT (to surface pending proposals in the
-- nightly briefing), INSERT (post-action hook + consolidation hook enqueue),
-- and UPDATE (resolve via approve/reject). No DELETE — append-only semantics.

GRANT SELECT, INSERT, UPDATE ON pending_memory_proposals TO aust_assistant;
