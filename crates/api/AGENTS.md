# crates/api — REST API, Repos, Services

The main backend crate. Axum HTTP server with JWT middleware, 22 route files, 21 repository modules, 18 service modules (16 files + `assistant_bridge/` and `bridge/`).

## File Map

### Routes (`src/routes/`)

| File | Purpose |
|------|---------|
| `submissions.rs` | Public form submissions (photo, mobile, AR, video, manual) |
| `admin.rs` | Dashboard, employees, notes, feedback, timesheets |
| `invoices.rs` | Invoice CRUD + XLSX generation |
| `inquiry_actions.rs` | Estimation triggers, offer generation, employee assignments |
| `inquiries.rs` | Inquiry CRUD, status transitions, PDF download, delete |
| `calendar.rs` | Calendar schedule, availability, bookings |
| `calendar_items.rs` | Calendar item CRUD (non-inquiry work blocks) |
| `customer.rs` | Customer-facing endpoints (OTP auth, inquiry list) |
| `employee.rs` | Employee CRUD, document upload/download, hours |
| `admin_customers.rs` | Admin customer CRUD, address update |
| `admin_emails.rs` | Email thread CRUD, drafts, send |
| `auth.rs` | JWT login/refresh |
| `estimates.rs` | Volume estimation CRUD + image serving |
| `distance.rs` | ORS distance calculation endpoint |
| `flash_contact.rs` | Public `POST /flash-contact` callback form, own rate limiter |
| `health.rs` | Health/readiness checks |
| `shared.rs` | Shared route utilities |
| `offers.rs` | Minimal offer route stub |
| `agent_activity.rs` | Admin view of the assistant's `agent_actions` audit log, `/admin/agent-activity` |
| `inquiry_appointments.rs` | CRUD for lightweight non-crew appointments (e.g. Besichtigung) on an inquiry, `/inquiries/{id}/appointments` |
| `storage.rs` | Storage-rental ("Lagerung") admin routes, `/admin/storage` — contracts + auto-generated monthly invoices, brutto-in/netto-stored at the boundary |
| `vehicles.rs` | Vehicle fleet CRUD + reminders (TÜV, Ölwechsel, ...), `/admin/vehicles` |

### Repositories (`src/repositories/`)

| File | Key Tables | Notes |
|------|-----------|-------|
| `inquiry_repo.rs` | `inquiries`, `inquiry_employees` | The largest and most complex repo |
| `employee_repo.rs` | `employees`, `inquiry_employees` | Document keys use `resolve_doc_column()` allowlist |
| `admin_repo.rs` | Aggregation queries (dashboard, orders) | |
| `calendar_repo.rs` | `inquiries`, `calendar_items`, employee assignments | Schedule queries use `generate_series` to expand multi-day spans |
| `customer_repo.rs` | `customers` | |
| `offer_repo.rs` | `offers` | Unique partial index `offers_inquiry_active_unique` prevents duplicate active offers |
| `estimation_repo.rs` | `volume_estimations` | |
| `invoice_repo.rs` | `invoices` (hand-edited positions live in its `line_items_json` column; there is no line-items table) | |
| `address_repo.rs` | `addresses` | |
| `customer_auth_repo.rs` | `customer_sessions`, OTP | |
| `email_repo.rs` | `email_threads`, `email_messages` | |
| `auth_repo.rs` | `users` (login/role lookups) | |
| `feedback_repo.rs` | `feedback_reports` | Admin customer feedback |
| `invoice_reminder_repo.rs` | `invoice_reminders` | Dashboard-driven dunning flow |
| `review_repo.rs` | `review_requests` | Google-review follow-up emails |
| `settings_repo.rs` | `settings` (invoices, reminders, review config) | |
| `calendar_item_repo.rs` | `calendar_items`, `calendar_item_employees` | Non-inquiry work blocks; mirrors `inquiry_employees` shape |
| `customer_address_repo.rs` | `customer_addresses` | Per-customer address book; rows are self-contained copies, not FKs into `addresses` |
| `inquiry_appointment_repo.rs` | `inquiry_appointments` | Lightweight, possibly non-consecutive appointments (e.g. Besichtigung); NOT crew/hours tracked |
| `storage_repo.rs` | `storage_contracts`, storage invoices | Deliberately isolated from `invoice_repo`/`inquiry_repo` |
| `vehicle_repo.rs` | `vehicles`, `vehicle_reminders` | |

