#!/usr/bin/env bash
# deploy-full.sh — Build & deploy backend (VPS Docker) + frontend (KAS FTP)
#
# Steps:
#   1. Pre-flight checks
#   2. Backup production DB + MinIO on VPS (before any changes)
#   3. Build backend Docker image locally
#   4. Build frontend (bun + inline-css)
#   5. Tag existing VPS image as :previous (rollback anchor)
#   6. Save + upload + load image on VPS
#   7. Upload migrations
#   8. Restart backend container (auto-migrates on startup)
#   9. Deploy frontend to KAS via FTP
#  10. Health check
#
# Requirements:
#   - FTP_PASS env var set (or a .env file in frontend/ with FTP_PASS=...)
#   - bun available in PATH
#   - python3 available in PATH
#   - Docker running locally
#   - SSH key at ~/.ssh/id_ed25519
#
# Run from your LOCAL machine (must be on main, clean working tree):
#   bash scripts/deploy-full.sh

set -euo pipefail

VPS_IP="72.62.89.179"
VPS_USER="root"
SSH_KEY="$HOME/.ssh/id_ed25519"
SSH="ssh -i ${SSH_KEY} -o StrictHostKeyChecking=no ${VPS_USER}@${VPS_IP}"
SCP="scp -i ${SSH_KEY} -o StrictHostKeyChecking=no"

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
FRONTEND_DIR="${PROJECT_DIR}/frontend"
IMAGE_NAME="aust_backend"
TARBALL="/tmp/aust_backend.tar.gz"

GREEN="\033[0;32m"; RED="\033[0;31m"; BOLD="\033[1m"; RESET="\033[0m"
ok()   { echo -e "  ${GREEN}OK${RESET}  ${1}"; }
fail() { echo -e "  ${RED}FAIL${RESET}  ${1}"; exit 1; }
step() { echo -e "\n${BOLD}>>> ${1}${RESET}"; }

echo -e "${BOLD}=============================="
echo "  AUST Full Deploy"
echo "  Backend  → VPS ${VPS_IP}"
echo "  Frontend → KAS FTP"
echo "  $(date '+%Y-%m-%d %H:%M:%S')"
echo -e "==============================${RESET}"

# ---------------------------------------------------------------------------
# 1. Pre-flight checks
# ---------------------------------------------------------------------------
step "Pre-flight checks"

if [ -n "$(git -C "${PROJECT_DIR}" status --porcelain)" ]; then
    fail "Working tree is not clean. Commit or stash changes first."
fi
ok "Working tree clean"

CURRENT_BRANCH="$(git -C "${PROJECT_DIR}" rev-parse --abbrev-ref HEAD)"
if [ "${CURRENT_BRANCH}" != "main" ]; then
    fail "Not on main branch (current: ${CURRENT_BRANCH}). Switch to main first."
fi
ok "On main branch"

if ! ${SSH} true 2>/dev/null; then
    fail "Cannot reach VPS via SSH (${VPS_USER}@${VPS_IP})"
fi
ok "SSH to VPS works"

if ! ${SSH} test -f /opt/aust/docker-compose.yml; then
    fail "/opt/aust/docker-compose.yml not found on VPS"
fi
ok "docker-compose.yml present on VPS"

if ! command -v bun >/dev/null 2>&1; then
    fail "bun not found in PATH"
fi
ok "bun available"

if ! command -v python3 >/dev/null 2>&1; then
    fail "python3 not found in PATH"
fi
ok "python3 available"

# Ensure FTP_PASS is available (either from env or frontend/.env)
if [ -z "${FTP_PASS:-}" ]; then
    if [ -f "${FRONTEND_DIR}/.env" ]; then
        # shellcheck disable=SC1090
        set -o allexport
        source "${FRONTEND_DIR}/.env"
        set +o allexport
    fi
fi
if [ -z "${FTP_PASS:-}" ]; then
    fail "FTP_PASS not set. Export it or add FTP_PASS=... to frontend/.env"
fi
ok "FTP_PASS available"

# ---------------------------------------------------------------------------
# 2. Backup production DB + MinIO on VPS
# ---------------------------------------------------------------------------
step "Backing up production DB + MinIO on VPS"
${SSH} 'bash /opt/aust/backup.sh'
ok "Production backup complete"

# ---------------------------------------------------------------------------
# 3. Build backend Docker image locally
# ---------------------------------------------------------------------------
step "Building backend Docker image (Debian 12 / bookworm)"
docker build \
    -f "${PROJECT_DIR}/docker/Dockerfile.backend" \
    -t "${IMAGE_NAME}:latest" \
    "${PROJECT_DIR}"
