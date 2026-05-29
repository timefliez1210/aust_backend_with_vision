# Assistant DB Privilege Boundary — Accepted Risk (M4, 2026-05-29)

Migration `20260609000008_aust_assistant_db_role.sql` originally created a
least-privilege `aust_assistant` role, and `20260609000027_drop_aust_assistant_role.sql`
later revokes all of its grants. The role survives but has no privileges.

The assistant subsystem therefore runs against the **single API connection pool**,
which has full CRUD on every business table plus read access to `users`,
`customer_otps`, and `customer_sessions`.

The assistant ingests LLM-shaped output derived from attacker-controllable
email and inquiry content. A successful prompt-injection could in principle
reach those tables. The "least-privilege DB role" boundary is **aspirational,
not enforced**.

## Mitigations in place today

1. `Safety::Confirm` on every customer-facing and destructive tool — Alex
   reviews every action before it executes.
2. `remember` is `Safety::Confirm` (B6) — durable rules cannot be planted
   silently via prompt-injection.
3. `post_action::reflect` routes all proposals (including high-confidence
   ones) through `pending_memory_proposals` — confidence ≥ 0.7 no longer
   bypasses the `remember` confirmation gate (H4).
4. `pending_actions.chat_id` ownership check on resolve (S2/H3) — a
   confirmation cannot be hijacked from a different Telegram chat.
5. `agent_actions` audit log of every tool call, with the originating
   pending_action_id and confirmed_action_id linkage.

## Path to an enforced boundary

To promote the boundary from documented to enforced:

1. Open a second sqlx `PgPool` whose connection callback issues `SET ROLE aust_assistant`.
2. Plumb that pool into the `ServiceBundle` constructors so the assistant's
   bridge impls use it instead of the API pool.
3. Restore the grants from `20260609000008` (as a new additive migration —
   editing the old one would break checksums).
4. Remove the revoke in `20260609000027` (again, as a new additive migration).
5. Re-run the assistant test suite — any privilege errors will surface here
   first.

See migrations `20260609000008` and `20260609000027` for the original grants
and revokes.
