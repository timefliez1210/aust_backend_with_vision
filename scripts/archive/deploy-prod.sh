#!/usr/bin/env bash
# deploy-prod.sh — Build backend image locally, push to VPS, restart container
#
# Replaces deploy-binary.sh for the Docker-based production stack.
# Run from your LOCAL machine:
#   bash scripts/deploy-prod.sh

set -euo pipefail

VPS_IP="72.62.89.179"
VPS_USER="root"
SSH_KEY="$HOME/.ssh/id_ed25519"
SSH="ssh -i ${SSH_KEY} -o StrictHostKeyChecking=no ${VPS_USER}@${VPS_IP}"
SCP="scp -i ${SSH_KEY} -o StrictHostKeyChecking=no"

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
IMAGE_NAME="aust_backend"
TARBALL="/tmp/aust_backend.tar.gz"

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
# 2. Build backend image locally
# ---------------------------------------------------------------------------
step "Building backend Docker image (Debian 12 / bookworm)"
docker build \
    -f "${PROJECT_DIR}/docker/Dockerfile.backend" \
    -t "${IMAGE_NAME}:latest" \
    "${PROJECT_DIR}"
ok "Image built: ${IMAGE_NAME}:latest"

# ---------------------------------------------------------------------------
# 3. Tag existing image as :previous (for rollback), then save new image
# ---------------------------------------------------------------------------
step "Tagging previous image for rollback"
if ${SSH} docker image inspect "${IMAGE_NAME}:latest" >/dev/null 2>&1; then
    ${SSH} "docker tag ${IMAGE_NAME}:latest ${IMAGE_NAME}:previous"
    ok "Tagged existing image as :previous"
else
    echo "  No existing image found — skipping rollback tag"
fi

step "Saving image to tarball"
docker save "${IMAGE_NAME}:latest" | gzip > "${TARBALL}"
TARBALL_SIZE=$(du -sh "${TARBALL}" | cut -f1)
ok "Tarball: ${TARBALL} (${TARBALL_SIZE})"

# ---------------------------------------------------------------------------
# 4. Upload image tarball to VPS
# ---------------------------------------------------------------------------
step "Uploading image tarball to VPS"
${SCP} "${TARBALL}" "${VPS_USER}@${VPS_IP}:/tmp/aust_backend.tar.gz"
ok "Tarball uploaded"

# ---------------------------------------------------------------------------
# 5. Load image on VPS
# ---------------------------------------------------------------------------
step "Loading image on VPS"
${SSH} 'docker load < /tmp/aust_backend.tar.gz && rm /tmp/aust_backend.tar.gz'
ok "Image loaded on VPS"

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

rm -f "${TARBALL}"

echo -e "\n${GREEN}${BOLD}=============================="
echo "  Deploy complete!"
echo "  Commit: $(git -C "${PROJECT_DIR}" rev-parse --short HEAD)"
echo "==============================${RESET}"
echo ""
echo "  Rollback (if needed):"
echo "  ssh -i ${SSH_KEY} ${VPS_USER}@${VPS_IP} \\"
echo "    'docker tag ${IMAGE_NAME}:previous ${IMAGE_NAME}:latest && cd /opt/aust && docker compose up -d backend'"
