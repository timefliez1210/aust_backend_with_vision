# crates/api — REST API, Repos, Services

The main backend crate. Axum HTTP server with JWT middleware, 18 route files, 16 repository modules, 8 service modules.

## File Map

### Routes (`src/routes/`)

| File | Purpose | Size |
|------|---------|------|
| `submissions.rs` | Public form submissions (photo, mobile, AR, video, manual) | 92KB |
| `admin.rs` | Dashboard, employees, notes, feedback, timesheets | 67KB |
| `invoices.rs` | Invoice CRUD + XLSX generation | 47KB |
| `inquiry_actions.rs` | Estimation triggers, offer generation, employee assignments | 29KB |
| `inquiries.rs` | Inquiry CRUD, status transitions, PDF download, delete | 29KB |
| `calendar.rs` | Calendar schedule, availability, bookings | 13KB |
| `calendar_items.rs` | Calendar item CRUD (non-inquiry work blocks) | 22KB |
| `customer.rs` | Customer-facing endpoints (OTP auth, inquiry list) | 19KB |
| `employee.rs` | Employee CRUD, document upload/download, hours | 26KB |
| `admin_customers.rs` | Admin customer CRUD, address update | 17KB |
| `admin_emails.rs` | Email thread CRUD, drafts, send | 17KB |
| `auth.rs` | JWT login/refresh | 17KB |
| `estimates.rs` | Volume estimation CRUD + image serving | 36KB |

### Repositories (`src/repositories/`)

| File | Key Tables | Notes |
|------|-----------|-------|
| `inquiry_repo.rs` | `inquiries`, `inquiry_employees` | 37KB — largest, most complex |
| `employee_repo.rs` | `employees`, `inquiry_employees` | Document keys use `resolve_doc_column()` allowlist |
| `admin_repo.rs` | Aggregation queries (dashboard, orders) | |
| `calendar_repo.rs` | `inquiries`, `calendar_items`, employee assignments | Schedule queries use `generate_series` to expand multi-day spans |
| `customer_repo.rs` | `customers` | |
| `offer_repo.rs` | `offers` | Unique partial index `offers_inquiry_active_unique` prevents duplicate active offers |
| `estimation_repo.rs` | `volume_estimations` | |
| `invoice_repo.rs` | `invoices`, `invoice_line_items` | |
| `address_repo.rs` | `addresses` | |
| `customer_auth_repo.rs` | `customer_sessions`, OTP | |
| `email_repo.rs` | `email_threads`, `email_messages` | |
| `auth_repo.rs` | `users` (login/role lookups) | |
| `feedback_repo.rs` | `feedback_reports` | Admin customer feedback |
| `invoice_reminder_repo.rs` | `invoice_reminders` | |
| `review_repo.rs` | `reviews` | |

### Services (`src/services/`)

| File | Purpose |
|------|---------|
| `offer_builder.rs` | **71KB** — full offer generation pipeline. Calls pricing engine, builds line items, generates XLSX/PDF, inserts offer. Race-condition safe via DB unique constraint. |
| `inquiry_builder.rs` | Canonical response builder — assembles inquiry detail from 6+ repo calls |
| `telegram_service.rs` | Telegram approval bot (✅ Approve / ✏️ Edit / ❌ Deny) |
| `offer_pipeline.rs` | Auto-offer trigger: check readiness → calculate distance → generate offer |
| `email_dispatch.rs` | SMTP email sending on offer approval |
| `email.rs` | Email formatting helpers |
| `otp_service.rs` | OTP generation + verification |
| `vision.rs` | Vision service client (photo, depth, video) |

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

### Status Gate (M3)
`InquiryStatus::is_locked_for_modifications()` returns true for `offer_ready` through `paid`. When locked, PATCH `/inquiries/{id}` rejects changes to `estimated_volume_m3`, `services`, `distance_km`, `origin_address_id`, `destination_address_id`.

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

`PricingEngine::with_rate(rate, surcharge)` and `ServicePrices::from_config(config)` replace `PricingEngine::new()` in non-test code.

### Submission Handlers
5 handlers in `submissions.rs`: photo, mobile (via `handle_submission`), AR, video, manual. All create billing addresses from parsed fields via `merge_address_parts()`. Manual mode has volume fast-path (skip vision pipeline).

## Test Infrastructure

- `src/test_helpers.rs` — DB pool factory, JWT generator, insert factories (customer, address, inquiry, employee, day, day-employee, estimation)
- `tests/integration_tests.rs` — 20 DB-level integration tests requiring `DATABASE_URL`
- Unit tests in `#[cfg(test)] mod tests` blocks within source files (repos, routes, services)

## When Adding a New Endpoint

1. Add repo function in `repositories/`
2. Add route handler in `routes/`
3. Wire route in `routes/mod.rs`
4. Add integration test in `tests/integration_tests.rs`
5. Update `docs/API.md`

## ⚠️ Connected Changes

| If you change... | ...also verify |
|---|---|
| Inquiry status machine | `can_transition_to()`, `is_locked_for_modifications()`, admin frontend status labels (`INQUIRY_STATUS_LABELS`), `inquiry_repo.rs` status query |
| `CompanyConfig` pricing | `PricingEngine::with_rate()`, `ServicePrices::from_config()`, offer XLSX template pricing cells, unit tests |
| `Services` struct flags | `build_line_items()` in offer_builder, XLSX rows 31–42, foto-angebot form, frontend service toggles |
| `inquiry_employees` schema (add/remove columns) | `calendar_item_employees` mirror, `calendar_repo` schedule queries, `employee_repo` hours queries, admin employee panel |
| `offers` unique constraint | `offer_pipeline.rs` race guard, `offer_builder.rs` insert catch block, `offer_repo.rs` fetch_active_id |
| DB migration | `test_helpers.rs` factory functions, integration tests, manual `deploy.sh` step |
| `EstimationMethod` enum | `volume.rs`, all 5 submission handlers in `submissions.rs`, offer_builder `parse_detected_items()`, DB CHECK constraint migration |
| Address schema | `merge_address_parts()` in all 5 submission handlers, offer PDF address block, XLSX cells A8-A11, frontend address editor |