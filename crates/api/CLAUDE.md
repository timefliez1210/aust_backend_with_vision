# crates/api — REST API Server

> **Full context**: [AGENTS.md](AGENTS.md)

Axum HTTP server with JWT middleware. 18 route files, 13 repos, 9 services.

**Architecture**: `routes/ → repositories/ → PostgreSQL`. Business logic in `services/`.

**Critical patterns**: Repository pattern (no inline SQL), dual-write to day-employees, status gate (`is_locked_for_modifications`), offer race condition guard (DB unique constraint), configurable pricing via `CompanyConfig`.

See [AGENTS.md](AGENTS.md) for: file map, critical patterns, submission handlers, test infrastructure, "when adding a new endpoint" checklist.