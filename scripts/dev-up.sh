#!/usr/bin/env bash
# dev-up.sh — Local dev environment with hot reload, against production data.
#
# What it does:
#   1. Ensures staging infra (postgres + minio + mailpit) is up
#      — these three reuse the staging-up.sh stack, so the DB can hold a
#      production backup restored via scripts/restore-local.sh.
#   2. Stops any running staging-backend/staging-frontend containers so
#      port 8080 and 5173 are free and there is no version confusion.
#   3. Runs `cargo watch -x run` (fallback: `cargo run`) — backend on :8080.
#   4. Runs `npm run dev` in frontend/ — Vite on :5173 with hot module reload,
#      VITE_API_BASE pointed at the local backend.
#
# Flags:
#   --fresh        Pull newest backup from VPS and restore before starting
#   --no-frontend  Skip the frontend dev server (backend only)
#   --no-watch     Use `cargo run` without cargo-watch (one-shot)
#
# Usage:
#   bash scripts/dev-up.sh                    # start against existing staging data
#   bash scripts/dev-up.sh --fresh            # pull+restore newest backup first
#
# Ctrl-C cleanly stops both processes; staging infra containers keep running
# (stop them with `bash scripts/staging-up.sh --down` if wanted).

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
FRONTEND_DIR="${PROJECT_DIR}/frontend"
STAGING_COMPOSE="${PROJECT_DIR}/docker/docker-compose.staging.yml"

GREEN="\033[0;32m"; RED="\033[0;31m"; YELLOW="\033[0;33m"; BOLD="\033[1m"; RESET="\033[0m"
ok()   { echo -e "  ${GREEN}OK${RESET}  ${1}"; }
warn() { echo -e "  ${YELLOW}WARN${RESET} ${1}"; }
fail() { echo -e "  ${RED}FAIL${RESET} ${1}" >&2; exit 1; }
step() { echo -e "\n${BOLD}>>> ${1}${RESET}"; }

# ---------------------------------------------------------------------------
# Parse flags
# ---------------------------------------------------------------------------
FRESH=0
NO_FRONTEND=0
NO_WATCH=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --fresh)       FRESH=1; shift ;;
        --no-frontend) NO_FRONTEND=1; shift ;;
        --no-watch)    NO_WATCH=1; shift ;;
        -h|--help)
            sed -n '2,22p' "$0"
            exit 0 ;;
        *) fail "Unknown flag: $1" ;;
    esac
done

# ---------------------------------------------------------------------------
# 1. Pull + restore fresh prod backup (optional)
# ---------------------------------------------------------------------------
if [[ "${FRESH}" -eq 1 ]]; then
    step "Pulling newest backup from VPS"
    bash "${PROJECT_DIR}/scripts/pull-backups.sh"
fi

# ---------------------------------------------------------------------------
# 2. Staging infra up (postgres + minio + mailpit only)
# ---------------------------------------------------------------------------
step "Starting staging infra (postgres + minio + mailpit)"
docker compose -f "${STAGING_COMPOSE}" up -d \
    staging-postgres staging-minio staging-minio-setup staging-mailpit

# Wait for postgres healthy
for i in $(seq 1 30); do
    if docker inspect aust_staging_postgres --format '{{.State.Health.Status}}' 2>/dev/null | grep -q healthy; then
        ok "postgres healthy"
        break
    fi
    [[ "${i}" -eq 30 ]] && fail "postgres did not become healthy in 30s"
    sleep 1
done

# Wait for minio healthy
for i in $(seq 1 30); do
    if docker inspect aust_staging_minio --format '{{.State.Health.Status}}' 2>/dev/null | grep -q healthy; then
        ok "minio healthy"
        break
    fi
    [[ "${i}" -eq 30 ]] && fail "minio did not become healthy in 30s"
    sleep 1
done

# ---------------------------------------------------------------------------
# 3. Restore fresh data (optional)
# ---------------------------------------------------------------------------
if [[ "${FRESH}" -eq 1 ]]; then
    step "Restoring newest backup into staging"
    bash "${PROJECT_DIR}/scripts/restore-local.sh" -y
fi

# ---------------------------------------------------------------------------
# 4. Stop staging-backend + staging-frontend containers (free ports, avoid
#    version mismatch confusion)
# ---------------------------------------------------------------------------
step "Stopping staging backend/frontend containers"
for c in aust_staging_backend aust_staging_frontend; do
    if docker ps --format '{{.Names}}' | grep -q "^${c}\$"; then
        docker stop "${c}" >/dev/null
        ok "${c} stopped"
    fi
done

# ---------------------------------------------------------------------------
# 5. Export env for local backend (points at staging infra via host ports)
# ---------------------------------------------------------------------------
# Keep in sync with docker/docker-compose.staging.yml → staging-backend.environment.
export RUN_MODE=development
export AUST__DATABASE__URL="postgres://aust_staging:aust_staging_password@localhost:5435/aust_staging"

