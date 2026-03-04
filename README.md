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

| Requirement | Minimum version | Notes |
|---|---|---|
| Rust | 1.82 (stable) | `rustup update stable` |
| PostgreSQL | 16 | Any 16.x release |
| Docker + Compose | Docker 24+ | For local dev infrastructure |
| LibreOffice | 7.x | Required for XLSX-to-PDF conversion (`soffice` must be on PATH) |

## Quick Start

1. **Clone the repository**

   ```bash
   git clone <repo-url>
   cd aust_backend
   ```

2. **Start infrastructure** (PostgreSQL 16, MinIO)

   ```bash
   cd docker && docker-compose up -d
   cd ..
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
| `AUST__LLM__DEFAULT_PROVIDER` | LLM provider: `claude`, `openai`, or `ollama` | `claude` |
| `AUST__LLM__CLAUDE__API_KEY` | Anthropic API key | `sk-ant-...` |
| `AUST__LLM__CLAUDE__MODEL` | Claude model ID | `claude-sonnet-4-20250514` |
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
| `aust-calendar` | Moving date booking management, capacity tracking, availability queries |

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
| `quotes` | Moving quote requests, status tracking, volume and distance |
| `volume_estimations` | Estimation results (method: vision / inventory / depth_sensor / video) |
| `offers` | Generated offers with pricing, PDF storage key, line items |
| `email_threads` / `email_messages` | Full email conversation history |
| `calendar_bookings` | Moving date bookings |
| `calendar_capacity_overrides` | Per-date capacity overrides |
| `users` | Admin users (email + Argon2 password hash + role) |

## API Reference

See [docs/API.md](docs/API.md) for the full API reference with request/response shapes and example curl commands.

### Endpoint summary

| Group | Base path |
|---|---|
| Health | `/health`, `/ready` |
| Auth | `/api/v1/auth/` |
| Quotes | `/api/v1/quotes/` |
| Volume Estimation | `/api/v1/estimates/` |
| Offers | `/api/v1/offers/` |
| Calendar | `/api/v1/calendar/` |
| Distance | `/api/v1/distance/` |
| Admin | `/api/v1/admin/` |

## Deployment

The backend runs as a systemd service (`aust-backend.service`) built from a release binary.

### Full deploy

```bash
# Backup DB → git pull → cargo build --release → restart service → health check
./scripts/deploy.sh
```

### Manual restart

```bash
sudo systemctl restart aust-backend
journalctl -u aust-backend -f
```

### Database backups

```bash
# Manual backup (stored in backups/db/ as gzipped pg_dump, 30-backup rotation)
./scripts/backup-db.sh

# Install daily backup timer (runs at 03:00)
sudo cp scripts/aust-backup.service scripts/aust-backup.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now aust-backup.timer
```

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
cargo check --workspace

# Clippy lints
cargo clippy --workspace

# Format
cargo fmt --all

# Follow server logs
journalctl -u aust-backend -f

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
