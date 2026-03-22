#!/usr/bin/env bash
# deploy-all.sh — Deploy backend + frontend in sync
#
# Steps:
#   1. Pre-flight checks
#   2. Backup database
#   3. Pull latest: backend (git pull) + frontend submodule (origin/main)
#   4. Build backend (cargo --release) + frontend (npm build) — in parallel
#   5. Inline CSS in frontend build
#   6. Upload frontend to KAS via FTP
#   7. Restart aust-backend systemd service
#   8. Health check
#
# Requirements:
#   - cargo, python3, npm (or bun) on PATH
#   - frontend/.env with FTP_PASS set (see frontend/.env.example)
#   - sudo rights for systemctl restart
#   - Docker running with aust_postgres healthy (for DB backup)

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
FRONTEND_DIR="${PROJECT_DIR}/frontend"

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

TOTAL_STEPS=8

echo -e "${BOLD}"
echo "============================================="
echo "  AUST Full Deploy (backend + frontend)"
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

if ! command -v python3 >/dev/null 2>&1; then
    fail "python3 not found (required for inline-css.py and deploy-full.py)"
fi
ok "Python $(python3 --version 2>&1 | awk '{print $2}')"

# Prefer bun, fall back to npm for frontend build
if command -v bun >/dev/null 2>&1; then
    NODE_CMD="bun"
elif command -v npm >/dev/null 2>&1; then
    NODE_CMD="npm"
else
    fail "Neither bun nor npm found (required for frontend build)"
fi
ok "${NODE_CMD} (frontend build)"

if [ ! -f "${FRONTEND_DIR}/.env" ]; then
    fail "frontend/.env not found — copy frontend/.env.example and fill in FTP_PASS"
fi
if ! grep -q "FTP_PASS=" "${FRONTEND_DIR}/.env" || grep -q "FTP_PASS=your_" "${FRONTEND_DIR}/.env"; then
    fail "FTP_PASS not set in frontend/.env"
fi
ok "frontend/.env present with FTP_PASS"

# Uncommitted changes on backend (submodule pointer changes are OK — we update it here)
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

# Backend
BEFORE=$(git -C "${PROJECT_DIR}" rev-parse HEAD)
git -C "${PROJECT_DIR}" pull --ff-only
AFTER=$(git -C "${PROJECT_DIR}" rev-parse HEAD)
if [ "${BEFORE}" = "${AFTER}" ]; then
    ok "Backend: already up to date (${BEFORE:0:7})"
else
    ok "Backend: ${BEFORE:0:7} → ${AFTER:0:7}"
    git -C "${PROJECT_DIR}" log --oneline "${BEFORE}..${AFTER}"
fi

# Frontend submodule — always pin to origin/main
git -C "${FRONTEND_DIR}" fetch origin
git -C "${FRONTEND_DIR}" checkout main
git -C "${FRONTEND_DIR}" pull origin main
FRONTEND_SHA=$(git -C "${FRONTEND_DIR}" rev-parse --short HEAD)
ok "Frontend: origin/main @ ${FRONTEND_SHA}"

# Commit updated submodule ref only if the recorded pointer changed
# (git status --porcelain also fires for untracked content inside the submodule,
#  which is not a pointer change and cannot be committed from here)
RECORDED_SHA=$(git -C "${PROJECT_DIR}" ls-files -s frontend | awk '{print $2}')
CURRENT_SHA=$(git -C "${FRONTEND_DIR}" rev-parse HEAD)
if [ "${RECORDED_SHA}" != "${CURRENT_SHA}" ]; then
    git -C "${PROJECT_DIR}" add frontend
    git -C "${PROJECT_DIR}" commit -m "chore: update frontend submodule → ${FRONTEND_SHA}"
    ok "Submodule ref committed"
fi

# ---------------------------------------------------------------------------
# 4. Build backend + frontend in parallel
# ---------------------------------------------------------------------------
step 4 "Building backend and frontend in parallel"

BACKEND_LOG=$(mktemp /tmp/aust-backend-build.XXXX)
FRONTEND_LOG=$(mktemp /tmp/aust-frontend-build.XXXX)

echo "  Starting cargo build --release..."
cargo build --release --manifest-path "${PROJECT_DIR}/Cargo.toml" \
    >"${BACKEND_LOG}" 2>&1 &
BACKEND_PID=$!

echo "  Starting ${NODE_CMD} run build (frontend)..."
(
    cd "${FRONTEND_DIR}"
    # Install/sync deps silently if needed
    if [ ! -d node_modules ]; then
        ${NODE_CMD} install --silent 2>/dev/null || ${NODE_CMD} install
    fi
    ${NODE_CMD} run build
) >"${FRONTEND_LOG}" 2>&1 &
FRONTEND_PID=$!

# Wait for both — report which one failed if either does
BACKEND_EXIT=0
FRONTEND_EXIT=0
wait "${BACKEND_PID}"  || BACKEND_EXIT=$?
wait "${FRONTEND_PID}" || FRONTEND_EXIT=$?

if [ "${BACKEND_EXIT}" -ne 0 ]; then
    echo -e "${RED}Backend build failed:${RESET}"
    tail -30 "${BACKEND_LOG}"
    rm -f "${BACKEND_LOG}" "${FRONTEND_LOG}"
    exit 1
fi
ok "Backend build"

if [ "${FRONTEND_EXIT}" -ne 0 ]; then
    echo -e "${RED}Frontend build failed:${RESET}"
    tail -30 "${FRONTEND_LOG}"
    rm -f "${BACKEND_LOG}" "${FRONTEND_LOG}"
    exit 1
fi
ok "Frontend build"

rm -f "${BACKEND_LOG}" "${FRONTEND_LOG}"

# ---------------------------------------------------------------------------
# 5. Inline CSS
# ---------------------------------------------------------------------------
step 5 "Inlining CSS"
python3 "${FRONTEND_DIR}/inline-css.py"
ok "CSS inlined"

# ---------------------------------------------------------------------------
# 6. Upload frontend to KAS
# ---------------------------------------------------------------------------
step 6 "Uploading frontend to KAS (FTP)"
python3 "${FRONTEND_DIR}/deploy-full.py"
ok "Frontend uploaded"

# ---------------------------------------------------------------------------
# 7. Restart backend service
# ---------------------------------------------------------------------------
step 7 "Restarting ${SERVICE_NAME}"
sudo systemctl restart "${SERVICE_NAME}"
ok "Service restarted"

# ---------------------------------------------------------------------------
# 8. Health check
# ---------------------------------------------------------------------------
step 8 "Health check"
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
echo "  Backend:  $(git -C "${PROJECT_DIR}" rev-parse --short HEAD)"
echo "  Frontend: ${FRONTEND_SHA}"
echo "  Service:  $(systemctl is-active ${SERVICE_NAME})"
echo "  Backup:   $(ls -1t "${PROJECT_DIR}/backups/db/"*.sql.gz 2>/dev/null | head -1 | xargs basename 2>/dev/null || echo 'n/a')"
echo "=============================================${RESET}"
