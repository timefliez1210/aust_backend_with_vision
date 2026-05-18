#!/usr/bin/env bash
# deploy-prod.sh — Build backend image locally, push to VPS, restart container
#
# Steps:
#   1. Pre-flight checks
#   2. Backup production DB + MinIO on VPS (before any changes)
#   3. Build backend Docker image locally
#   4. Tag existing VPS image as :previous (rollback anchor)
#   5. Save + upload + load image on VPS
#   6. Upload migrations
#   7. Restart backend container (auto-migrates on startup)
#   8. Health check
#
# Run from your LOCAL machine (must be on main, clean working tree):
#   bash scripts/deploy-prod.sh

set -euo pipefail

VPS_IP="${VPS_IP:-72.62.89.179}"
VPS_USER="root"
SSH_KEY="$HOME/.ssh/id_ed25519"
SSH="ssh -i ${SSH_KEY} -o StrictHostKeyChecking=no ${VPS_USER}@${VPS_IP}"
SCP="scp -i ${SSH_KEY} -o StrictHostKeyChecking=no"

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
IMAGE_NAME="aust_backend"
FLASH_BOT_IMAGE="aust_flash_contact_bot"
TARBALL="/tmp/aust_backend.tar.gz"
FLASH_BOT_TARBALL="/tmp/aust_flash_contact_bot.tar.gz"

GREEN="\033[0;32m"; RED="\033[0;31m"; BOLD="\033[1m"; RESET="\033[0m"
ok()   { echo -e "  ${GREEN}OK${RESET}  ${1}"; }
fail() { echo -e "  ${RED}FAIL${RESET}  ${1}"; exit 1; }
step() { echo -e "\n${BOLD}>>> ${1}${RESET}"; }

echo -e "${BOLD}=============================="
echo "  AUST Docker Deploy"
echo "  Target: ${VPS_IP}"
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

# ---------------------------------------------------------------------------
# 2. Backup production DB + MinIO on VPS
# ---------------------------------------------------------------------------
step "Verifying flash-contact bot token on VPS"
if ! ${SSH} 'grep -q "^AUST__TELEGRAM__FLASH_CONTACT_BOT_TOKEN=" /opt/aust/.env'; then
    fail "AUST__TELEGRAM__FLASH_CONTACT_BOT_TOKEN missing from /opt/aust/.env on VPS — flash-contact-bot will crash-loop."
fi
ok "Flash-contact bot token present"

step "Backing up production DB + MinIO on VPS"
${SSH} 'bash /opt/aust/backup.sh'
ok "Production backup complete"

# ---------------------------------------------------------------------------
# 3. Build backend image locally
# ---------------------------------------------------------------------------
step "Building backend Docker image (Debian 12 / bookworm)"
docker build \
    -f "${PROJECT_DIR}/docker/Dockerfile.backend" \
    -t "${IMAGE_NAME}:latest" \
    "${PROJECT_DIR}"
ok "Image built: ${IMAGE_NAME}:latest"

step "Building flash-contact-bot Docker image"
docker build \
    -f "${PROJECT_DIR}/docker/Dockerfile.flash-contact-bot" \
    -t "${FLASH_BOT_IMAGE}:latest" \
    "${PROJECT_DIR}"
ok "Image built: ${FLASH_BOT_IMAGE}:latest"

# ---------------------------------------------------------------------------
# 4. Tag existing image as :previous (for rollback), then save new image
# ---------------------------------------------------------------------------
step "Tagging previous image for rollback"
if ${SSH} docker image inspect "${IMAGE_NAME}:latest" >/dev/null 2>&1; then
    ${SSH} "docker tag ${IMAGE_NAME}:latest ${IMAGE_NAME}:previous"
    ok "Tagged existing image as :previous"
else
    echo "  No existing image found — skipping rollback tag"
fi

step "Tagging previous flash-contact-bot image for rollback"
if ${SSH} docker image inspect "${FLASH_BOT_IMAGE}:latest" >/dev/null 2>&1; then
    ${SSH} "docker tag ${FLASH_BOT_IMAGE}:latest ${FLASH_BOT_IMAGE}:previous"
    ok "Tagged existing flash-bot image as :previous"
else
    echo "  No existing flash-bot image found — skipping rollback tag"
fi

step "Saving image to tarball"
docker save "${IMAGE_NAME}:latest" | gzip > "${TARBALL}"
TARBALL_SIZE=$(du -sh "${TARBALL}" | cut -f1)
ok "Tarball: ${TARBALL} (${TARBALL_SIZE})"

step "Saving flash-contact-bot image to tarball"
docker save "${FLASH_BOT_IMAGE}:latest" | gzip > "${FLASH_BOT_TARBALL}"
FLASH_BOT_TARBALL_SIZE=$(du -sh "${FLASH_BOT_TARBALL}" | cut -f1)
ok "Tarball: ${FLASH_BOT_TARBALL} (${FLASH_BOT_TARBALL_SIZE})"

# ---------------------------------------------------------------------------
# 4. Upload image tarball to VPS
# ---------------------------------------------------------------------------
step "Uploading image tarball to VPS"
${SCP} "${TARBALL}" "${VPS_USER}@${VPS_IP}:/tmp/aust_backend.tar.gz"
ok "Tarball uploaded"

step "Uploading flash-contact-bot tarball to VPS"
${SCP} "${FLASH_BOT_TARBALL}" "${VPS_USER}@${VPS_IP}:/tmp/aust_flash_contact_bot.tar.gz"
ok "Flash-bot tarball uploaded"

# ---------------------------------------------------------------------------
# 5. Load image on VPS
# ---------------------------------------------------------------------------
step "Loading image on VPS"
${SSH} 'docker load < /tmp/aust_backend.tar.gz && rm /tmp/aust_backend.tar.gz'
ok "Image loaded on VPS"

step "Loading flash-contact-bot image on VPS"
${SSH} 'docker load < /tmp/aust_flash_contact_bot.tar.gz && rm /tmp/aust_flash_contact_bot.tar.gz'
ok "Flash-bot image loaded on VPS"

# ---------------------------------------------------------------------------
# 6. Upload migrations
# ---------------------------------------------------------------------------
step "Uploading migrations"
${SSH} mkdir -p /opt/aust/migrations
${SCP} -r "${PROJECT_DIR}/migrations/"* "${VPS_USER}@${VPS_IP}:/opt/aust/migrations/"
ok "Migrations uploaded"

# ---------------------------------------------------------------------------
# 7. Restart backend container via compose
# ---------------------------------------------------------------------------
step "Restarting backend container"
${SSH} 'cd /opt/aust && docker compose up -d backend'
ok "docker compose up -d backend"

step "Restarting flash-contact-bot container"
${SSH} 'cd /opt/aust && docker compose up -d flash-contact-bot'
ok "docker compose up -d flash-contact-bot"

# ---------------------------------------------------------------------------
# 8. Health check
# ---------------------------------------------------------------------------
step "Health check"
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

rm -f "${TARBALL}" "${FLASH_BOT_TARBALL}"

echo -e "\n${GREEN}${BOLD}=============================="
echo "  Deploy complete!"
echo "  Commit: $(git -C "${PROJECT_DIR}" rev-parse --short HEAD)"
echo "==============================${RESET}"
echo ""
echo "  Rollback (if needed):"
echo "  ssh -i ${SSH_KEY} ${VPS_USER}@${VPS_IP} \\"
echo "    'docker tag ${IMAGE_NAME}:previous ${IMAGE_NAME}:latest && cd /opt/aust && docker compose up -d backend'"