ok "Image built: ${IMAGE_NAME}:latest"

# ---------------------------------------------------------------------------
# 4. Build frontend (bun build + inline CSS)
# ---------------------------------------------------------------------------
step "Building frontend (bun run build)"
cd "${FRONTEND_DIR}"
bun run build
ok "SvelteKit build complete"

step "Inlining CSS (inline-css.py)"
python3 inline-css.py
ok "CSS inlined"

cd "${PROJECT_DIR}"

# ---------------------------------------------------------------------------
# 5. Tag existing image as :previous (for rollback), then save new image
# ---------------------------------------------------------------------------
step "Tagging previous backend image for rollback"
if ${SSH} docker image inspect "${IMAGE_NAME}:latest" >/dev/null 2>&1; then
    ${SSH} "docker tag ${IMAGE_NAME}:latest ${IMAGE_NAME}:previous"
    ok "Tagged existing image as :previous"
else
    echo "  No existing image found — skipping rollback tag"
fi

step "Saving backend image to tarball"
docker save "${IMAGE_NAME}:latest" | gzip > "${TARBALL}"
TARBALL_SIZE=$(du -sh "${TARBALL}" | cut -f1)
ok "Tarball: ${TARBALL} (${TARBALL_SIZE})"

# ---------------------------------------------------------------------------
# 6. Upload image tarball to VPS
# ---------------------------------------------------------------------------
step "Uploading backend image to VPS"
${SCP} "${TARBALL}" "${VPS_USER}@${VPS_IP}:/tmp/aust_backend.tar.gz"
ok "Tarball uploaded"

# ---------------------------------------------------------------------------
# 7. Load image on VPS
# ---------------------------------------------------------------------------
step "Loading backend image on VPS"
${SSH} 'docker load < /tmp/aust_backend.tar.gz && rm /tmp/aust_backend.tar.gz'
ok "Image loaded on VPS"

# ---------------------------------------------------------------------------
# 8. Upload migrations
# ---------------------------------------------------------------------------
step "Uploading migrations"
${SSH} mkdir -p /opt/aust/migrations
${SCP} -r "${PROJECT_DIR}/migrations/"* "${VPS_USER}@${VPS_IP}:/opt/aust/migrations/"
ok "Migrations uploaded"

# ---------------------------------------------------------------------------
# 9. Restart backend container via compose
# ---------------------------------------------------------------------------
step "Restarting backend container"
${SSH} 'cd /opt/aust && docker compose up -d backend'
ok "docker compose up -d backend"

# ---------------------------------------------------------------------------
# 10. Deploy frontend to KAS via FTP
# ---------------------------------------------------------------------------
step "Deploying frontend to KAS via FTP"
cd "${FRONTEND_DIR}"
python3 deploy-full.py
ok "Frontend deployed to KAS"
cd "${PROJECT_DIR}"

# ---------------------------------------------------------------------------
# 11. Health check (backend)
# ---------------------------------------------------------------------------
step "Backend health check"
sleep 5
for i in $(seq 1 12); do
    if ${SSH} curl -sf http://localhost:8080/health >/dev/null 2>&1; then
        ok "Backend healthy (attempt ${i})"
        break
    fi
    if [ "${i}" -eq 12 ]; then
        echo -e "  ${RED}Health check failed — container logs:${RESET}"
        ${SSH} 'docker logs aust_backend --tail 50'
        echo ""
        echo -e "  ${RED}Rollback command:${RESET}"
        echo "  ssh -i ${SSH_KEY} ${VPS_USER}@${VPS_IP} \\"
        echo "    'docker tag ${IMAGE_NAME}:previous ${IMAGE_NAME}:latest && cd /opt/aust && docker compose up -d backend'"
        exit 1
    fi
    echo "  Attempt ${i}/12 — retrying in 5s..."
    sleep 5
done

rm -f "${TARBALL}"

echo -e "\n${GREEN}${BOLD}=============================="
echo "  Full deploy complete!"
echo "  Backend commit: $(git -C "${PROJECT_DIR}" rev-parse --short HEAD)"
echo "  Frontend commit: $(git -C "${FRONTEND_DIR}" rev-parse --short HEAD)"
echo "==============================${RESET}"
echo ""
echo "  Backend rollback (if needed):"
echo "  ssh -i ${SSH_KEY} ${VPS_USER}@${VPS_IP} \\"
echo "    'docker tag ${IMAGE_NAME}:previous ${IMAGE_NAME}:latest && cd /opt/aust && docker compose up -d backend'"
