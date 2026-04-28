# AUST Umzüge — Agent Context Index

Moving company automation: customer inquiry → volume estimation → offer generation → scheduling → invoicing.

## 🚨 THIS IS A PRODUCTION SYSTEM

This system runs live at **www.aust-umzuege.de**. The PostgreSQL database contains **real customer PII**: names, addresses, phone numbers, email addresses, moving details. There is no staging database — only production.

**Rules that follow from this:**

1. **Never run destructive SQL** — no `DROP COLUMN`, `DROP TABLE`, `DELETE` without `WHERE`, or `TRUNCATE`. All migrations must be additive (add column, add table, add index). If a column must be removed, do it in two steps: stop writing → remove later.
2. **Never log PII** — no customer names, addresses, phone numbers, or email in `tracing::info!` or `println!`. Use IDs only (`inquiry_id={}`, `customer_id={}`).
3. **Never hard-delete customer rows** — the delete endpoints soft-delete (set `deleted_at`) or hard-delete with S3 cleanup first. See `inquiry_actions.rs` for the pattern.
4. **Migrations run automatically on startup** — `sqlx::migrate!()` runs every time the backend container starts. Upload `migrations/` to the VPS before restarting (`deploy-prod.sh` does this). A bad migration on production data is irreversible — always back up first.
5. **Test against factories, not production data** — `test_helpers.rs` creates test data. Never point tests at production.
6. **GDPR applies** — customers can request data export or deletion. The `delete_customer` endpoint must clean up S3 objects (offers PDFs, estimation images, employee contracts) before removing the DB row.
7. **Email contains real content** — offer PDFs, Telegram messages, and SMTP emails go to real customers. Never hardcode test content in production code paths.

If in doubt, ask before running anything against the database.

## Data Integrity

Production data lives in exactly three stateful resources. Losing any one of them is irreversible.

### Stateful resources

- **`aust_postgres_data`** — Docker named volume. All customer PII, inquiries, offers, invoices, calendar, employees.
- **`aust_minio_data`** — Docker named volume. All binary objects: PDFs, images, employee docs. PostgreSQL references S3 keys here.
- **`/opt/aust/.env`** — Secrets on the VPS.

### Never-do list

- **Never `docker compose down -v`** — volumes are `external: true` specifically to block this.
- **Never `docker volume rm aust_postgres_data` or `aust_minio_data`.**
- **Never delete DB rows or MinIO objects independently.** Use hard-delete helpers in `inquiry_actions.rs`.
- **Never deploy with anonymous or renamed volumes.**

### Backups & Restore

- **VPS nightly backup** (`scripts/backup.sh` at 03:00 UTC): `pg_dump` + MinIO tar → `/opt/aust/backups/`, 7-day retention. Size anomaly alerts via Telegram.
- **Off-site replication** (`scripts/pull-backups.sh`): rsyncs to dev machine. Run local cron via `scripts/setup-local-backup-cron.sh`.
- **Restore drill**: `scripts/pull-backups.sh && scripts/restore-local.sh -y` quarterly. Verify row counts and bucket sizes.

