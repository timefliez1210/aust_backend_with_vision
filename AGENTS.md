# AUST Umzüge — Agent Context Index

Moving company automation: customer inquiry → volume estimation → offer generation → scheduling → invoicing.

## 🚨 This is a production system

It runs live at **www.aust-umzuege.de** and the database holds real customer PII.
There is no staging database, only production. If in doubt, ask before running
anything against it.

1. **Migrations are additive only** and run automatically on every container start
   via `sqlx::migrate!()`. No `DROP`, no `DELETE` without `WHERE`, no `TRUNCATE`.
   To remove a column: stop writing it, remove it much later. Back up before any
   deploy carrying a new migration — a bad one is irreversible.
2. **Never log PII.** Names, addresses, phone numbers and email addresses stay out
   of `tracing::info!` and `println!`. Log IDs instead (`inquiry_id={}`).
3. **Never hard-delete customer rows by hand.** The delete endpoints soft-delete,
   or hard-delete after cleaning up S3 first — see `inquiry_actions.rs`. GDPR export
   and deletion requests go through those same paths.
4. **Test against factories, never production data** (`test_helpers.rs`).
5. **Offer PDFs, Telegram messages and SMTP mail reach real customers.** No test
   content in a production code path.

### The three stateful resources

Losing any one of them is irreversible: the `aust_postgres_data` volume (all PII),
the `aust_minio_data` volume (all PDFs, images and employee documents), and
`/opt/aust/.env` on the VPS (secrets).

Both volumes are declared `external: true` precisely so `docker compose down -v`
cannot take them. Never remove that flag, never `docker volume rm` either one, and
never delete DB rows or MinIO objects independently of each other.

