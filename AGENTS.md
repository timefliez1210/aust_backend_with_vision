# AUST Umzüge — Agent Context Index

Moving company automation: customer inquiry → volume estimation → offer generation → scheduling → invoicing.

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
- **[frontend/AGENTS.md](frontend/AGENTS.md)** — SvelteKit admin dashboard, components, stores, pages
- **[services/vision/AGENTS.md](services/vision/AGENTS.md)** — Python ML pipeline, Modal deployment, inference endpoints

## Critical Constraints

1. **DB migrations are one-way doors** — additive only, no destructive changes without explicit agreement
2. **No auto-migration on deploy** — run `migrations/` manually before/after `deploy.sh`
3. **`inquiry_employees` is being replaced by `inquiry_day_employees`** — write to both (dual-write), read from day-level
4. **`preferred_date` is retired** — use `scheduled_date` (DATE) everywhere
5. **Money is stored as cents** (`i64`), never floats. Display: `cents / 100.0`, format DE: `30,00 €`
6. **Customer-facing strings are German** — error messages, emails, offer PDFs, Telegram captions
7. **UUIDs are v7** (time-ordered) for new records

## Status State Machine

```
pending → estimating → estimated → offer_ready → offer_sent → accepted → scheduled → completed → invoiced → paid
                                                                                                  ↘ cancelled
```

Enforced by `InquiryStatus::can_transition_to()` in `crates/core/src/models/inquiry.rs`.
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
- **Test helpers**: `crates/api/src/test_helpers.rs` — DB pool, factories for customer/address/inquiry/employee/day

## ⚠️ Connected Changes — Touch One, Check These

When you modify something in column A, verify or update everything in column B. This is the #1 source of regressions in this codebase.

| If you change... | ...also check/verify | ...because |
|---|---|---|
| `InquiryStatus` enum or state machine | `can_transition_to()`, `is_locked_for_modifications()`, integration tests, admin frontend status labels | Status is enforced in 3 places (model, API handler, frontend) |
| `CompanyConfig` pricing fields | `PricingEngine::with_rate()`, `ServicePrices::from_config()`, offer XLSX template, unit tests | Price constants flow through 4 layers |
| `Services` struct (flags like `packing`, `assembly`) | `build_line_items()`, `format_services_display()`, XLSX rows 31–42, foto-angebot form | Adding a service flag touches submission, offer, and PDF |
| `PricingInput` / `PricingResult` | `build_offer_with_overrides()`, `ServicePrices`, XLSX `persons` cell (J50), Telegram edit flow | Pricing inputs flow into offer generation and Telegram editing |
| `inquiry_day_employees` table | `inquiry_employees` sync (dual-write), calendar queries, clock-time updates, admin employee assignment panel | Day-level is primary but flat table must stay in sync |
| `offers` table or unique constraint | `offer_pipeline.rs` (race guard), `offer_builder.rs` (insert_returning catch), `offer_repo.rs` | Unique partial index prevents duplicates, insert path must handle constraint violation |
| DB migration | `test_helpers.rs` (factory functions), integration tests, `deploy.sh` (manual migrate) | Migrations are one-way; test factories must match new columns |
| Frontend `api.svelte.ts` | All admin pages that call the API | Adding/removing endpoints requires updating both API routes and fetch functions |
| `EstimationMethod` enum | `volume.rs`, `submissions.rs` (4 handlers), `offer_builder.rs` (parse_detected_items), vision service | New estimation methods need handler + parsing + DB CHECK constraint update |
| `build_line_items()` / service prices | XLSX template rows, foto-angebot form, `ServicePrices.from_config()`, unit tests | Line item order and max (12) must match template slots |
| `Scheduled_date` / date fields | Calendar queries, offer PDF date, XLSX cell B17, Telegram summary | Date changes propagate to calendar, offer, PDF, Telegram |
| `address_repo` or address fields | `merge_address_parts()` in all 5 submission handlers, offer PDF address block, XLSX cells A8-A11 | Address format changes must match both submission parsing and PDF rendering |
| `deploy-all.sh` / deployment | Frontend submodule version, DB migration order, `deploy.sh` | Frontend must be built+committed before backend deploys |

## Deployment

```bash
./scripts/deploy.sh           # Full: backup DB → git pull → build → restart → health check
./scripts/staging.sh up       # Staging stack on ports 8099/5435/4173
./scripts/backup-db.sh        # Manual DB backup
```

Production runs as `aust-backend.service` (systemd).