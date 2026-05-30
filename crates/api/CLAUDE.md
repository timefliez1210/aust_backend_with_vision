# crates/api — REST API Server

> **Full context**: [AGENTS.md](AGENTS.md)

Axum HTTP server with JWT middleware. 19 route files, 18 repos, 10 services.

**Architecture**: `routes/ → repositories/ → PostgreSQL`. Business logic in `services/`.

**Critical patterns**: Repository pattern for route handlers (no inline SQL in `routes/`); the `services/bridge/` adapters that back the assistant's `ServiceBundle` are the deliberate exception and use inline SQL. Offer race condition guard (DB unique constraint), configurable pricing via `CompanyConfig`.

See [AGENTS.md](AGENTS.md) for: file map, critical patterns, submission handlers, test infrastructure, "when adding a new endpoint" checklist.