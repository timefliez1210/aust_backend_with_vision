# AUST Backend

A modular Rust backend that automates the quote-to-offer pipeline for a moving company operating in the Austrian/German market. It ingests customer inquiries from email and direct API calls, estimates moving volume via LLM vision or a 3D ML pipeline, generates priced PDF offers, and sends them to an admin for approval via Telegram before delivering them to the customer by email.

## Architecture

```
                   +--------------------------+
    Customer email |  email-agent             |
    Form JSON      |  (IMAP poll, parse,       |
    Photo API      |   Telegram approval loop) |
    Mobile API     +-----------+--------------+
                               |
                               v  MovingInquiry event
                   +-----------+--------------+
                   |  orchestrator.rs          |
                   |  (upsert customer,        |
                   |   create addresses,       |
                   |   create quote)           |
                   +-----------+--------------+
                               |
                               v
          +--------------------+--------------------+
          |                    |                    |
          v                    v                    v
  volume-estimator    distance-calculator    offer-generator
  (LLM vision /       (ORS geocode +        (PricingEngine +
   3D ML service)      routing)              XLSX template ->
                                             LibreOffice PDF)
                               |
                               v
                   +-----------+--------------+
                   |  storage (S3 / local)    |
                   |  PDF uploaded, key stored |
                   +-----------+--------------+
                               |
                               v
                   +-----------+--------------+
                   |  Telegram bot             |
                   |  (Senden / Bearbeiten /   |
                   |   Verwerfen)             |
                   +-----------+--------------+
                               |
              +----------------+----------------+
              |                                 |
              v                                 v
    SMTP email to customer              Offer rejected /
    (PDF attachment)                    back to edit loop
```

## Prerequisites

| Requirement | Version | Notes |
|---|---|---|
| Rust | 1.88 | Pinned via `rust-toolchain.toml` — `rustup` installs it automatically |
| PostgreSQL | 16 | Any 16.x release |
| Docker + Compose | Docker 24+ | For local dev infrastructure and production |
| LibreOffice | 7.x | Required for XLSX-to-PDF conversion (`soffice` must be on PATH) |

## Quick Start

1. **Clone the repository**

   ```bash
   git clone <repo-url>
   cd aust_backend
   ```

2. **Start infrastructure** (PostgreSQL 16, MinIO)

   ```bash
   docker compose -f docker/docker-compose.yml up -d
   ```

3. **Configure environment**

   ```bash
   cp .env.example .env
   # Edit .env — at minimum set your LLM API key, maps key, and Telegram tokens
   ```

4. **Run the server** (migrations apply automatically on startup)

   ```bash
   cargo run
   ```

   The server starts on `http://localhost:8080`. Verify with:

   ```bash
   curl http://localhost:8080/health
   ```

### Vision Service (optional, serverless GPU)

The 3D volume estimation pipeline runs on Modal (serverless L4 GPU). It is disabled by default — the system falls back to LLM vision analysis when the service is unavailable.

```bash
# Deploy to Modal (requires Modal account and CLI)
pip install modal
modal deploy services/vision/modal_app.py
```

Then set in `.env`:
```
AUST__VISION_SERVICE__ENABLED=true
AUST__VISION_SERVICE__BASE_URL=https://<your-modal-endpoint>.modal.run
```

## Environment Variables

All variables follow the pattern `AUST__SECTION__KEY` (double underscore as separator). They override values in `config/default.toml`.

