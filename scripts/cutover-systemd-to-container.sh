#!/usr/bin/env bash
# cutover-systemd-to-container.sh — One-time migration: systemd binary → Docker container
#
# This script stops the aust-backend systemd service, uploads the new compose file,
# and starts the backend container. The binary under /opt/aust/bin remains untouched
# for emergency rollback via `systemctl start aust-backend`.
#
# Run from your LOCAL machine:
#   bash scripts/cutover-systemd-to-container.sh

set -euo pipefail

VPS_IP="72.62.89.179"
VPS_USER="root"
SSH_KEY="$HOME/.ssh/id_ed25519"
SSH="ssh -i ${SSH_KEY} -o StrictHostKeyChecking=no ${VPS_USER}@${VPS_IP}"
SCP="scp -i ${SSH_KEY} -o StrictHostKeyChecking=no"

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

GREEN="\033[0;32m"; RED="\033[0;31m"; BOLD="\033[1m"; RESET="\033[0m"
ok()   { echo -e "  ${GREEN}OK${RESET}  ${1}"; }
fail() { echo -e "  ${RED}FAIL${RESET}  ${1}"; exit 1; }
step() { echo -e "\n${BOLD}>>> ${1}${RESET}"; }

echo -e "${BOLD}=============================="
echo "  AUST Cutover: systemd → Docker"
echo "  Target: ${VPS_IP}"
echo "  $(date '+%Y-%m-%d %H:%M:%S')"
echo -e "==============================${RESET}"
echo ""
echo "  This stops aust-backend systemd and starts the Docker container."
echo "  Expected downtime: ~30 seconds."
echo "  The binary at /opt/aust/bin/aust_backend is NOT deleted."
echo "  Emergency rollback: systemctl start aust-backend"
echo ""
read -r -p "Continue? [y/N] " CONFIRM
if [[ "${CONFIRM}" != "y" && "${CONFIRM}" != "Y" ]]; then
    echo "Aborted."
    exit 0
fi

# ---------------------------------------------------------------------------
# 1. Verify SSH
# ---------------------------------------------------------------------------
step "Verifying SSH access"
if ! ${SSH} true 2>/dev/null; then
    fail "Cannot reach VPS via SSH (${VPS_USER}@${VPS_IP})"
fi
ok "SSH works"

# ---------------------------------------------------------------------------
# 2. Stop and disable systemd service
# ---------------------------------------------------------------------------
step "Stopping and disabling aust-backend systemd service"
${SSH} 'systemctl stop aust-backend && systemctl disable aust-backend'
ok "aust-backend systemd service stopped and disabled"

# ---------------------------------------------------------------------------
# 3. Upload new docker-compose.yml (includes backend service)
# ---------------------------------------------------------------------------
step "Uploading new docker-compose.yml to VPS"
${SCP} "${PROJECT_DIR}/docker/docker-compose.prod.yml" \
    "${VPS_USER}@${VPS_IP}:/opt/aust/docker-compose.yml"
ok "docker-compose.yml uploaded"

# ---------------------------------------------------------------------------
# 4. Run deploy (build image + upload + start container)
# ---------------------------------------------------------------------------
step "Running deploy-prod.sh"
bash "${PROJECT_DIR}/scripts/deploy-prod.sh"

# ---------------------------------------------------------------------------
# 5. Verify via public URL
# ---------------------------------------------------------------------------
step "Verifying public endpoint"
sleep 3
if curl -sf https://aufraeumhelden.com/health >/dev/null 2>&1; then
    ok "https://aufraeumhelden.com/health responded"
else
    echo -e "  ${RED}Public health check failed.${RESET}"
    echo ""
    echo "  Check container logs:"
    echo "  ${SSH} 'docker logs aust_backend --tail 50'"
    echo ""
    echo "  Emergency rollback (re-enables the original systemd binary):"
    echo "    ${SSH} 'systemctl enable aust-backend && systemctl start aust-backend'"
    exit 1
fi

echo -e "\n${GREEN}${BOLD}=============================="
echo "  Cutover complete!"
echo "  Backend is now running as a Docker container."
echo "==============================${RESET}"
echo ""
echo "  The old binary is still at /opt/aust/bin/aust_backend."
echo "  To clean it up later:"
echo "    ${SSH} 'rm /opt/aust/bin/aust_backend'"