Backups: `scripts/backup.sh` runs nightly on the VPS (pg_dump + MinIO tar, 7-day
retention, Telegram alert on a size anomaly), `scripts/pull-backups.sh` replicates
them off-site. Drill the restore quarterly with `scripts/restore-local.sh -y`.
See [DEPLOYMENT.md](DEPLOYMENT.md#backups).

## Map of the Repo

Each row links its own AGENTS.md — read that one before working in that area.

| Where | What |
|---|---|
| [`crates/api/`](crates/api/AGENTS.md) | Axum routes, repositories, services — the main backend |
| [`crates/core/`](crates/core/AGENTS.md) | Config, domain models (`InquiryStatus`, `Services`, `PricingInput`), service traits |
| [`crates/offer-generator/`](crates/offer-generator/AGENTS.md) | Pricing engine, XLSX templates → PDF |
| [`crates/distance-calculator/`](crates/distance-calculator/AGENTS.md) | ORS geocoding + route calculation |
| [`crates/email-agent/`](crates/email-agent/AGENTS.md) | IMAP polling, ParsedInquiry, Telegram approval |
| [`crates/assistant/`](crates/assistant/AGENTS.md) | "Josie" — in-Telegram tool-calling agent |
| [`crates/llm-providers/`](crates/llm-providers/AGENTS.md) | Claude/OpenAI/Ollama trait + mocks |
| [`crates/storage/`](crates/storage/AGENTS.md) | S3/MinIO upload-download-delete trait |
| [`crates/volume-estimator/`](crates/volume-estimator/AGENTS.md) | Vision service client + VLM fallback |
| [`crates/flash-contact/`](crates/flash-contact/AGENTS.md) | Quick-callback form → DB + Telegram ping |
| [`crates/flash-contact-bot/`](crates/flash-contact-bot/AGENTS.md) | Standalone bot binary for those callbacks |
| [`frontend/`](frontend/AGENTS.md) | SvelteKit admin dashboard (git submodule) |
| [`frontend/src/routes/admin/`](frontend/src/routes/admin/AGENTS.md) | Admin SPA pages, components, auth |
| [`app/`](app/AGENTS.md) | Customer capture app (SvelteKit + Capacitor, git submodule) |
| [`services/vision/`](services/vision/AGENTS.md) | FastAPI + GroundingDINO + SAM2 + MASt3R on Modal GPU |
| [`tests/e2e/`](tests/e2e/AGENTS.md) | Playwright suite against the staging stack |

**Language**: German for user-facing strings, English for code.
**Scale**: single-tenant, <1000 req/day — no horizontal scaling needed.
**DB**: PostgreSQL 16, 115+ migrations in `migrations/`, additive only.

## Where code goes

All SQL lives in `crates/api/src/repositories/*_repo.rs`.  
All business logic lives in `crates/api/src/services/`.  
Route handlers are thin orchestration — they call repo + service functions.

## Critical Constraints

1. **Multi-day appointments use `end_date` on the parent** — `inquiries.end_date`
   and `calendar_items.end_date`, NULL meaning a single day. Crew assignments live
   in `inquiry_employees` / `calendar_item_employees`, one row per employee per
   `job_date`. The old `*_days` and `*_day_employees` tables no longer exist.
2. **`preferred_date` is retired** — `scheduled_date` (DATE) everywhere.
3. **Money is `i64` cents**, never a float. Display as `cents / 100.0`, formatted
   German: `30,00 €`.
4. **UUIDs are v7** (time-ordered) for new records.

## Status State Machine

```
pending → info_requested → estimating → estimated → offer_ready → offer_sent → accepted → scheduled → completed → invoiced → paid
                                                                                                  ↘ cancelled
```

Informational only — `can_transition_to()` returns `true` for all transitions (admin dashboard has full flexibility).

## Key Data Flow: Submission → Offer

1. **Photo/Mobile/AR/Video/Manual** → `submissions.rs` → `handle_submission()`
2. Parse form → merge addresses → create customer + inquiry + estimation
3. If volume available → skip vision pipeline, create "manual" estimation
4. Calculate ORS distance → `try_auto_generate_offer()`
5. `offer_builder.rs::build_offer_with_overrides()` → pricing → XLSX → PDF → S3
6. Insert offer (unique constraint prevents duplicates under concurrency)
7. Telegram approval → `email_dispatch` on accept

## Testing

- **Unit tests**: `cargo test --lib --workspace` — zero DB dependency, runs across every crate (`aust-api` and `aust-assistant` carry the bulk of them)
- **Integration tests**: `DATABASE_URL=... cargo test -p aust-api --tests` — needs Postgres, spins up a throwaway DB per test via `#[sqlx::test(migrations = "../../migrations")]`; lives in `crates/api/tests/integration_tests.rs` (bug-regression tests) and `crates/api/tests/e2e_submissions.rs` (submission-handler coverage)
- **Test helpers**: `crates/api/src/test_helpers.rs` — DB pool, factories for customer/address/inquiry/employee
- **E2E**: Playwright suite in `tests/e2e/` against the staging stack — see `tests/e2e/AGENTS.md`

## ⚠️ Connected Changes — Touch One, Check These

When you modify something in column A, verify or update everything in column B. This is the #1 source of regressions in this codebase.

| If you change... | ...also check/verify | ...because |
|---|---|---|
| `InquiryStatus` enum or state machine | `can_transition_to()`, integration tests, admin frontend status labels | Status is enforced in 3 places (model, API handler, frontend) |
| `CompanyConfig` pricing fields | `PricingEngine::with_rate()`, `ServicePrices::from_pricing()`, offer XLSX template, unit tests | Price constants flow through 4 layers |
| `Services` struct (flags like `packing`, `assembly`) | `build_line_items()`, `format_services_display()`, XLSX rows 31–50, foto-angebot form | Adding a service flag touches submission, offer, and PDF |
| `PricingInput` / `PricingResult` | `build_offer_with_overrides()`, `ServicePrices`, XLSX `persons` cell (J58), Telegram edit flow | Pricing inputs flow into offer generation and Telegram editing |
| `inquiry_employees` / `calendar_item_employees` schema | `calendar_repo` schedule queries, `employee_repo` hours/schedule queries, admin employee panel, `inquiry_builder` snapshot | One row per (entity, employee, job_date) — all reads go through this single flat table |
| `offers` table or unique constraint | `offer_pipeline.rs` (race guard), `offer_builder.rs` (insert_returning catch), `offer_repo.rs`, every "active offer" query | Regenerating a KVA **updates the existing row**; the unique partial index plus an advisory lock in the insert branch keep it at one active offer per inquiry. `'superseded'` rows exist in prod but nothing writes them any more, so every "active offer" query still has to filter them out or an old price resurfaces (regressed once, fixed) |
| DB migration | `test_helpers.rs` (factory functions), integration tests, `deploy-prod.sh` (manual migrate) | Migrations are one-way; test factories must match new columns |
| Frontend `api.svelte.ts` | All admin pages that call the API | Adding/removing endpoints requires updating both API routes and fetch functions |
| `EstimationMethod` enum | `volume.rs`, `submissions.rs` (5 handlers), `offer_builder.rs` (parse_detected_items), vision service | New estimation methods need handler + parsing; `volume_estimations.method` is a plain VARCHAR with no CHECK constraint, so nothing rejects a typo |
| `build_line_items()` / service prices | XLSX template rows, foto-angebot form, `ServicePrices.from_pricing()`, unit tests | Line item order and the max of 20 must match the template slots (rows 31–50) |
| `Scheduled_date` / date fields | Calendar queries, offer PDF date, XLSX cell B17, Telegram summary | Date changes propagate to calendar, offer, PDF, Telegram |
| `address_repo` or address fields | `merge_address_parts()` in all 5 submission handlers, offer PDF address block, XLSX cells A8-A11 | Address format changes must match both submission parsing and PDF rendering |
| `deploy-prod.sh` / deployment | Frontend submodule version, DB migration order, `migrations/` | Migrations auto-run on startup; upload new ones before deploy |

## Deployment

```bash
bash scripts/deploy-prod.sh          # Deploy backend + flash-bot: backup VPS → build image → push → restart → health check
bash scripts/deploy-full.sh          # Same as above, plus build + FTP-deploy the frontend to KAS hosting
bash scripts/staging-up.sh           # Start full staging stack (Docker) on ports 8099/5435/4173
bash scripts/staging-up.sh --rebuild # Force rebuild of staging images
bash scripts/dev-up.sh               # Local dev with hot reload (cargo watch + Vite) on 8080/5173
bash scripts/backup.sh               # Manual backup (runs ON the VPS)
```

Production runs as a Docker container managed by `docker compose` on the VPS (`/opt/aust/docker-compose.yml`).
Migrations run automatically on container startup via `sqlx::migrate!()` — no manual step needed.
See [DEPLOYMENT.md](DEPLOYMENT.md) for full details including rollback, restore drill, and staging setup.