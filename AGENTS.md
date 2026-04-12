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

## Deployment

```bash
./scripts/deploy.sh           # Full: backup DB → git pull → build → restart → health check
./scripts/staging.sh up       # Staging stack on ports 8099/5435/4173
./scripts/backup-db.sh        # Manual DB backup
```

Production runs as `aust-backend.service` (systemd).