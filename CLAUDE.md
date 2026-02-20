# AUST Backend

A modular Rust backend for automating moving company operations - from initial customer email contact through volume estimation to automated offer generation.

## Project Overview

**Purpose**: Automate the quote-to-offer pipeline for a moving company (Austrian market, German language)

**Architecture**: Modular monolith with 9 crates + 1 Python sidecar service, designed for future microservices extraction

**Scale**: Single tenant, single region, <1000 requests/day

## Tech Stack

| Component | Technology |
|-----------|------------|
| Language | Rust 2021 |
| Web Framework | Axum 0.8 |
| Database | PostgreSQL 16 + SQLx |
| Cache/Queue | Redis |
| Object Storage | S3-compatible (MinIO for dev) |
| LLM | Pluggable: Claude, OpenAI, Ollama |
| Vision ML | Grounding DINO + SAM + Depth Anything V2 + Open3D |
| Vision Infra | Modal (serverless GPU, T4) |
| Maps/Routing | OpenRouteService |
| Email | IMAP/SMTP via lettre + async-imap |
| Approval UI | Telegram Bot (human-in-the-loop) |
| PDF | Typst |

## Project Structure

```
crates/
├── core/                 # Domain models, config, shared errors
├── api/                  # REST API, routes, middleware (Axum)
├── llm-providers/        # LLM abstraction (Claude, OpenAI, Ollama)
├── storage/              # File storage abstraction (S3, local)
├── email-agent/          # Email processing + Telegram approval workflow
├── volume-estimator/     # Volume calculation (LLM vision + external service client)
├── distance-calculator/  # Geocoding + multi-stop route calculation
├── offer-generator/      # Pricing engine + PDF generation (Typst)
└── calendar/             # Booking management + capacity tracking

services/
└── vision/               # Python ML service (GPU) - 3D volume estimation
    ├── app/              # FastAPI application
    └── modal_app.py      # Modal serverless deployment
```

## Key Files

- `src/main.rs` - Application entry point, config loading, service wiring
- `config/default.toml` - Default configuration
- `migrations/*.sql` - Database schema (initial + calendar)
- `docker/docker-compose.yml` - Local dev infrastructure (Postgres, Redis, MinIO, Vision)
- `docker/docker-compose.gpu.yml` - GPU override for vision service
- `services/vision/modal_app.py` - Modal deployment for vision service

## Running Locally

```bash
# Start PostgreSQL, Redis, MinIO
cd docker && docker-compose up -d

# Configure environment
cp .env.example .env
# Edit .env with your API keys (LLM, Maps, Telegram)

# Run migrations and start server
cargo run
```

Server runs on `http://localhost:8080`

### Vision Service (Modal)

```bash
# Deploy to Modal (serverless GPU)
modal deploy services/vision/modal_app.py

# Test
curl https://crfabig--aust-vision-serve.modal.run/health
curl -X POST https://crfabig--aust-vision-serve.modal.run/estimate/upload \
  -F "job_id=test" -F "images=@room.jpg"
```

## API Endpoints

### Health
- `GET /health` - Liveness check
- `GET /ready` - Readiness check (includes DB status)

### Auth (Admin only)
- `POST /api/v1/auth/login` - Login
- `POST /api/v1/auth/refresh` - Refresh token

### Quotes
- `POST /api/v1/quotes` - Create quote
- `GET /api/v1/quotes` - List quotes
- `GET /api/v1/quotes/{id}` - Get quote
- `PATCH /api/v1/quotes/{id}` - Update quote

### Volume Estimation
- `POST /api/v1/estimates/vision` - LLM image analysis (base64 JSON)
- `POST /api/v1/estimates/depth-sensor` - 3D ML pipeline (multipart upload, falls back to LLM)
- `POST /api/v1/estimates/inventory` - Manual inventory form
- `GET /api/v1/estimates/{id}` - Get estimation

### Calendar
- `GET /api/v1/calendar/availability?date=YYYY-MM-DD` - Check date + alternatives
- `GET /api/v1/calendar/schedule?from=...&to=...` - Schedule with capacity (max 90 days)
- `POST /api/v1/calendar/bookings` - Create booking
- `GET /api/v1/calendar/bookings/{id}` - Get booking
- `PATCH /api/v1/calendar/bookings/{id}` - Update status (confirm/cancel)
- `PUT /api/v1/calendar/capacity/{date}` - Override daily capacity

### Distance
- `POST /api/v1/distance/calculate` - Multi-stop route calculation

### Offers
- `POST /api/v1/offers/generate` - Generate offer from quote
- `GET /api/v1/offers/{id}` - Get offer
- `GET /api/v1/offers/{id}/pdf` - Download offer PDF

## Data Flow

