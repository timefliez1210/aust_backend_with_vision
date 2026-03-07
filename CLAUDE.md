# AUST Backend

A modular Rust backend for automating moving company operations - from initial customer email contact through volume estimation to automated offer generation.

## Project Overview

**Purpose**: Automate the quote-to-offer pipeline for a moving company (German market, German language)

**Architecture**: Modular monolith with 9 crates + 1 Python sidecar service, designed for future microservices extraction

**Scale**: Single tenant, single region, <1000 requests/day

## Documentation

Full user-facing docs live in the repository root:

- **[README.md](README.md)** — Quick-start, architecture overview, prerequisites, environment variables, deployment
- **[docs/API.md](docs/API.md)** — Complete HTTP endpoint reference (request/response shapes, curl examples, business rules, status codes)
- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — Crate dependency graph, full pipeline mermaid diagrams, key data types, XLSX template layout, external service map
- **[docs/DEBUGGING.md](docs/DEBUGGING.md)** — Recurring failure patterns with root causes and fixes (read before investigating unexpected behavior)

**Keep these files current.**
- Endpoint added/removed/changed → update `docs/API.md`
- Deployment steps, prerequisites, or config keys changed → update `README.md`
- New recurring bug found and fixed → add an entry to `docs/DEBUGGING.md`
- Crate added, removed, or pipeline flow changed → update `docs/ARCHITECTURE.md`

## Tech Stack

| Component | Technology |
|-----------|------------|
| Language | Rust 2021 |
| Web Framework | Axum 0.8 |
| Database | PostgreSQL 16 + SQLx |
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
- `docker/docker-compose.yml` - Local dev infrastructure (Postgres, MinIO, Vision)
- `docker/docker-compose.gpu.yml` - GPU override for vision service
- `services/vision/modal_app.py` - Modal deployment for vision service

## Repository Structure

The frontend (SvelteKit admin dashboard) lives at `frontend/` as a git submodule pointing to
`git@github.com:timefliez1210/aust-umzuege.git`.

```bash
# Clone everything in one go
git clone --recursive git@github.com:timefliez1210/aust_backend.git

# If you already cloned without --recursive
git submodule update --init --recursive

# Pull latest frontend commit and update the pinned ref
git submodule update --remote frontend
git add frontend && git commit -m "chore: update frontend submodule"
```

Frontend code lives at `frontend/` — all staging scripts, Playwright tests, and Docker builds
reference this path.

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

## Staging Environment

An isolated staging stack runs on different ports so it never conflicts with the production service.

```bash
# First time: copy secrets template
cp docker/.env.staging.example docker/.env.staging
# Edit docker/.env.staging — add LLM/maps keys if you need real ORS calls

# Start all containers and wait for health
./scripts/staging.sh up

# Run all tests: backend unit tests + frontend vitest + HTTP integration tests
./scripts/staging.sh test

# Tail logs
./scripts/staging.sh logs

# Tear down (keep volumes)
./scripts/staging.sh down

# Full reset (destroy volumes)
./scripts/staging.sh clean
```

### Staging ports

| Service | Port |
|---------|------|
| Backend API | 8099 |
| Frontend (nginx) | 4173 |
| PostgreSQL | 5435 |
| MinIO API | 9010 |
| MinIO UI | 9011 |
| Mailpit (web) | 8025 |

Mailpit catches all outbound emails — check `http://localhost:8025` instead of a real inbox.

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

### Inquiries (main resource — replaces /quotes, /offers, /estimates)
- `POST /api/v1/inquiries` - Create inquiry (customer_email + addresses)
- `GET /api/v1/inquiries` - List inquiries (filters: status, search, has_offer, limit, offset)
- `GET /api/v1/inquiries/{id}` - Full detail with customer, addresses, estimation, items, offer
- `PATCH /api/v1/inquiries/{id}` - Update fields / transition status
- `DELETE /api/v1/inquiries/{id}` - Soft-delete (→ cancelled)
- `GET /api/v1/inquiries/{id}/pdf` - Download active offer PDF
- `PUT /api/v1/inquiries/{id}/items` - Edit estimation items
- `POST /api/v1/inquiries/{id}/estimate/{method}` - Trigger estimation (depth, video)
- `POST /api/v1/inquiries/{id}/generate-offer` - Generate/regenerate offer
- `GET /api/v1/inquiries/{id}/emails` - Email thread

### Public Submissions (no auth)
- `POST /api/v1/submit/photo` - Photo webapp multipart upload
- `POST /api/v1/submit/mobile` - Mobile app multipart upload

