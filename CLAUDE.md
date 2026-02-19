# AUST Backend

A modular Rust backend for automating moving company operations - from initial customer email contact through volume estimation to automated offer generation.

## Project Overview

**Purpose**: Automate the quote-to-offer pipeline for a moving company (Austrian market, German language)

**Architecture**: Modular monolith with 8 crates, designed for future microservices extraction

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
| Maps | Google Maps API |
| Email | IMAP/SMTP via lettre + async-imap |

## Crate Structure

```
crates/
├── core/               # Domain models, config, shared errors
├── api/                # REST API, routes, middleware
├── llm-providers/      # LLM abstraction (Claude, OpenAI, Ollama)
├── storage/            # File storage abstraction (S3, local)
├── email-agent/        # Email processing (IMAP/SMTP + LLM)
├── volume-estimator/   # Volume calculation (vision AI, inventory)
├── distance-calculator/# Geocoding + distance calculation
└── offer-generator/    # Pricing engine + PDF generation
```

## Key Files

- `src/main.rs` - Application entry point, config loading, server startup
- `config/*.toml` - Configuration files (default, development, production)
- `migrations/*.sql` - Database schema
- `docker/docker-compose.yml` - Local dev infrastructure

## Running Locally

```bash
# Start PostgreSQL, Redis, MinIO
cd docker && docker-compose up -d

# Configure environment
cp .env.example .env
# Edit .env with your API keys (LLM, Maps)

# Run migrations and start server
cargo run
```

Server runs on `http://localhost:8080`

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
- `POST /api/v1/estimates/vision` - AI image analysis (base64 JSON)
- `POST /api/v1/estimates/inventory` - Manual inventory form
- `GET /api/v1/estimates/{id}` - Get estimation

### Distance
- `POST /api/v1/distance/calculate` - Calculate distance between addresses

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
              ┌─────────────────────┴─────────────────────┐
              ↓                                           ↓
    Volume Estimator                            Distance Calculator
    (Vision AI / Inventory)                     (Geocoding + Routing)
              ↓                                           ↓
              └─────────────────────┬─────────────────────┘
                                    ↓
                            Offer Generator
                            (Pricing + PDF)
                                    ↓
                            Email Agent sends offer
```

## Configuration

Environment variables override config files. Format: `AUST__SECTION__KEY`

Examples:
- `AUST__DATABASE__URL=postgres://...`
- `AUST__LLM__CLAUDE__API_KEY=sk-...`
- `AUST__MAPS__API_KEY=...`

---

# TODOs

## High Priority (Core Functionality)

### Email Agent - Full Implementation
- [ ] Implement IMAP polling in `crates/email-agent/src/imap_client.rs`
- [ ] Implement SMTP sending in `crates/email-agent/src/smtp_client.rs`
- [ ] Implement email parsing (extract addresses, dates) in `parser.rs`
- [ ] Implement LLM-powered intent detection in `parser.rs`
- [ ] Implement LLM-powered response generation in `responder.rs`
- [ ] Add background task for periodic IMAP polling
- [ ] Handle email threading (In-Reply-To, References headers)

### Authentication
- [ ] Implement proper JWT token generation in `crates/api/src/routes/auth.rs`
- [ ] Implement password hashing with Argon2
- [ ] Add JWT validation middleware in `crates/api/src/middleware/auth.rs`
- [ ] Add admin user seeding/creation endpoint
- [ ] Protect API routes with auth middleware

### PDF Generation
- [ ] Implement proper PDF generation using Typst in `crates/offer-generator/src/pdf.rs`
- [ ] Create German offer letter template
- [ ] Add company logo/branding support
- [ ] Store generated PDFs in S3

## Medium Priority (Features)

### Volume Estimation Improvements
- [ ] Add multipart file upload support (currently base64 JSON only)
- [ ] Implement depth sensor data processing for mobile app
- [ ] Add item catalog with predefined volumes
- [ ] Improve vision analysis prompts for better accuracy

### Pricing Engine
- [ ] Make pricing configurable via database/config
- [ ] Add seasonal pricing adjustments
- [ ] Add floor/elevator surcharge configuration
- [ ] Add weekend/holiday pricing
- [ ] Support multiple pricing tiers

### Distance Calculator
- [ ] Add result caching in Redis
- [ ] Support alternative routing providers (OpenRouteService)
- [ ] Add travel time estimation

### Customer Management
- [ ] Add customer CRUD endpoints
- [ ] Add address CRUD endpoints
- [ ] Link email threads to customers automatically

## Low Priority (Polish)

### API Improvements
- [ ] Add request validation with detailed error messages
- [ ] Add pagination metadata to list endpoints
- [ ] Add filtering by status in quotes list
- [ ] Add OpenAPI/Swagger documentation
- [ ] Add rate limiting middleware

### Observability
- [ ] Add structured logging with request IDs
- [ ] Add Prometheus metrics endpoint
- [ ] Add health check for Redis and S3
- [ ] Add request/response logging middleware

### Testing
- [ ] Add unit tests for pricing engine
- [ ] Add unit tests for volume calculator
- [ ] Add integration tests with test database
- [ ] Add API endpoint tests

### DevOps
- [ ] Add GitHub Actions CI/CD pipeline
- [ ] Add Kubernetes deployment manifests
- [ ] Add database backup strategy
- [ ] Add secrets management (Vault, etc.)

## Technical Debt

- [ ] Remove unused `patch` import in `quotes.rs`
- [ ] Use `status` field in `ListQuotesQuery` for filtering
- [ ] Extract duplicate `QuoteRow` struct to shared location
- [ ] Add proper error handling for LLM API failures (retries, fallbacks)
- [ ] Add database connection pooling configuration
- [ ] Consider sqlx offline mode for compile-time query verification

## Future Considerations

### Mobile App Integration
- [ ] Design API for depth sensor volume estimation
- [ ] Add push notification support
- [ ] Add real-time quote status updates (WebSocket?)

### Multi-tenancy (if needed later)
- [ ] Add tenant ID to all tables
- [ ] Add tenant isolation middleware
- [ ] Add tenant configuration management

### Analytics
- [ ] Track quote conversion rates
- [ ] Track volume estimation accuracy
- [ ] Add admin dashboard endpoints

---

## Code Conventions

- **German for user-facing content**: Error messages, email responses, offer letters
- **English for code**: Variables, functions, comments
- **UUIDs**: Use v7 (time-ordered) for new records
- **Dates**: Always UTC in database, convert for display
- **Money**: Store as cents (i64), currency code separate
- **Status enums**: Store as lowercase strings in DB

## LLM Provider Notes

The system supports multiple LLM providers for benchmarking:

- **Claude**: Best for German language, vision capabilities
- **OpenAI**: Alternative, good vision support
- **Ollama**: Local/self-hosted, privacy-focused

Switch providers via `AUST__LLM__DEFAULT_PROVIDER` (claude/openai/ollama)

## Database Schema

See `migrations/20240101000000_initial.sql` for full schema.

Key tables:
- `customers` - Contact information
- `addresses` - Origin/destination addresses with geocoding
- `quotes` - Quote requests with status tracking
- `volume_estimations` - Volume calculation results
- `offers` - Generated offers with pricing
- `email_threads` / `email_messages` - Email conversation tracking
- `users` - Admin users
