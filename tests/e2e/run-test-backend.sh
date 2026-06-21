#!/usr/bin/env bash
# Run the API backend for the worker-portal e2e suite.
#
# Points at the staging Postgres/MinIO/Mailpit from docker/docker-compose.staging.yml
# (where the admin@integration-test.invalid seed lives) and routes SMTP to Mailpit
# so the OTP login works. Telegram is pointed at a local mock the e2e spec runs
# (worker-pending-hours.spec.ts) so the "hours logged" notification can be asserted.
#
# Prereqs:
#   docker compose -f docker/docker-compose.staging.yml up -d \
#       staging-postgres staging-minio staging-minio-setup staging-mailpit
#   cargo build -p aust-api
#
# Then:  bash tests/e2e/run-test-backend.sh
set -euo pipefail
cd "$(dirname "$0")/../.."

pkill -f "target/debug/aust_backend" 2>/dev/null || true
sleep 1

export RUN_MODE=development
export AUST__DATABASE__URL="postgres://aust_staging:aust_staging_password@localhost:5435/aust_staging"
export AUST__STORAGE__PROVIDER=s3
export AUST__STORAGE__ENDPOINT="http://localhost:9010"
export AUST__STORAGE__BUCKET=aust-uploads
export AUST__STORAGE__REGION=us-east-1
export AUST__STORAGE__ACCESS_KEY_ID=minioadmin
export AUST__STORAGE__SECRET_ACCESS_KEY=minioadmin
export AUST__EMAIL__SMTP_HOST=localhost
export AUST__EMAIL__SMTP_PORT=1025
export AUST__EMAIL__SMTP_TLS=none
export AUST__EMAIL__IMAP_HOST=imap.staging.invalid
export AUST__EMAIL__IMAP_PORT=993
export AUST__EMAIL__USERNAME=staging@aust-umzuege.de
export AUST__EMAIL__PASSWORD=staging-email-password
export AUST__EMAIL__FROM_ADDRESS=staging@aust-umzuege.de
export AUST__EMAIL__FROM_NAME="AUST Umzuege Dev"
export AUST__LLM__DEFAULT_PROVIDER=ollama
export AUST__LLM__OLLAMA__BASE_URL=http://localhost:11434
export AUST__LLM__OLLAMA__MODEL=qwen2.5:7b
export AUST__TELEGRAM__BOT_TOKEN="0000000000:e2e-mock-bot-token"
export AUST__TELEGRAM__ADMIN_CHAT_ID=0
export AUST__AUTH__JWT_SECRET="dev-jwt-secret-do-not-use-in-production-min32chars"
export AUST__VISION_SERVICE__ENABLED=false
export AUST__COMPANY__DEPOT_ADDRESS="Borsigstr 6 31135 Hildesheim"
export AUST__COMPANY__FAHRT_RATE_PER_KM=1.00

# Worker hours-logged notification → local mock recorder the e2e spec listens on.
export AUST_TELEGRAM_API_BASE="http://localhost:${TELEGRAM_MOCK_PORT:-8077}"

exec ./target/debug/aust_backend