### Services (`src/services/`)

| File | Purpose |
|------|---------|
| `offer_builder.rs` | The full offer-generation pipeline, and the largest file in the crate. Calls pricing engine, builds line items, generates XLSX/PDF, inserts offer. Race-condition safe via DB unique constraint. |
| `inquiry_builder.rs` | Canonical response builder — assembles inquiry detail from 6+ repo calls |
| `telegram_service.rs` | Telegram approval bot (✅ Approve / ✏️ Edit / ❌ Deny) |
| `offer_pipeline.rs` | Auto-offer trigger: check readiness → calculate distance → generate offer |
| `email_dispatch.rs` | SMTP email sending on offer approval |
| `email.rs` | Email formatting helpers |
| `otp_service.rs` | Shared OTP request/verify logic, used by both customer and employee auth flows |
| `vision.rs` | Vision service client (photo, depth, video) |
| `flash_contact_service.rs` | Flash-contact reminder cron (`run_reminder_check`) — sends the delayed Telegram ping via the flash-contact bot token |
| `invoice_number.rs` | Invoice numbers: parse, format (`YYYY-NN`, two-digit minimum), and the register sort order |
| `kva_export.rs` | XLSX export for the KVA-Buch; mirrors `register_export`'s workbook plumbing |
| `kva_followup_service.rs` | Nachfassen cron for Kostenvoranschläge — 60s tick spawned in `src/main.rs`, pings Telegram |
| `register_export.rs` | Rechnungsausgangsbuch → XLSX, handed to the Steuerberater |
| `billing_reminder_service.rs` | Zahlungserinnerung/Mahnung dunning + review-request logic; driven by both admin routes and the assistant service bridge |
| `storage_billing_service.rs` | Generates one invoice per active storage contract per calendar month |
| `vehicle_reminder_service.rs` | Vehicle reminder cron — 60s tick spawned in `src/main.rs`, pings Telegram |
| `assistant_bridge/` | Glue between the `aust-assistant` driver and the Telegram bot / offer pipeline (`notifier_impl`, `telegram_input`/`telegram_output`, `confirm_dispatcher`, `media`) |
| `bridge/` | One `*ServiceImpl` per `aust_core::services::traits` trait, delegating to these repos/services; grouped into `ServiceBundle` at startup for the assistant's `ToolCtx` |

## Critical Patterns

### Repository Pattern
ALL SQL goes in `src/repositories/*_repo.rs`. Route handlers never contain inline `sqlx::query`. If you need a new query, add a function to the appropriate repo module.

### Scheduling Model (single code path)
Multi-day appointments are expressed via `inquiries.end_date` (NULL = same day as `scheduled_date`) and `calendar_items.end_date`. Employee assignments live in one flat table per entity type:

- `inquiry_employees` — one row per `(inquiry_id, employee_id, job_date)`. Unique key includes `job_date`.
- `calendar_item_employees` — same shape for calendar items.

The old `inquiry_days`, `inquiry_day_employees`, `calendar_item_days`, `calendar_item_day_employees` tables were dropped in migration `20260601000000_simplify_scheduling.sql`.

**Calendar schedule query** (`calendar_repo::fetch_schedule_inquiries`) uses `CROSS JOIN LATERAL generate_series(scheduled_date, COALESCE(end_date, scheduled_date), '1 day')` to expand multi-day inquiries into one row per day, then LEFT JOINs `inquiry_employees ie ON ie.job_date = gs.day` for per-day staffing.

**Employee assignment endpoints**: `GET/PUT /api/v1/inquiries/{id}/employees` and `GET/PUT /api/v1/calendar-items/{id}/employees`. PUT does full-replace (delete all + insert). Body is a flat array of `{employee_id, job_date, planned_hours, ...}`.