| Variable | Description | Example |
|---|---|---|
| `AUST__DATABASE__URL` | PostgreSQL connection string | `postgres://aust:pass@localhost/aust_backend` |
| `AUST__DATABASE__MAX_CONNECTIONS` | Connection pool size | `10` |
| `AUST__STORAGE__PROVIDER` | Storage backend: `s3` or `local` | `s3` |
| `AUST__STORAGE__BUCKET` | S3 bucket name | `aust-uploads` |
| `AUST__STORAGE__ENDPOINT` | S3-compatible endpoint (MinIO) | `http://localhost:9000` |
| `AUST__STORAGE__REGION` | S3 region | `us-east-1` |
| `AWS_ACCESS_KEY_ID` | S3 access key | `minioadmin` |
| `AWS_SECRET_ACCESS_KEY` | S3 secret key | `minioadmin` |
| `AUST__LLM__DEFAULT_PROVIDER` | LLM provider: `claude`, `openai`, or `ollama` | `ollama` |
| `AUST__LLM__CLAUDE__API_KEY` | Anthropic API key | `sk-ant-...` |
| `AUST__LLM__CLAUDE__MODEL` | Claude model ID | `claude-sonnet-4-5` |
| `AUST__LLM__OPENAI__API_KEY` | OpenAI API key | `sk-...` |
| `AUST__LLM__OPENAI__MODEL` | OpenAI model ID | `gpt-4o` |
| `AUST__LLM__OLLAMA__BASE_URL` | Ollama base URL | `http://localhost:11434` |
| `AUST__LLM__OLLAMA__MODEL` | Ollama model name | `llama3.2-vision` |
| `AUST__MAPS__API_KEY` | OpenRouteService API key | `5b3ce3...` |
| `AUST__EMAIL__IMAP_HOST` | IMAP server hostname | `imap.example.com` |
| `AUST__EMAIL__IMAP_PORT` | IMAP port (SSL) | `993` |
| `AUST__EMAIL__SMTP_HOST` | SMTP server hostname | `smtp.example.com` |
| `AUST__EMAIL__SMTP_PORT` | SMTP port (STARTTLS) | `587` |
| `AUST__EMAIL__USERNAME` | Email account username | `umzug@example.com` |
| `AUST__EMAIL__PASSWORD` | Email account password | `secret` |
| `AUST__EMAIL__FROM_ADDRESS` | Sender address for outgoing mail | `umzug@example.com` |
| `AUST__EMAIL__FROM_NAME` | Sender display name | `AUST Umzüge` |
| `AUST__EMAIL__POLL_INTERVAL_SECS` | IMAP polling interval | `60` |
| `AUST__TELEGRAM__BOT_TOKEN` | Telegram bot token | `123456:ABC-...` |
| `AUST__TELEGRAM__ADMIN_CHAT_ID` | Admin Telegram chat ID | `987654321` |
| `AUST__AUTH__JWT_SECRET` | JWT signing secret (change in production) | `long-random-string` |
| `AUST__AUTH__JWT_EXPIRY_HOURS` | Access token lifetime in hours | `24` |
| `AUST__CALENDAR__DEFAULT_CAPACITY` | Max bookings per day | `1` |
| `AUST__CALENDAR__ALTERNATIVES_COUNT` | Alternatives to suggest when date is full | `3` |
| `AUST__VISION_SERVICE__ENABLED` | Enable 3D ML vision pipeline | `true` |
| `AUST__VISION_SERVICE__BASE_URL` | Modal vision service base URL | `https://...modal.run` |
| `AUST__VISION_SERVICE__TIMEOUT_SECS` | Vision request timeout | `600` |
| `AUST__COMPANY__DEPOT_ADDRESS` | Company depot for route calculation | `Borsigstr 6 31135 Hildesheim` |

## Crate Overview

| Crate | Responsibility |
|---|---|
| `aust-core` | Domain models, configuration structs, shared error types — used by every other crate |
| `aust-api` | Axum HTTP server, all route handlers, request/response types, orchestrator event loop |
| `aust-email-agent` | IMAP polling, email parsing, JSON attachment extraction, Telegram approval bot |
| `aust-volume-estimator` | LLM vision analysis + client for the external 3D ML vision service |
| `aust-distance-calculator` | Geocoding and multi-stop driving distance via OpenRouteService |
| `aust-offer-generator` | Pricing engine, XLSX template rendering, LibreOffice PDF conversion |
| `aust-llm-providers` | Pluggable LLM abstraction (Claude, OpenAI, Ollama) behind a common trait |
| `aust-storage` | Pluggable file storage abstraction (S3-compatible, local filesystem) |
| `aust-flash-contact` | Ultra-quick callback-request capture (public flash-contact form → DB) |
| `flash-contact-bot` | Standalone Telegram bot binary that notifies/handles flash-contact callbacks (own Docker image) |

