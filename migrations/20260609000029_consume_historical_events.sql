-- Pre-activation flood guard.
--
-- When agent_owns_approval is first flipped to true the event consumer would
-- process all historical unconsumed domain_events and fire Telegram notifications
-- for ancient offers. Mark them all consumed now so only forward-looking events
-- get processed by the assistant consumer.
UPDATE domain_events
SET consumed_by = consumed_by || jsonb_build_object('assistant', now()::text)
WHERE created_at < now()
  AND NOT (consumed_by ? 'assistant');
