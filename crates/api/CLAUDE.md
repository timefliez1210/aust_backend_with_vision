# crates/api — REST API, Routing & Offer Orchestration

> Pipeline map: [../../docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md)
> Recurring bugs (employee hours NULL, brutto/netto price): [../../docs/DEBUGGING.md](../../docs/DEBUGGING.md)

HTTP server built on Axum 0.8. Defines all REST endpoints, request/response types, application state, and the offer orchestration pipeline.

## Architecture

```
routes/           — HTTP handlers (request parsing, response building)
  ↓ calls
repositories/     — SQL queries (all sqlx::query calls live here)
  ↓ reads/writes
PostgreSQL

services/         — Business logic (no SQL, no HTTP)
  offer_builder   — offer generation pipeline (pricing → XLSX → PDF → S3)
  inquiry_builder — canonical InquiryResponse/InquiryListItem builder
  telegram_service — Telegram approval/edit flow
  email_dispatch  — SMTP email on offer approval
  offer_pipeline  — auto-offer triggering after estimation
```

## Key Files

### Routes (`src/routes/`)

| File | Responsibility |
|------|---------------|
| `mod.rs` | Route tree assembly (`public_api_router`, `protected_api_router`) |
| `inquiries.rs` | Inquiry CRUD (create, list, get, update, delete) |
| `inquiry_actions.rs` | Estimation, offer generation, items, employee assignments, emails |
| `submissions.rs` | Public multipart uploads (`/submit/photo`, `/submit/mobile`) |
| `admin.rs` | Dashboard, employees, router assembly for admin sub-routes |
| `admin_customers.rs` | Customer CRUD (list, create, get, update, delete) |
| `admin_emails.rs` | Email thread listing, detail, reply, compose |
| `estimates.rs` | Public image/video proxy + protected estimation CRUD (polling, delete) |
| `calendar.rs` | Calendar availability, schedule, bookings, capacity |
| `calendar_items.rs` | CalendarItem/Termine CRUD |
| `customer.rs` | Customer-facing endpoints (OTP auth, accept/reject offer) |
| `auth.rs` | Admin authentication (login, refresh, OTP) |
| `employee.rs` | Employee auth routes |
| `invoices.rs` | Invoice endpoints |
| `offers.rs` | Re-export shim → `services::offer_builder` |
| `shared.rs` | Shared utilities (pagination, error mapping) |
| `health.rs` | Health/readiness checks |

### Repositories (`src/repositories/`)

All SQL queries are centralized here. Route handlers call repo functions instead of inline `sqlx::query`.

| File | Queries | Domain |
|------|---------|--------|
| `inquiry_repo.rs` | 25 | Inquiry CRUD, status transitions, volume updates |
| `admin_repo.rs` | 42 | Dashboard KPIs, user listing, orders |
| `employee_repo.rs` | 33 | Employee CRUD, assignments, hours |
| `estimation_repo.rs` | 15 | Volume estimation lifecycle |
| `invoice_repo.rs` | 23 | Invoice CRUD, address snapshots |
| `customer_auth_repo.rs` | 16 | OTP codes, sessions |
| `offer_repo.rs` | 15 | Offer storage, active PDF lookup |
| `auth_repo.rs` | 12 | Admin users, JWT tokens |
| `calendar_repo.rs` | 12 | Bookings, capacity overrides |
| `calendar_item_repo.rs` | 12 | CalendarItem/Termine CRUD |
| `email_repo.rs` | 6 | Email threads, messages |
| `customer_repo.rs` | 5 | Customer upsert, lookup |
| `address_repo.rs` | 3 | Address creation |

### Services (`src/services/`)

| File | Responsibility |
|------|---------------|
| `offer_builder.rs` | Full offer pipeline: pricing → line items → XLSX → PDF → S3 |
| `inquiry_builder.rs` | Canonical `build_inquiry_response()` and `build_inquiry_list()` |
| `telegram_service.rs` | Telegram approval/edit flow (send PDF, handle callbacks) |
| `email_dispatch.rs` | SMTP email on offer approval |
| `offer_pipeline.rs` | Auto-offer triggering after estimation completes |
| `vision.rs` | Vision service client wrapper |
| `email.rs` | Email service utilities |
| `db.rs` | Legacy shared SQL helpers (being migrated to repositories) |

### Other

- `src/orchestrator.rs` — Background event handler (receives from email agent via mpsc channel)
- `src/state.rs` — `AppState` shared across handlers
- `src/error.rs` — `ApiError` enum with typed variants
- `src/lib.rs` — Router assembly, middleware stack

## AppState

```rust
pub struct AppState {
    pub db: PgPool,
    pub llm: Arc<dyn LlmProvider>,
    pub storage: Arc<dyn StorageProvider>,
    pub calendar_service: Arc<CalendarService>,
    pub vision_service: Option<VisionServiceClient>,
    pub config: Config,
}
```

## Endpoints

### Inquiries (`/api/v1/inquiries`) — Main Resource

