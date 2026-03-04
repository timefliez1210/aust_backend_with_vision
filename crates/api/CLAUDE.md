# crates/api — REST API, Routing & Offer Orchestration

HTTP server built on Axum 0.8. Defines all REST endpoints, request/response types, application state, and the offer orchestration pipeline.

## Key Files

- `src/routes/mod.rs` - Route tree assembly
- `src/routes/inquiries.rs` - Unified inquiry CRUD + estimation + offer generation
- `src/routes/estimates.rs` - Public image/video proxy + protected estimation CRUD (polling, delete)
- `src/routes/calendar.rs` - Calendar & booking endpoints
- `src/routes/admin.rs` - Admin panel (dashboard, customers, emails, users, orders)
- `src/routes/customer.rs` - Customer-facing endpoints (OTP auth, accept/reject)
- `src/routes/auth.rs` - Authentication (stub)
- `src/routes/offers.rs` - Offer generation helpers (`build_offer_with_overrides`, line items, XLSX/PDF)
- `src/services/inquiry_builder.rs` - Canonical `build_inquiry_response()` and `build_inquiry_list()`
- `src/orchestrator.rs` - Offer event handler: Telegram approval/edit loop, inquiry pipeline
- `src/state.rs` - `AppState` shared across handlers

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

| Method | Path | Description |
|--------|------|-------------|
| POST | `/` | Create inquiry (customer_email + addresses) → 201 |
| GET | `/` | List with filters: status, search, has_offer, limit, offset |
| GET | `/{id}` | Full detail with customer, addresses, estimation, items, offer |
| PATCH | `/{id}` | Update fields / transition status (validated by `can_transition_to()`) |
| DELETE | `/{id}` | Soft-delete → status=cancelled |
| GET | `/{id}/pdf` | Download latest active offer PDF |
| PUT | `/{id}/items` | Replace estimation items + recalculate volume |
| POST | `/{id}/estimate/{method}` | Trigger estimation (depth, video) |
| POST | `/{id}/generate-offer` | Generate/regenerate offer |
| GET | `/{id}/emails` | Email thread for this inquiry |

### Public Submissions (`/api/v1/submit`)

| Method | Path | Description |
|--------|------|-------------|
| POST | `/photo` | Photo webapp multipart upload (no auth) |
| POST | `/mobile` | Mobile app multipart upload (no auth) |

### Calendar (`/api/v1/calendar`)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/availability?date=` | Check availability + alternatives |
| GET | `/schedule?from=&to=` | Date range schedule (max 90 days) |
| POST | `/bookings` | Create booking (respects capacity) |
| GET | `/bookings/{id}` | Get booking |
| PATCH | `/bookings/{id}` | Update status (confirm/cancel) |
| PUT | `/capacity/{date}` | Override daily capacity |

### Admin (`/api/v1/admin`)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/dashboard` | KPIs, recent activity, conflict dates |
| GET/POST | `/customers` | List / create customers |
| GET/PATCH | `/customers/{id}` | Detail / update customer |
| POST | `/customers/{id}/delete` | Delete customer |
| PATCH | `/addresses/{id}` | Update address fields |
| GET | `/emails` | List email threads |
| GET | `/emails/{id}` | Thread detail with messages |
| POST | `/emails/{id}/reply` | Reply to thread |
| POST | `/emails/compose` | Send new email |
| GET | `/users` | List admin users |
| GET | `/orders` | List orders |

## Orchestrator (`orchestrator.rs`)

Background task that receives events from the email agent's Telegram poller via an `mpsc` channel.

### Event Handling

| Event | Action |
|-------|--------|
| `InquiryComplete(inquiry)` | Create customer + addresses + inquiry → auto-generate offer → Telegram |
| `OfferApprove(id)` | Download PDF from S3 → SMTP email to customer → mark sent |
| `OfferEdit(id)` | Enter edit mode, prompt Alex for instructions |
| `OfferEditText(text)` | LLM parse instructions → regenerate offer with overrides → Telegram |
| `OfferDeny(id)` | Mark offer rejected |

### Inquiry Pipeline (`handle_complete_inquiry`)

1. Upsert customer by email
2. Create origin/destination addresses (parse `"Straße 1, 31157 Sarstedt"` → street, city, postal)
3. Determine volume: use form-provided `volume_m3`, or rough estimate
4. Create inquiry with status `"estimated"`, services as JSONB
5. Store volume estimation with parsed items list in `result_data`
6. Trigger `try_auto_generate_offer()`

### Items List Parsing (`parse_items_list_text`)

Parses VolumeCalculator text format into structured items:
- Input: `"2x Sofa, Couch (0.80 m³)\n1x Schreibtisch (1.20 m³)"`
- Handles both newline-separated and comma-separated formats
- Extracts: quantity (`2x`), name, volume from parenthesized notation `(0.80 m³)`

### Services JSONB

Services stored as JSONB on `inquiries.services` (replaces comma-separated notes parsing):
```rust
pub struct Services {
    pub packing: bool,                  // Einpackservice
    pub assembly: bool,                 // Montage
    pub disassembly: bool,              // Demontage
    pub storage: bool,                  // Einlagerung
    pub disposal: bool,                 // Entsorgung
    pub parking_ban_origin: bool,       // Halteverbot Beladestelle
    pub parking_ban_destination: bool,  // Halteverbot Entladestelle
}
```

### LLM Edit Parsing (`llm_parse_edit_instructions`)

Sends Alex's message + current offer summary to LLM. The LLM returns JSON with overrides:
- `price_cents_netto` — bare prices default to brutto (÷1.19)
- `persons`, `hours`, `rate`, `volume_m3`

Falls back to regex-based `parse_edit_instructions()` if LLM fails.

## Offer Generation (`offers.rs`)

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

Replaces 3 duplicate implementations that existed in quotes.rs, admin.rs, and customer.rs.

## Dependencies

All other crates, Axum (with multipart feature), SQLx, tower-http, bytes, lettre (SMTP).