Calendar logic has no dedicated crate — it lives in `aust-api` (`calendar_repo.rs`, `calendar_item_repo.rs`) and `aust-email-agent` (`calendar.rs`).

## Database

Migrations are applied automatically at startup via SQLx. To run them manually:

```bash
sqlx migrate run --database-url postgres://aust:aust_dev_password@localhost/aust_backend
```

Migration files are in `migrations/`. Key tables:

| Table | Purpose |
|---|---|
| `customers` | Customer contact information |
| `addresses` | Origin, destination, and stop addresses with geocoordinates |
| `inquiries` | Moving inquiries, status tracking, volume and distance |
| `volume_estimations` | Estimation results (method: vision / inventory / depth_sensor / video) |
| `offers` | Generated offers with pricing, PDF storage key, line items |
| `invoices` | Issued invoices and payment tracking |
| `employees` | Field staff — profile, auth, documents, clock times |
| `inquiry_employees` / `calendar_item_employees` | Employee assignments (one row per employee per `job_date`) |
| `calendar_items` | Non-inquiry calendar work items |
| `flash_contacts` | Quick callback requests from the public flash-contact form |
| `users` | Admin users (email + password hash + role) |
| `email_threads` / `email_messages` | Full email conversation history |

## API Reference

See [docs/API.md](docs/API.md) for the full API reference with request/response shapes and example curl commands.

### Endpoint summary

| Group | Base path |
|---|---|
| Health | `/health`, `/ready` |
| Auth | `/api/v1/auth/` |
| Inquiries | `/api/v1/inquiries/` |
| Volume Estimation | `/api/v1/estimates/` |
| Offers | `/api/v1/offers/` |
| Calendar | `/api/v1/calendar/` |
| Distance | `/api/v1/distance/` |
| Admin | `/api/v1/admin/` |

## Deployment

Production runs as Docker containers on a single VPS (`docker compose`), fronted
by a Cloudflare Tunnel. The backend container applies DB migrations automatically
on startup via `sqlx::migrate!()` — there is no manual migration step.

See [DEPLOYMENT.md](DEPLOYMENT.md) for the full runbook (staging, rollback,
restore drill, VPS layout).

### Deploy

```bash
# Backend + flash-bot only (build image → backup VPS → push → restart → health check)
bash scripts/deploy-prod.sh

# Backend + flash-bot + frontend (frontend ships to KAS hosting via FTP)
bash scripts/deploy-full.sh
```

Both scripts require a clean working tree on `main`. They back up the production
DB + MinIO before making any change and tag the previous image as `:previous`
for one-step rollback.

### Database backups

A nightly cron on the VPS runs `backup.sh` at 03:00 UTC — `pg_dump` + a MinIO
tarball into `/opt/aust/backups/`, 7-day retention, with Telegram size-anomaly
alerts. Pull backups off-site with `bash scripts/pull-backups.sh`. Install the
VPS cron once with `bash scripts/setup-backups.sh`.

## Development Workflow

### Run tests

```bash
# Run tests for a specific crate (skips broken example binaries)
cargo test -p aust-api --lib
cargo test -p aust-core --lib
cargo test -p aust-offer-generator --lib

# Run all library tests
cargo test --lib --workspace
```

### Useful development commands

```bash
# Check compilation without building
cargo check -p aust-api

# Clippy lints
cargo clippy -p aust-api

# Format
cargo fmt --all

# Follow server logs (production — container)
ssh root@<vps> 'docker logs -f aust_backend'

# Health checks
curl http://localhost:8080/health
curl http://localhost:8080/ready
```

### Logging

Set `RUST_LOG` to control log levels:

```bash
RUST_LOG=aust_api=debug,aust_email_agent=info cargo run
```

## License / Contact

License: MIT

Authors: AUST Team
