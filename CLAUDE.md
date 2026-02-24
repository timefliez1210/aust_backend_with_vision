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
| Vision ML | Grounding DINO + SAM 2 + Depth Anything V2 + MASt3R + Open3D |
| Vision Infra | Modal (serverless GPU, L4) |
| Maps/Routing | OpenRouteService |
| Email | IMAP/SMTP via lettre + async-imap |
| Approval UI | Telegram Bot (human-in-the-loop) |
| PDF | XLSX template (umya-spreadsheet) → LibreOffice PDF conversion |

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
├── offer-generator/      # Pricing engine + XLSX template → PDF generation
└── calendar/             # Booking management + capacity tracking

services/
└── vision/               # Python ML service (GPU) - 3D volume estimation
    ├── app/              # FastAPI application
    └── modal_app.py      # Modal serverless deployment
```

## Key Files

- `src/main.rs` - Application entry point, config loading, service wiring
- `config/default.toml` - Default configuration
- `migrations/*.sql` - Database schema (initial + calendar + address_floor)
- `templates/Angebot_Vorlage.xlsx` - XLSX offer template (embedded at compile time)
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

## Deployment (Production)

The backend runs as a systemd service (`aust-backend.service`) with a release binary.

### Deploy Workflow

```bash
# Full deploy: backup DB → git pull → cargo build --release → restart service → health check
./scripts/deploy.sh
```

The deploy script:
1. Checks Docker, PostgreSQL, and cargo are available
2. Aborts if there are uncommitted local changes
3. Backs up the database (gzipped pg_dump)
4. Pulls latest changes (`git pull --ff-only`)
5. Builds release binary
6. Restarts `aust-backend` systemd service
7. Verifies health endpoint (3 retries)

### Database Backups

**Pre-deploy backup**: Every deploy automatically backs up the DB first.

**Daily backup**: systemd timer runs at 03:00 daily.

```bash
# Manual backup
./scripts/backup-db.sh

# Install daily backup timer
sudo cp scripts/aust-backup.service scripts/aust-backup.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now aust-backup.timer

# Verify timer is scheduled
systemctl list-timers aust-backup
```

Backups are stored in `backups/db/` as `aust_backend_YYYYMMDD_HHMMSS.sql.gz`. The last 30 backups are kept, older ones are rotated.

### Useful Commands

```bash
# Service status and logs
sudo systemctl status aust-backend
journalctl -u aust-backend -f              # follow logs
journalctl -u aust-backend -n 100          # last 100 lines
journalctl -u aust-backend --since today   # today's logs

# Manual restart (without full deploy)
sudo systemctl restart aust-backend

# Check health
curl http://localhost:8080/health
curl http://localhost:8080/ready
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
- `POST /api/v1/estimates/video` - Video 3D reconstruction pipeline (multipart upload, 600s timeout)
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

## Data Flow — Quote-to-Offer Pipeline

Four input sources feed into the pipeline:

| Source | Entry Point | Volume Data | Status |
|--------|------------|-------------|--------|
| A. Kontakt form | Email → email agent | None (general inquiry) | Working |
| B. Kostenloses Angebot form | Email → email agent (JSON attachment) | VolumeCalculator items list | Working |
| C. Photo webapp | Direct API POST | Vision pipeline (ML) | Not yet implemented |
| D. Mobile app | Direct API POST | Depth sensor + AR | Not yet implemented |
| E. Video upload | Admin dashboard → API | Video 3D reconstruction (MASt3R + SAM 2) | Implemented |

```
                   ┌─── A. Kontakt form (email) ────────┐
                   ├─── B. Angebot form (email+JSON) ───┤
Input Sources ─────┤                                    ├─→ MovingInquiry
                   ├─── C. Photo webapp (API) ──────────┤
                   └─── D. Mobile app (API) ────────────┘
                                    ↓
            Email Agent: parse JSON attachment / email text
            → merge into MovingInquiry
            → if complete: forward to orchestrator
                                    ↓
            Orchestrator (orchestrator.rs):
            → Create customer (by email, upsert)
            → Create origin/destination addresses
            → Create quote with volume + notes
            → Store volume estimation (parsed items)
            → Auto-generate offer
                                    ↓
            Offer Generation (offers.rs):
            → PricingEngine: persons, hours, rate from volume/distance/floors
            → build_line_items(): transport, Halteverbot, De/Montage, Einpackservice, Anfahrt
            → XlsxGenerator: fill template → XLSX
            → LibreOffice: XLSX → PDF
            → Upload PDF to S3
            → Store offer in DB
                                    ↓
            Telegram: PDF sent with ✅ Senden / ✏️ Bearbeiten / ❌ Verwerfen
                                    ↓
            ┌── ✅ Approve → download PDF from S3 → SMTP email to customer
            ├── ✏️ Edit → Alex types natural language → LLM parses overrides
            │        → regenerate offer with overrides → re-send to Telegram
            └── ❌ Deny → mark offer rejected
```

### Telegram Edit Flow

Alex presses ✏️ and types natural language instructions. The LLM parses them into numeric overrides:
- **Price** (default brutto): `"350 Euro"` / `"mach auf 350"` → netto = 350/1.19 = €294.12
- **Persons**: `"4 Helfer"` → persons=4
- **Hours**: `"6 Stunden"` → hours=6
- **Rate**: `"Stundensatz 35"` → rate=35.0
- **Volume**: `"15 m³"` → volume=15.0

Rate back-calculation when price is overridden:
```
rate = (target_netto - sum_of_non_labor_line_items) / (persons × hours)
```

### XLSX Template Mapping

The offer uses an XLSX template (`templates/Angebot_Vorlage.xlsx`). Key rows:

| Row | Column D | Column E | Column F | Description |
|-----|----------|----------|----------|-------------|
| 31 | De/Montage | quantity | €50/unit | Furniture assembly if requested |
| 32 | Halteverbotszone | count (1-2) | €100/zone | Parking ban zones |
| 33 | Umzugsmaterial | quantity | €30/unit | Packing service if requested |
| 38 | N Umzugshelfer | hours | rate/hr | Labor (G38 = E38 × F38 × J50) |
| 39 | Transporter | truck count | €60/truck | 1 truck, 2 if >30m³ |
| 42 | Anfahrt/Abfahrt | quantity | distance-based | €30 + €1.50/km |
| 44 | | | | **Netto total** (sum of G31:G42) |

The generator clears ALL template preset values (rows 31-42 except 38) before writing, ensuring only explicit line items contribute to the total.

PDF output: columns A-H only (print area set to `$A$1:$H$120`), internal calculation columns I-P excluded.

### Items Sheet ("Erfasste Gegenstände")

For form submissions with a VolumeCalculator items list, a second sheet is added with:
- Parsed item name, volume (m³), dimensions, confidence
- Total volume row at bottom
- Items are parsed from text format: `"2x Sofa, Couch (0.80 m³)"` → name, quantity, volume

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

### Direct API Endpoints (Sources C + D)
- [ ] `POST /api/v1/inquiries/photo` — multipart form + photos for webapp
- [ ] `POST /api/v1/inquiries/mobile` — multipart form + photos + depth maps for mobile app
- [ ] Wire both into vision pipeline → offer generation → Telegram approval

### Missing Offer Data
- [ ] Auto-trigger distance calculation when addresses exist (currently `distance_km: 0.0`)
- [ ] Add elevator field to addresses table and forms
- [ ] Salutation detection: currently hardcodes "Herrn", should detect from name or store

### Authentication
- [ ] Implement proper JWT token generation in `crates/api/src/routes/auth.rs`
- [ ] Implement password hashing with Argon2
- [ ] Add JWT validation middleware
- [ ] Protect API routes with auth middleware

## Medium Priority

### Volume Estimation
- [ ] Fine-tune 3D pipeline with real production photos
- [ ] Add item catalog volume overrides for high-confidence standard items
- [ ] Implement ensemble mode (combine LLM + 3D estimates)
- [ ] Improve cross-image deduplication with better feature extraction

### Pricing Engine
- [ ] Make pricing configurable via database (currently hardcoded rates)
- [ ] Add seasonal/weekend/holiday pricing (Saturday surcharge exists: +€50)
- [ ] Store services as JSONB on quotes instead of comma-separated text in `notes`

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
- [x] Database backup strategy
- [x] Deploy script with pre-deploy backup

## Technical Debt

- [ ] Remove unused `patch` import in `quotes.rs`
- [ ] Use `status` field in `ListQuotesQuery` for filtering
- [ ] Extract duplicate `QuoteRow` to shared location
- [ ] Fix `run_agent.rs` example (missing 4th arg to `EmailProcessor::new()`)