**`day_number` and `total_days`** are computed on the fly: `(job_date - scheduled_date + 1)` and `(end_date - scheduled_date + 1)`.


### Offer Race Condition (M1)
`offers_inquiry_active_unique` partial unique index prevents duplicate active offers. `offer_builder.rs::insert_returning()` catches constraint violations and falls back to updating the existing offer.

### Configurable Pricing (M2)
All pricing constants are in `CompanyConfig`:
- `rate_per_person_hour_cents` (default 3000 = €30/hr)
- `assembly_price` (default 25.0 = €25)
- `parking_ban_price` (default 100.0 = €100)
- `packing_price` (default 30.0 = €30)
- `saturday_surcharge_cents` (default 5000 = €50)
- `fahrt_rate_per_km` (default 1.0)

`PricingEngine::with_rate(rate, surcharge)` and `ServicePrices::from_pricing()` replaces `PricingEngine::new()` in non-test code.

### Submission Handlers
5 handlers in `submissions.rs`: photo, mobile (via `handle_submission`), AR, video, manual. All create billing addresses from parsed fields via `merge_address_parts()`. Manual mode has volume fast-path (skip vision pipeline).

**AR submissions are volume-first.** The mobile app's capture screen makes the item name optional, so `item_manifest` entries may carry an empty (or missing) `label`. `device_volume_items()` keeps those items — only an implausible `device_volume_m3` (outside 0.005–12 m³) drops the whole submission back to server-side vision. Unnamed items are then named by `fill_missing_labels()` via `VlmEstimator::label_objects` (one representative frame per item, batches of 8), falling back to `aust_volume_estimator::FALLBACK_LABEL` when the VLM backend is unconfigured or unreachable. A missing name must never cost a measured volume.

### Middleware (`src/middleware/`)

| File | Purpose |
|------|---------|
| `auth.rs` | Admin JWT verification, populates `TokenClaims` extension |
| `customer_auth.rs` | Customer session-token check (DB-backed, not JWT) — see `crates/api/AGENTS.md` customer app notes |
| `employee_auth.rs` | Employee session-token check for the worker-facing endpoints |
| `rate_limit.rs` | `RateLimiter` + `apply_rate_limit`; instantiate one per endpoint group that needs its own bucket (e.g. `flash_contact.rs` uses its own instance, separate from the auth-route limiter) |
| `request_id.rs` | Assigns/propagates a request ID, wraps the handler span |
| `security_headers.rs` | Injects standard security headers on every response |

## Test Infrastructure

- `src/test_helpers.rs` — DB pool factory, JWT generator, insert factories (customer, address, inquiry, employee, estimation)
- `tests/integration_tests.rs` — 25 DB-level integration tests requiring `DATABASE_URL`
- `tests/e2e_submissions.rs` — 19 tests exercising the submission handlers end-to-end
- Unit tests in `#[cfg(test)] mod tests` blocks within source files (repos, routes, services)

## When Adding a New Endpoint

1. Add repo function in `repositories/`
2. Add route handler in `routes/`
3. Wire route in `routes/mod.rs`
4. Add integration test in `tests/integration_tests.rs`
5. Update `docs/API.md`

## ⚠️ Connected Changes

The repo-wide table is in the [root AGENTS.md](../../AGENTS.md#-connected-changes--touch-one-check-these).
Specific to this crate:

| If you change... | ...also verify |
|---|---|
| Inquiry status handling | `can_transition_to()` in core, the PATCH validation in `inquiries.rs`, `inquiry_repo.rs` status queries, `INQUIRY_STATUS_LABELS` in the frontend |
| `inquiry_employees` columns | the `calendar_item_employees` mirror, `calendar_repo` schedule queries, `employee_repo` hours queries, the admin employee panel |
| The `offers` unique constraint | the race guard in `offer_pipeline.rs`, the insert catch in `offer_builder.rs`, `offer_repo.rs::fetch_active_id` |
| Address handling | `merge_address_parts()` in all 5 submission handlers, the offer PDF address block, XLSX cells A8-A11, the frontend address editor |
