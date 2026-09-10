# crates/api — REST API Server

> **Full context**: [AGENTS.md](AGENTS.md)

Axum HTTP server with JWT middleware. 22 route files, 21 repos, 18 service modules.

**Architecture**: `routes/ → repositories/ → PostgreSQL`. Business logic in `services/`.

**Critical patterns**: Repository pattern — new SQL belongs in `repositories/`, not in a handler. `services/bridge/` is the deliberate exception; several `routes/` files still carry inline queries as debt (see AGENTS.md). Offer race condition guard (DB unique constraint), configurable pricing via `CompanyConfig`.

See [AGENTS.md](AGENTS.md) for: file map, critical patterns, submission handlers, test infrastructure, "when adding a new endpoint" checklist.