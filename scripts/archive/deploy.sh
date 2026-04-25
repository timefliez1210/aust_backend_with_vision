#!/usr/bin/env bash
# deploy.sh — Backend-only deploy (no frontend build/upload)
#
# Steps:
#   1. Pre-flight checks
#   2. Backup database
#   3. Pull latest (git pull --ff-only)
#   4. Build backend (cargo --release)
#   5. Restart aust-backend systemd service
#   6. Health check
#
# Requirements:
#   - cargo on PATH
#   - Docker running with aust_postgres healthy (for DB backup)
#   - sudo rights for systemctl restart

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

HEALTH_URL="http://localhost:8080/health"
SERVICE_NAME="aust-backend"
HEALTH_RETRIES=5
HEALTH_DELAY=3

# ---------------------------------------------------------------------------
# Colors
# ---------------------------------------------------------------------------
if [ -t 1 ]; then
    GREEN="\033[0;32m"; RED="\033[0;31m"; YELLOW="\033[0;33m"
    BOLD="\033[1m"; RESET="\033[0m"
else
    GREEN=""; RED=""; YELLOW=""; BOLD=""; RESET=""
fi

step()    { echo -e "\n${BOLD}[${1}/${TOTAL_STEPS}] ${2}${RESET}"; }
ok()      { echo -e "  ${GREEN}OK${RESET}  ${1}"; }
warn()    { echo -e "  ${YELLOW}WARN${RESET} ${1}"; }
fail()    { echo -e "  ${RED}FAIL${RESET} ${1}" >&2; exit 1; }

TOTAL_STEPS=6

echo -e "${BOLD}"
echo "============================================="
echo "  AUST Backend Deploy"
echo "  $(date '+%Y-%m-%d %H:%M:%S')"
echo "=============================================${RESET}"

# ---------------------------------------------------------------------------
# 1. Pre-flight checks
# ---------------------------------------------------------------------------
step 1 "Pre-flight checks"

if ! docker info >/dev/null 2>&1; then
    fail "Docker is not running"
fi
ok "Docker"

if ! docker exec aust_postgres pg_isready -U aust -d aust_backend >/dev/null 2>&1; then
    fail "PostgreSQL is not ready (is docker/docker-compose up?)"
fi
ok "PostgreSQL"

if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo not found"
fi
ok "Cargo $(cargo --version 2>/dev/null | awk '{print $2}')"

# Exclude submodules — frontend and app are managed independently
if [ -n "$(git -C "${PROJECT_DIR}" status --porcelain -- ':!frontend' ':!app')" ]; then
    fail "Uncommitted changes in backend. Commit or stash first."
fi
ok "Working tree clean"

# ---------------------------------------------------------------------------
# 2. Backup database
# ---------------------------------------------------------------------------
step 2 "Backing up database"
bash "${PROJECT_DIR}/scripts/backup-db.sh"
ok "Database backed up"

# ---------------------------------------------------------------------------
# 3. Pull latest
# ---------------------------------------------------------------------------
step 3 "Pulling latest changes"

BEFORE=$(git -C "${PROJECT_DIR}" rev-parse HEAD)
git -C "${PROJECT_DIR}" pull --ff-only
AFTER=$(git -C "${PROJECT_DIR}" rev-parse HEAD)

if [ "${BEFORE}" = "${AFTER}" ]; then
    ok "Already up to date (${BEFORE:0:7})"
else
    ok "${BEFORE:0:7} → ${AFTER:0:7}"
    git -C "${PROJECT_DIR}" log --oneline "${BEFORE}..${AFTER}"
fi

# ---------------------------------------------------------------------------
# 4. Build backend
# ---------------------------------------------------------------------------
step 4 "Building backend (cargo --release)"

BUILD_LOG=$(mktemp /tmp/aust-backend-build.XXXX)
cargo build --release --manifest-path "${PROJECT_DIR}/Cargo.toml" \
    >"${BUILD_LOG}" 2>&1 || {
    echo -e "${RED}Build failed:${RESET}"
    tail -30 "${BUILD_LOG}"
    rm -f "${BUILD_LOG}"
    exit 1
}
rm -f "${BUILD_LOG}"
ok "Backend build"

# ---------------------------------------------------------------------------
# 5. Restart backend service
# ---------------------------------------------------------------------------
step 5 "Restarting ${SERVICE_NAME}"
sudo systemctl restart "${SERVICE_NAME}"
ok "Service restarted"

# ---------------------------------------------------------------------------
# 6. Health check
# ---------------------------------------------------------------------------
step 6 "Health check"
for i in $(seq 1 "${HEALTH_RETRIES}"); do
    sleep "${HEALTH_DELAY}"
    if curl -sf "${HEALTH_URL}" >/dev/null 2>&1; then
        ok "Backend healthy (attempt ${i}/${HEALTH_RETRIES})"
        break
    fi
    if [ "${i}" -eq "${HEALTH_RETRIES}" ]; then
        fail "Health check failed after ${HEALTH_RETRIES} attempts — check: journalctl -u ${SERVICE_NAME} -n 50"
    fi
    warn "Attempt ${i}/${HEALTH_RETRIES} — retrying in ${HEALTH_DELAY}s..."
done

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo -e "\n${GREEN}${BOLD}"
echo "============================================="
echo "  Deploy complete!"
echo "  Commit:  $(git -C "${PROJECT_DIR}" rev-parse --short HEAD)"
echo "  Service: $(systemctl is-active ${SERVICE_NAME})"
echo "  Backup:  $(ls -1t "${PROJECT_DIR}/backups/db/"*.sql.gz 2>/dev/null | head -1 | xargs basename 2>/dev/null || echo 'n/a')"
echo "=============================================${RESET}"