See [DEPLOYMENT.md](DEPLOYMENT.md#backups) and [scripts/backup.sh](scripts/backup.sh).

## Quick Orientation

| What | Where | One-line |
|------|-------|----------|
| Rust API server | `crates/api/` | Axum routes, repos, services — the main backend |
| Domain models | `crates/core/` | Config, models (`InquiryStatus`, `Services`, `PricingInput`), shared types |
| Offer generator | `crates/offer-generator/` | Pricing engine, XLSX template → PDF |
| Distance calculator | `crates/distance-calculator/` | ORS route calculation |
| Email agent | `crates/email-agent/` | IMAP polling, ParsedInquiry, Telegram approval |
| LLM providers | `crates/llm-providers/` | Claude/OpenAI/Ollama trait + mocks |
| Object storage | `crates/storage/` | S3/MinIO upload-download-delete trait |
| Volume estimator | `crates/volume-estimator/` | Vision service client |
| Admin frontend | `frontend/` | SvelteKit dashboard (git submodule) |
| Python vision | `services/vision/` | FastAPI + GroundingDINO + SAM2 + MASt3R on Modal GPU |

**Language**: German for all user-facing strings. English for code.

**Scale**: Single-tenant, <1000 req/day. No horizontal scaling needed.

**DB**: PostgreSQL 16, 40+ migrations in `migrations/`. **Additive only** — never destructive without explicit agreement.

## Architecture in 30 seconds

```
Customer form / email / photo app
        │
        ▼
  submissions.rs ──► inquiry_repo ──► PostgreSQL
        │                              │
        ▼                              │
  offer_pipeline.rs ──────────────────►│
        │                              │
        ▼                              │
  offer_builder.rs ──► offer_repo ───►│
        │                              │
        ▼                              │
  telegram_service ──► Alex approves    │
        │                              │
        ▼                              │
  email_dispatch ──► SMTP to customer  ◄┘
```

All SQL lives in `crates/api/src/repositories/*_repo.rs`.  
All business logic lives in `crates/api/src/services/`.  
Route handlers are thin orchestration — they call repo + service functions.

## Subsystem Deep-Dives

When working on a specific area, read the corresponding AGENTS.md for focused context:

- **[crates/api/AGENTS.md](crates/api/AGENTS.md)** — Routes, repos, services, types, test infrastructure
- **[crates/core/AGENTS.md](crates/core/AGENTS.md)** — Domain models, config, state machines, shared types
- **[crates/offer-generator/AGENTS.md](crates/offer-generator/AGENTS.md)** — Pricing engine, XLSX template, line items
- **[crates/distance-calculator/AGENTS.md](crates/distance-calculator/AGENTS.md)** — ORS geocoding + routing
- **[crates/email-agent/AGENTS.md](crates/email-agent/AGENTS.md)** — IMAP polling, parsing, Telegram bot
- **[crates/llm-providers/AGENTS.md](crates/llm-providers/AGENTS.md)** — Claude/OpenAI/Ollama trait abstraction
- **[crates/storage/AGENTS.md](crates/storage/AGENTS.md)** — S3/MinIO upload-download-delete trait
- **[crates/volume-estimator/AGENTS.md](crates/volume-estimator/AGENTS.md)** — Vision service client, estimation methods
- **[frontend/AGENTS.md](frontend/AGENTS.md)** — SvelteKit admin dashboard, components, stores, pages
- **[frontend/src/routes/admin/AGENTS.md](frontend/src/routes/admin/AGENTS.md)** — Admin SPA pages, components, auth, multi-day scheduling
- **[services/vision/AGENTS.md](services/vision/AGENTS.md)** — Python ML pipeline, Modal deployment, inference endpoints
- **[app/AGENTS.md](app/AGENTS.md)** — Mobile customer photo app (SvelteKit + Capacitor)

## Critical Constraints

1. **DB migrations are one-way doors** — additive only, no destructive changes without explicit agreement (see 🚨 above)
2. **Migrations auto-run on startup** — `deploy-prod.sh` uploads `migrations/` then restarts the container; `sqlx::migrate!()` applies them. Back up production DB before any deploy that includes new migrations.
3. **Multi-day appointments use `end_date` on the parent** — `inquiries.end_date` and `calendar_items.end_date` (NULL = single day). Employee assignments are in `inquiry_employees` / `calendar_item_employees` with a `job_date` column (one row per employee per day). The old `inquiry_days`, `inquiry_day_employees`, `calendar_item_days`, `calendar_item_day_employees` tables no longer exist.
4. **`preferred_date` is retired** — use `scheduled_date` (DATE) everywhere
5. **Money is stored as cents** (`i64`), never floats. Display: `cents / 100.0`, format DE: `30,00 €`
6. **Customer-facing strings are German** — error messages, emails, offer PDFs, Telegram captions
7. **UUIDs are v7** (time-ordered) for new records

## Status State Machine

```
pending → estimating → estimated → offer_ready → offer_sent → accepted → scheduled → completed → invoiced → paid
                                                                                                  ↘ cancelled
```

Informational only — `can_transition_to()` returns `true` for all transitions (admin dashboard has full flexibility).
Once `offer_ready` or beyond, core fields (volume, services, distance, addresses) are locked — see `is_locked_for_modifications()`.

## Key Data Flow: Submission → Offer

1. **Photo/Mobile/AR/Video/Manual** → `submissions.rs` → `handle_submission()`
2. Parse form → merge addresses → create customer + inquiry + estimation
3. If volume available → skip vision pipeline, create "manual" estimation
4. Calculate ORS distance → `try_auto_generate_offer()`
5. `offer_builder.rs::build_offer_with_overrides()` → pricing → XLSX → PDF → S3
6. Insert offer (unique constraint prevents duplicates under concurrency)
7. Telegram approval → `email_dispatch` on accept

## Testing

- **Unit tests**: `cargo test --lib --workspace` (219 tests, zero DB dependency)
- **Integration tests**: `DATABASE_URL=... cargo test -p aust-api --test integration_tests` (20 tests, needs Postgres)
- **Test helpers**: `crates/api/src/test_helpers.rs` — DB pool, factories for customer/address/inquiry/employee

## ⚠️ Connected Changes — Touch One, Check These

When you modify something in column A, verify or update everything in column B. This is the #1 source of regressions in this codebase.

| If you change... | ...also check/verify | ...because |
|---|---|---|
| `InquiryStatus` enum or state machine | `can_transition_to()`, `is_locked_for_modifications()`, integration tests, admin frontend status labels | Status is enforced in 3 places (model, API handler, frontend) |
| `CompanyConfig` pricing fields | `PricingEngine::with_rate()`, `ServicePrices::from_config()`, offer XLSX template, unit tests | Price constants flow through 4 layers |
| `Services` struct (flags like `packing`, `assembly`) | `build_line_items()`, `format_services_display()`, XLSX rows 31–42, foto-angebot form | Adding a service flag touches submission, offer, and PDF |
| `PricingInput` / `PricingResult` | `build_offer_with_overrides()`, `ServicePrices`, XLSX `persons` cell (J50), Telegram edit flow | Pricing inputs flow into offer generation and Telegram editing |
| `inquiry_employees` / `calendar_item_employees` schema | `calendar_repo` schedule queries, `employee_repo` hours/schedule queries, admin employee panel, `inquiry_builder` snapshot | One row per (entity, employee, job_date) — all reads go through this single flat table |
| `offers` table or unique constraint | `offer_pipeline.rs` (race guard), `offer_builder.rs` (insert_returning catch), `offer_repo.rs` | Unique partial index prevents duplicates, insert path must handle constraint violation |
| DB migration | `test_helpers.rs` (factory functions), integration tests, `deploy.sh` (manual migrate) | Migrations are one-way; test factories must match new columns |
| Frontend `api.svelte.ts` | All admin pages that call the API | Adding/removing endpoints requires updating both API routes and fetch functions |
| `EstimationMethod` enum | `volume.rs`, `submissions.rs` (4 handlers), `offer_builder.rs` (parse_detected_items), vision service | New estimation methods need handler + parsing + DB CHECK constraint update |
| `build_line_items()` / service prices | XLSX template rows, foto-angebot form, `ServicePrices.from_config()`, unit tests | Line item order and max (12) must match template slots |
| `Scheduled_date` / date fields | Calendar queries, offer PDF date, XLSX cell B17, Telegram summary | Date changes propagate to calendar, offer, PDF, Telegram |
| `address_repo` or address fields | `merge_address_parts()` in all 5 submission handlers, offer PDF address block, XLSX cells A8-A11 | Address format changes must match both submission parsing and PDF rendering |
| `deploy-prod.sh` / deployment | Frontend submodule version, DB migration order, `migrations/` | Migrations auto-run on startup; upload new ones before deploy |

## Deployment

```bash
bash scripts/deploy-prod.sh          # Deploy: backup VPS → build image → push → restart → health check
bash scripts/staging-up.sh           # Start full staging stack (Docker) on ports 8099/5435/4173
bash scripts/staging-up.sh --rebuild # Force rebuild of staging images
bash scripts/dev-up.sh               # Local dev with hot reload (cargo watch + Vite) on 8080/5173
bash scripts/backup.sh               # Manual backup (runs ON the VPS)
```

Production runs as a Docker container managed by `docker compose` on the VPS (`/opt/aust/docker-compose.yml`).
Migrations run automatically on container startup via `sqlx::migrate!()` — no manual step needed.
See [DEPLOYMENT.md](DEPLOYMENT.md) for full details including rollback, restore drill, and staging setup.