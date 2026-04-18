#!/usr/bin/env bash
# pull-backups.sh — Download latest backups from VPS to local machine
#
# Run from your LOCAL machine (requires ProtonVPN):
#   bash scripts/pull-backups.sh

set -euo pipefail

VPS_IP="72.62.89.179"
VPS_USER="root"
SSH_KEY="$HOME/.ssh/id_ed25519"
SSH="ssh -i ${SSH_KEY} -o StrictHostKeyChecking=no ${VPS_USER}@${VPS_IP}"

LOCAL_DIR="${HOME}/aust-backups"
mkdir -p "${LOCAL_DIR}"

GREEN="\033[0;32m"; BOLD="\033[1m"; RESET="\033[0m"
ok() { echo -e "  ${GREEN}OK${RESET}  ${1}"; }
step() { echo -e "\n${BOLD}>>> ${1}${RESET}"; }

echo -e "${BOLD}=============================="
echo "  AUST Backup Pull"
echo "  From: ${VPS_IP}:/opt/aust/backups/"
echo "  To:   ${LOCAL_DIR}"
echo -e "==============================${RESET}"

step "Available backups on VPS"
$SSH ls -lh /opt/aust/backups/ 2>/dev/null || echo "  (no backups yet)"

step "Syncing to ${LOCAL_DIR}"
rsync -avz --progress \
    -e "ssh -i ${SSH_KEY} -o StrictHostKeyChecking=no" \
    "${VPS_USER}@${VPS_IP}:/opt/aust/backups/" \
    "${LOCAL_DIR}/"

ok "Sync complete"

echo -e "\n${GREEN}${BOLD}Local backups:${RESET}"
ls -lh "${LOCAL_DIR}"
