# crates/api — REST API & Routing

HTTP server built on Axum 0.8. Defines all REST endpoints, request/response types, and application state.

## Key Files

- `src/routes/mod.rs` - Route tree assembly
- `src/routes/estimates.rs` - Volume estimation endpoints (vision, inventory, depth-sensor)
- `src/routes/calendar.rs` - Calendar & booking endpoints
- `src/routes/quotes.rs` - Quote CRUD
- `src/routes/auth.rs` - Authentication (stub)
- `src/routes/distance.rs` - Distance calculation
- `src/routes/offers.rs` - Offer generation
- `src/state.rs` - `AppState` shared across handlers

## AppState

```rust
pub struct AppState {
    pub db: PgPool,
    pub llm: Arc<dyn LlmProvider>,
    pub storage: Arc<dyn StorageProvider>,
    pub calendar_service: Arc<CalendarService>,
    pub vision_service: Option<VisionServiceClient>,
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

## Dependencies

All other crates, Axum (with multipart feature), SQLx, tower-http, bytes.