```
Customer Email → Email Agent → Parse Intent (LLM)
                                    ↓
                            Create/Update Quote
                                    ↓
              ┌─────────────────────┼─────────────────────┐
              ↓                     ↓                     ↓
    Volume Estimator        Calendar Service      Distance Calculator
    ┌─────────────────┐     (Availability +       (Geocoding + Routing)
    │ 3D ML Pipeline  │      Booking)
    │ (Modal GPU)     │
    │   ↓ fallback    │
    │ LLM Vision      │
    └─────────────────┘
              ↓                     ↓                     ↓
              └─────────────────────┼─────────────────────┘
                                    ↓
                            Offer Generator
                            (Pricing + PDF)
                                    ↓
                    Telegram → Alex approves → Email sent
```

## Configuration

Environment variables override config files. Format: `AUST__SECTION__KEY`

Key sections in `config/default.toml`:
- `[server]` - host, port
- `[database]` - PostgreSQL URL
- `[redis]` - Redis URL
- `[storage]` - S3/MinIO settings
- `[email]` - IMAP/SMTP settings
- `[llm]` - Provider selection + API keys
- `[maps]` - OpenRouteService API key
- `[telegram]` - Bot token + admin chat ID
- `[calendar]` - Default capacity, alternatives count
- `[vision_service]` - Enabled flag, base URL, timeout

Examples:
- `AUST__DATABASE__URL=postgres://...`
- `AUST__LLM__CLAUDE__API_KEY=sk-...`
- `AUST__VISION_SERVICE__ENABLED=true`
- `AUST__VISION_SERVICE__BASE_URL=https://crfabig--aust-vision-serve.modal.run`

## Code Conventions

- **German for user-facing content**: Error messages, email responses, offer letters, Telegram messages
- **English for code**: Variables, functions, comments
- **UUIDs**: Use v7 (time-ordered) for new records
- **Dates**: Always UTC in database, convert for display
- **Money**: Store as cents (i64), currency code separate
- **Status enums**: Store as lowercase strings in DB
- **Traits for abstraction**: `LlmProvider`, `StorageProvider` - pluggable implementations
- **Factory functions**: `create_provider()` for centralized instantiation
- **Service/Repository pattern**: Business logic in service layer, SQL in repository layer

## Database Schema

See `migrations/` for full schema.

Key tables:
- `customers` - Contact information
- `addresses` - Origin/destination with geocoding
- `quotes` - Quote requests with status tracking
- `volume_estimations` - Volume calculation results (method: vision/inventory/depth_sensor)
- `offers` - Generated offers with pricing
- `email_threads` / `email_messages` - Email conversation tracking
- `calendar_bookings` - Moving date bookings with status
- `calendar_capacity_overrides` - Date-specific capacity limits
- `users` - Admin users

## LLM Provider Notes

The system supports multiple LLM providers:

- **Claude**: Best for German language, vision capabilities (primary)
- **OpenAI**: Alternative, good vision support
- **Ollama**: Local/self-hosted, privacy-focused

Switch providers via `AUST__LLM__DEFAULT_PROVIDER` (claude/openai/ollama)

---

# TODOs

## High Priority

### Authentication
- [ ] Implement proper JWT token generation in `crates/api/src/routes/auth.rs`
- [ ] Implement password hashing with Argon2
- [ ] Add JWT validation middleware
- [ ] Protect API routes with auth middleware

### PDF Generation
- [ ] Finalize Typst offer letter template (German)
- [ ] Add company logo/branding
- [ ] Store generated PDFs in S3

## Medium Priority

### Volume Estimation
- [ ] Fine-tune 3D pipeline with real production photos
- [ ] Add item catalog volume overrides for high-confidence standard items
- [ ] Implement ensemble mode (combine LLM + 3D estimates)
- [ ] Improve cross-image deduplication with better feature extraction

### Pricing Engine
- [ ] Make pricing configurable via database
- [ ] Add seasonal/weekend/holiday pricing
- [ ] Add floor/elevator surcharges

### Distance Calculator
- [ ] Add result caching in Redis
- [ ] Add travel time estimation

## Low Priority

### API
- [ ] Add OpenAPI/Swagger documentation
- [ ] Add rate limiting middleware
- [ ] Add pagination metadata

### Observability
- [ ] Structured logging with request IDs
- [ ] Prometheus metrics endpoint

### Testing
- [ ] Unit tests for pricing engine and volume calculator
- [ ] Integration tests with test database
- [ ] API endpoint tests

### DevOps
- [ ] GitHub Actions CI/CD
- [ ] Database backup strategy

## Technical Debt

- [ ] Remove unused `patch` import in `quotes.rs`
- [ ] Use `status` field in `ListQuotesQuery` for filtering
- [ ] Extract duplicate `QuoteRow` to shared location