| Method | Path | Handler file | Description |
|--------|------|-------------|-------------|
| POST | `/` | `inquiries.rs` | Create inquiry (customer_email + addresses) → 201 |
| GET | `/` | `inquiries.rs` | List with filters: status, search, has_offer, limit, offset |
| GET | `/{id}` | `inquiries.rs` | Full detail with customer, addresses, estimation, items, offer |
| PATCH | `/{id}` | `inquiries.rs` | Update fields / transition status |
| DELETE | `/{id}` | `inquiries.rs` | Soft-delete → status=cancelled |
| GET | `/{id}/pdf` | `inquiries.rs` | Download latest active offer PDF |
| PUT | `/{id}/items` | `inquiry_actions.rs` | Replace estimation items + recalculate volume |
| POST | `/{id}/estimate/{method}` | `inquiry_actions.rs` | Trigger estimation (depth, video) |
| POST | `/{id}/generate-offer` | `inquiry_actions.rs` | Generate/regenerate offer |
| GET | `/{id}/emails` | `inquiry_actions.rs` | Email thread for this inquiry |
| GET | `/{id}/employees` | `inquiry_actions.rs` | List assigned employees |
| POST | `/{id}/employees` | `inquiry_actions.rs` | Assign employee |
| PATCH | `/{id}/employees/{emp_id}` | `inquiry_actions.rs` | Update hours/notes |
| DELETE | `/{id}/employees/{emp_id}` | `inquiry_actions.rs` | Remove assignment |

### Public Submissions (`/api/v1/submit`)

| Method | Path | Handler file | Description |
|--------|------|-------------|-------------|
| POST | `/photo` | `submissions.rs` | Photo webapp multipart upload (no auth) |
| POST | `/mobile` | `submissions.rs` | Mobile app multipart upload (no auth) |

### Admin (`/api/v1/admin`)

| Method | Path | Handler file | Description |
|--------|------|-------------|-------------|
| GET | `/dashboard` | `admin.rs` | KPIs, recent activity, conflict dates |
| GET/POST | `/customers` | `admin_customers.rs` | List / create customers |
| GET/PATCH | `/customers/{id}` | `admin_customers.rs` | Detail / update customer |
| POST | `/customers/{id}/delete` | `admin_customers.rs` | Delete customer |
| PATCH | `/addresses/{id}` | `admin_customers.rs` | Update address fields |
| GET | `/emails` | `admin_emails.rs` | List email threads |
| GET | `/emails/{id}` | `admin_emails.rs` | Thread detail with messages |
| POST | `/emails/{id}/reply` | `admin_emails.rs` | Reply to thread |
| POST | `/emails/compose` | `admin_emails.rs` | Send new email |
| GET | `/users` | `admin.rs` | List admin users |
| GET | `/orders` | `admin.rs` | List orders |
| GET/POST | `/employees` | `admin.rs` | List / create employees |
| GET/PATCH | `/employees/{id}` | `admin.rs` | Detail / update employee |

## Orchestrator (`orchestrator.rs`)

Background task that receives events from the email agent's Telegram poller via an `mpsc` channel. Heavy lifting is delegated to service modules:

- **`telegram_service.rs`** — Sends formatted offers to Telegram, handles approve/edit/deny callbacks, LLM edit parsing
- **`email_dispatch.rs`** — Downloads PDF from S3, sends via SMTP on approval
- **`offer_pipeline.rs`** — Auto-generates offer after estimation completes

### Event Handling

| Event | Action | Delegated to |
|-------|--------|-------------|
| `InquiryComplete(inquiry)` | Create customer + addresses + inquiry → auto-generate offer → Telegram | `orchestrator.rs` |
| `OfferApprove(id)` | Download PDF from S3 → SMTP email to customer → mark sent | `email_dispatch.rs` |
| `OfferEdit(id)` | Enter edit mode, prompt Alex for instructions | `telegram_service.rs` |
| `OfferEditText(text)` | LLM parse instructions → regenerate offer with overrides → Telegram | `telegram_service.rs` |
| `OfferDeny(id)` | Mark offer rejected | `orchestrator.rs` |

## Offer Generation (`services/offer_builder.rs`)

`routes/offers.rs` is a thin re-export shim. All logic lives in `services/offer_builder.rs`.

### `build_offer_with_overrides()`

1. Fetch inquiry, customer, addresses from DB
2. Fetch latest volume estimation for detected items
3. Calculate pricing via `PricingEngine`
4. Apply overrides (persons, hours, price)
5. Build line items from services JSONB
6. Back-calculate rate if price overridden: `rate = (netto - other_items) / (persons × hours)`
7. Generate XLSX via `XlsxGenerator`
8. Convert to PDF via LibreOffice
9. Upload to S3, store offer in DB

### `build_line_items()`

Builds `Vec<OfferLineItem>` from `Services` struct, distance, and volume:
- Row 31: De/Montage (€50) — if `services.disassembly` or `services.assembly`
- Row 32: Halteverbotszone (€100) — count of parking ban locations (1-2)
- Row 33: Einpackservice (€30) — if `services.packing`
- Row 39: Transporter (€60) — 1 truck, 2 if volume > 30m³

## Canonical Response Builder (`services/inquiry_builder.rs`)

Single source of truth for building `InquiryResponse` and `InquiryListItem`:
- `build_inquiry_response(pool, inquiry_id)` — full detail with customer, addresses, estimation, items, offer
- `build_inquiry_list(pool, status, search, has_offer, limit, offset)` — paginated list

## Dependencies

All other crates, Axum (with multipart feature), SQLx, tower-http, bytes, lettre (SMTP).
