# crates/api — REST API, Routing & Offer Orchestration

HTTP server built on Axum 0.8. Defines all REST endpoints, request/response types, application state, and the offer orchestration pipeline.

## Key Files

- `src/routes/mod.rs` - Route tree assembly
- `src/routes/estimates.rs` - Volume estimation endpoints (vision, inventory, depth-sensor)
- `src/routes/calendar.rs` - Calendar & booking endpoints
- `src/routes/quotes.rs` - Quote CRUD
- `src/routes/auth.rs` - Authentication (stub)
- `src/routes/distance.rs` - Distance calculation
- `src/routes/offers.rs` - Offer generation, line item building, XLSX/PDF pipeline
- `src/orchestrator.rs` - Offer event handler: Telegram approval/edit loop, inquiry→quote→offer pipeline
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

### Estimates (`/api/v1/estimates`)

| Method | Path | Description |
|--------|------|-------------|
| POST | `/vision` | Base64 image array → LLM vision analysis |
| POST | `/depth-sensor` | Multipart upload → 3D ML pipeline (falls back to LLM) |
| POST | `/inventory` | Manual inventory form → aggregated volume |
| GET | `/{id}` | Retrieve past estimation |

**depth-sensor flow**: Upload images → store in S3 → call VisionServiceClient → if unavailable, fallback to VisionAnalyzer (LLM) → store result → update quote.

### Calendar (`/api/v1/calendar`)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/availability?date=` | Check availability + alternatives |
| GET | `/schedule?from=&to=` | Date range schedule (max 90 days) |
| POST | `/bookings` | Create booking (respects capacity) |
| GET | `/bookings/{id}` | Get booking |
| PATCH | `/bookings/{id}` | Update status (confirm/cancel) |
| PUT | `/capacity/{date}` | Override daily capacity |

### Offers (`/api/v1/offers`)

| Method | Path | Description |
|--------|------|-------------|
| POST | `/generate` | Generate offer from quote |
| GET | `/{id}` | Get offer |
| GET | `/{id}/pdf` | Download offer PDF |

## Orchestrator (`orchestrator.rs`)

Background task that receives events from the email agent's Telegram poller via an `mpsc` channel.

### Event Handling

| Event | Action |
|-------|--------|
| `InquiryComplete(inquiry)` | Create customer + addresses + quote → auto-generate offer → Telegram |
| `OfferApprove(id)` | Download PDF from S3 → SMTP email to customer → mark sent |
| `OfferEdit(id)` | Enter edit mode, prompt Alex for instructions |
| `OfferEditText(text)` | LLM parse instructions → regenerate offer with overrides → Telegram |
| `OfferDeny(id)` | Mark offer rejected |

### Inquiry → Quote Pipeline (`handle_complete_inquiry`)

1. Upsert customer by email
2. Create origin/destination addresses (parse `"Straße 1, 31157 Sarstedt"` → street, city, postal)
3. Determine volume: use form-provided `volume_m3`, or rough estimate from notes
4. Create quote with status `"volume_estimated"`, notes from services/parking bans
5. Store volume estimation with parsed items list in `result_data`
6. Trigger `try_auto_generate_offer()`

### Items List Parsing (`parse_items_list_text`)

Parses VolumeCalculator text format into structured items:
- Input: `"2x Sofa, Couch (0.80 m³)\n1x Schreibtisch (1.20 m³)"`
- Handles both newline-separated and comma-separated formats
- Extracts: quantity (`2x`), name, volume from parenthesized notation `(0.80 m³)`

### Quote Notes Format

Services and metadata stored as comma-separated text in `quotes.notes`:
- `"Auszug: 1. Stock, Einzug: 3. Stock, Halteverbot Auszug, Halteverbot Einzug, Verpackungsservice, Montage, Demontage"`

### LLM Edit Parsing (`llm_parse_edit_instructions`)

Sends Alex's message + current offer summary to LLM. The LLM returns JSON with overrides:
- `price_cents_netto` — bare prices default to brutto (÷1.19)
- `persons`, `hours`, `rate`, `volume_m3`

Falls back to regex-based `parse_edit_instructions()` if LLM fails.

## Offer Generation (`offers.rs`)

### `build_offer_with_overrides()`

1. Fetch quote, customer, addresses from DB
2. Fetch latest volume estimation for detected items
3. Calculate pricing via `PricingEngine`
4. Apply overrides (persons, hours, price)
5. Build line items from quote notes
6. Back-calculate rate if price overridden: `rate = (netto - other_items) / (persons × hours)`
7. Generate XLSX via `XlsxGenerator`
8. Convert to PDF via LibreOffice
9. Upload to S3, store offer in DB

### `build_line_items()`

Builds `Vec<OfferLineItem>` from quote notes, distance, and volume:
- Row 31: De/Montage (€50) — if notes contain "montage" or "demontage"
- Row 32: Halteverbotszone (€100) — count of parking ban locations (1-2)
- Row 33: Einpackservice (€30) — if notes contain "verpackungsservice" or "einpackservice"
- Row 39: Transporter (€60) — 1 truck, 2 if volume > 30m³
- Row 42: Anfahrt (€30 + km×1.50) — if distance > 0

## Dependencies

All other crates, Axum (with multipart feature), SQLx, tower-http, bytes, lettre (SMTP).