### Media (public, no auth)
- `GET /api/v1/estimates/images/{key}` - Public image/video proxy
- `GET /api/v1/media/{key}` - Public image/video proxy (alias)

### Volume Estimation (protected, legacy — to be migrated into `/inquiries`)
- `POST /api/v1/estimates/vision` - LLM image analysis (base64 JSON)
- `POST /api/v1/estimates/depth-sensor` - 3D ML pipeline (multipart upload)
- `POST /api/v1/estimates/video` - Video 3D reconstruction (multipart upload)
- `POST /api/v1/estimates/inventory` - Manual inventory form
- `GET /api/v1/estimates/{id}` - Get estimation (used for polling)
- `DELETE /api/v1/estimates/{id}` - Delete estimation + S3 cleanup

### Calendar
- `GET /api/v1/calendar/availability?date=YYYY-MM-DD` - Check date + alternatives
- `GET /api/v1/calendar/schedule?from=...&to=...` - Schedule with capacity (max 90 days)
- `POST /api/v1/calendar/bookings` - Create booking
- `GET /api/v1/calendar/bookings/{id}` - Get booking
- `PATCH /api/v1/calendar/bookings/{id}` - Update status (confirm/cancel)
- `PUT /api/v1/calendar/capacity/{date}` - Override daily capacity

### Customer (OTP auth)
- `POST /api/v1/customer/auth/request` - Request OTP code
- `POST /api/v1/customer/auth/verify` - Verify OTP, get session token
- `GET /api/v1/customer/me` - Customer profile
- `GET /api/v1/customer/inquiries` - List customer's inquiries
- `GET /api/v1/customer/inquiries/{id}` - Inquiry detail (ownership-validated)
- `POST /api/v1/customer/inquiries/{id}/accept` - Accept offer
- `POST /api/v1/customer/inquiries/{id}/reject` - Reject offer
- `GET /api/v1/customer/inquiries/{id}/pdf` - Download offer PDF

### Admin (dashboard, customers, employees, emails, users)
- `GET /api/v1/admin/dashboard` - KPIs and recent activity
- `GET/POST /api/v1/admin/customers` - List / create customers
- `GET/PATCH /api/v1/admin/customers/{id}` - Detail / update customer
- `PATCH /api/v1/admin/addresses/{id}` - Update address
- `GET/POST /api/v1/admin/employees` - List / create employees
- `GET/PATCH /api/v1/admin/employees/{id}` - Detail / update employee
- `POST /api/v1/admin/employees/{id}/delete` - Soft-delete (active=false)
- `GET /api/v1/admin/employees/{id}/hours` - Monthly hours summary
- `GET /api/v1/admin/emails` - List email threads
- `GET /api/v1/admin/emails/{id}` - Thread detail
- `GET /api/v1/admin/users` - List users

### Inquiry Employee Assignments
- `GET /api/v1/inquiries/{id}/employees` - List assigned employees
- `POST /api/v1/inquiries/{id}/employees` - Assign employee
- `PATCH /api/v1/inquiries/{id}/employees/{emp_id}` - Update hours/notes
- `DELETE /api/v1/inquiries/{id}/employees/{emp_id}` - Remove assignment

## Data Flow — Quote-to-Offer Pipeline

Four input sources feed into the pipeline:

| Source | Entry Point | Volume Data | Status |
|--------|------------|-------------|--------|
| A. Kontakt form | Email → email agent | None (general inquiry) | Working |
| B. Kostenloses Angebot form | Email → email agent (JSON attachment) | VolumeCalculator items list | Working |
| C. Photo webapp | `POST /api/v1/submit/photo` | Vision pipeline (ML) | Implemented |
| D. Mobile app | `POST /api/v1/submit/mobile` | Depth sensor + AR | Implemented |
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
            → Create inquiry with volume + services JSONB
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

The offer uses an XLSX template (`templates/Angebot_Vorlage.xlsx`). Line items are written dynamically into rows 31–42 (max 12 items). Item order:

| Slot | Item | Notes |
|------|------|-------|
| 1 | **Fahrkostenpauschale** | Always first. `flat_total` set via ORS round-trip (depot→origin→[stop]→dest→depot). E/F blank, G = flat amount. |
| 2 | Demontage | Only if `services.disassembly` is true |
| 3 | Montage | Only if `services.assembly` is true |
| 4 | Halteverbotszone | remark = Beladestelle / Entladestelle / both; qty = number of zones from `services.parking_ban_origin/destination` |
| 5 | Umzugsmaterial | Only if `services.packing` is true |
| 6+ | Manual items | Möbellift, Kleiderboxen, Kartons — passed via `overrides.line_items` only |
| Labor | N Umzugshelfer | `is_labor=true`. G = E × F × J50 (J50 holds persons count) |
| Last | Nürnbergerversicherung | Always last; qty=1, price=0, total=0 |
| **G44** | | **Netto total** — `SUM(G31:G42)` |

