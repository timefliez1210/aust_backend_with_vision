# AUST Backend

> **Full context**: [AGENTS.md](AGENTS.md)

Moving company automation: inquiry → estimation → offer → scheduling → invoicing.

**Tech**: Rust/Axum, PostgreSQL 16, S3, LLM, Telegram bot. Single-tenant, <1000 req/day.

**Key constraints**: DB migrations additive-only. German for all user-facing strings. Money in cents. UUIDs v7.

**Subsystem deep-dives**:
- [crates/api/AGENTS.md](crates/api/AGENTS.md) — Routes, repos, services, tests
- [crates/core/AGENTS.md](crates/core/AGENTS.md) — Domain models, config, state machines
- [crates/offer-generator/AGENTS.md](crates/offer-generator/AGENTS.md) — Pricing engine, XLSX template
- [crates/distance-calculator/AGENTS.md](crates/distance-calculator/AGENTS.md) — ORS routing
- [crates/email-agent/AGENTS.md](crates/email-agent/AGENTS.md) — IMAP + Telegram approval
- [crates/llm-providers/AGENTS.md](crates/llm-providers/AGENTS.md) — LLM abstraction
- [crates/storage/AGENTS.md](crates/storage/AGENTS.md) — S3 file storage
- [crates/volume-estimator/AGENTS.md](crates/volume-estimator/AGENTS.md) — Vision client
- [frontend/AGENTS.md](frontend/AGENTS.md) — SvelteKit admin dashboard
- [services/vision/AGENTS.md](services/vision/AGENTS.md) — Python ML pipeline