export AUST__STORAGE__PROVIDER=s3
export AUST__STORAGE__ENDPOINT="http://localhost:9010"
export AUST__STORAGE__BUCKET=aust-staging-uploads
export AUST__STORAGE__REGION=us-east-1
export AUST__STORAGE__ACCESS_KEY_ID=minioadmin
export AUST__STORAGE__SECRET_ACCESS_KEY=minioadmin

export AUST__EMAIL__SMTP_HOST=localhost
export AUST__EMAIL__SMTP_PORT=1025
export AUST__EMAIL__IMAP_HOST=imap.staging.invalid
export AUST__EMAIL__IMAP_PORT=993
export AUST__EMAIL__USERNAME=staging@aust-umzuege.de
export AUST__EMAIL__PASSWORD=staging-email-password
export AUST__EMAIL__FROM_ADDRESS=staging@aust-umzuege.de
export AUST__EMAIL__FROM_NAME="AUST Umzüge (Dev)"

export AUST__LLM__DEFAULT_PROVIDER=ollama
export AUST__LLM__OLLAMA__BASE_URL=http://localhost:11434
export AUST__LLM__OLLAMA__MODEL=qwen2.5:7b

export AUST__TELEGRAM__BOT_TOKEN="0000000000:AAAAAAAAAAaaaaaaaaaaaaaaaaaaaaaaaaaa"
export AUST__TELEGRAM__ADMIN_CHAT_ID=0

export AUST__AUTH__JWT_SECRET="dev-jwt-secret-do-not-use-in-production-min32chars"

export AUST__VISION_SERVICE__ENABLED=false

export AUST__COMPANY__DEPOT_ADDRESS="Borsigstr 6 31135 Hildesheim"
export AUST__COMPANY__FAHRT_RATE_PER_KM=1.00

# Allow overrides from crates/api/.env or docker/.env.staging if the user wants
# real LLM/Telegram credentials locally. Loaded AFTER the defaults above so
# anything in the file wins.
if [[ -f "${PROJECT_DIR}/docker/.env.staging" ]]; then
    set -a
    source "${PROJECT_DIR}/docker/.env.staging"
    set +a
    ok "loaded docker/.env.staging overrides"
fi

# ---------------------------------------------------------------------------
# 6. Start backend + frontend with cleanup trap
# ---------------------------------------------------------------------------
BACKEND_PID=""
FRONTEND_PID=""

cleanup() {
    echo -e "\n${BOLD}>>> Shutting down${RESET}"
    [[ -n "${BACKEND_PID}"  ]] && kill "${BACKEND_PID}"  2>/dev/null || true
    [[ -n "${FRONTEND_PID}" ]] && kill "${FRONTEND_PID}" 2>/dev/null || true
    wait 2>/dev/null || true
    ok "done (staging infra still running — stop with scripts/staging-up.sh --down)"
}
trap cleanup EXIT INT TERM

# Free port 8080 if something else grabbed it
if lsof -ti:8080 >/dev/null 2>&1; then
    warn "port 8080 in use — killing previous dev backend"
    kill $(lsof -ti:8080) 2>/dev/null || true
    sleep 1
fi

step "Starting backend on :8080"
cd "${PROJECT_DIR}"
if [[ "${NO_WATCH}" -eq 1 ]] || ! command -v cargo-watch >/dev/null 2>&1; then
    [[ "${NO_WATCH}" -eq 0 ]] && warn "cargo-watch not installed — falling back to cargo run"
    cargo run --bin aust_backend &
else
    cargo watch -x 'run --bin aust_backend' -w crates -w config &
fi
BACKEND_PID=$!
ok "backend PID=${BACKEND_PID}"

if [[ "${NO_FRONTEND}" -eq 0 ]]; then
    step "Starting frontend dev server on :5173"
    cd "${FRONTEND_DIR}"
    [[ -d node_modules ]] || npm install
    VITE_API_BASE=http://localhost:8080 npm run dev -- --host &
    FRONTEND_PID=$!
    ok "frontend PID=${FRONTEND_PID}"
fi

echo -e "\n${GREEN}${BOLD}=============================="
echo "  Dev stack up"
echo "  Backend:  http://localhost:8080  (hot reload)"
[[ "${NO_FRONTEND}" -eq 0 ]] && \
echo "  Frontend: http://localhost:5173  (hot reload)"
echo "  Mailpit:  http://localhost:8025  (captured emails)"
echo "  MinIO UI: http://localhost:9011  (minioadmin / minioadmin)"
echo "  DB:       postgres://aust_staging:aust_staging_password@localhost:5435/aust_staging"
echo ""
echo "  Ctrl-C to stop. Staging infra stays running."
echo -e "==============================${RESET}"

# Wait for either process to exit — then trap handles cleanup
wait -n "${BACKEND_PID}" ${FRONTEND_PID:+$FRONTEND_PID} 2>/dev/null || true