The generator hides ALL template rows 31–42 first, then writes only the active items starting at row 31. Rows beyond the item count stay hidden.

**Removed items** (no longer generated): 3,5t Transporter, Anfahrt/Abfahrt (€30+€1.50/km).

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
- **Function-level doc comments required**: Every `pub` function, `pub struct`, `pub enum`, and trait method must have a `///` doc comment. Add them to non-public functions too when the logic is non-obvious. Use this format:

  ```rust
  /// Brief one-line description.
  ///
  /// **Caller**: [module or function that calls this]
  /// **Why**: [business reason — why this function needs to exist]
  ///
  /// # Parameters
  /// - `param_name` — what it represents
  ///
  /// # Returns
  /// What is returned and under what conditions
  ///
  /// # Errors
  /// When this returns `Err` (omit section if the function is infallible)
  ///
  /// # Math
  /// Include only when there is a real formula, e.g.:
  /// `persons = ceil(volume_m3 / 5.0)`
  /// `rate = (target_netto - non_labor_netto) / (persons × hours)`
  ```

## Database Schema

See `migrations/` for full schema.

Key tables:
- `customers` - Contact information
- `addresses` - Origin/destination with geocoding
- `inquiries` - Unified inquiry lifecycle (was `quotes`); status + services JSONB + lifecycle timestamps
- `volume_estimations` - Volume calculation results (FK: `inquiry_id`)
- `offers` - Generated offers with pricing (FK: `inquiry_id`)
- `email_threads` / `email_messages` - Email conversation tracking (FK: `inquiry_id`)
- `calendar_bookings` - Moving date bookings with status (FK: `inquiry_id`)
- `calendar_capacity_overrides` - Date-specific capacity limits
- `users` - Admin users
- `employees` - Employee profiles (salutation, name, email, phone, monthly_hours_target, active)
- `inquiry_employees` - Junction: employee ↔ inquiry assignment (planned_hours, actual_hours, notes)

### Inquiry Status State Machine

```
PRE-SALES:
  pending → info_requested → estimating → estimated → offer_ready → offer_sent
    → accepted | rejected | expired | cancelled

OPERATIONS:
  scheduled → completed → invoiced → paid
```

Enforced by `InquiryStatus::can_transition_to()` in `crates/core/src/models/inquiry.rs`.

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
- [x] `POST /api/v1/submit/photo` — multipart form + photos for webapp
- [x] `POST /api/v1/submit/mobile` — multipart form + photos + depth maps for mobile app
- [x] Wire both into vision pipeline → offer generation → Telegram approval

### Missing Offer Data
- [x] Auto-trigger distance calculation when addresses exist (`try_auto_generate_offer` now runs ORS when `distance_km=0`)
- [x] Add elevator field to addresses table (migration done; form wiring for email path still partial)
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
- [x] Store services as JSONB on inquiries instead of comma-separated text in `notes`

### Distance Calculator
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
- [x] Unit tests for pricing engine and volume calculator
- [x] Integration tests with test database
- [x] API endpoint tests

### DevOps
- [ ] GitHub Actions CI/CD
- [x] Database backup strategy
- [x] Deploy script with pre-deploy backup

## Technical Debt

- [x] Remove legacy `/quotes`, `/offers`, `/distance` route stubs (superseded by `/inquiries`)
- [x] Delete old `crates/api/src/routes/quotes.rs` and `distance.rs`
- [x] Delete orphaned frontend `/admin/offers` pages (offers embedded in inquiry detail)
- [x] Remove dead `status_sync.rs` (sync_quote_* functions never wired; offer status synced inline in customer.rs)
- [ ] Migrate `/estimates` protected handlers into inquiry-level endpoints (currently re-mounted for frontend polling + delete)
- [ ] Add multipart upload support to `trigger_estimate` for vision/depth/video methods (currently only inventory works inline)
- [ ] Restore `/distance/calculate` endpoint or embed route geometry in inquiry response (route map broken)
- [ ] Rename `update_quote_volume` → `update_inquiry_volume` in `services/db.rs`
