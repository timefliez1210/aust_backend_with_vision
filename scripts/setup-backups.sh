#!/usr/bin/env bash
# setup-backups.sh — Install daily backup cron job on VPS
#
# Run from your LOCAL machine (requires ProtonVPN):
#   bash scripts/setup-backups.sh

set -euo pipefail

VPS_IP="72.62.89.179"
VPS_USER="root"
SSH_KEY="$HOME/.ssh/id_ed25519"
SSH="ssh -i ${SSH_KEY} -o StrictHostKeyChecking=no ${VPS_USER}@${VPS_IP}"
SCP="scp -i ${SSH_KEY} -o StrictHostKeyChecking=no"

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

GREEN="\033[0;32m"; BOLD="\033[1m"; RESET="\033[0m"
ok() { echo -e "  ${GREEN}OK${RESET}  ${1}"; }
step() { echo -e "\n${BOLD}>>> ${1}${RESET}"; }

echo -e "${BOLD}=============================="
echo "  AUST Backup Setup"
echo "  Target: ${VPS_IP}"
echo -e "==============================${RESET}"

# ---------------------------------------------------------------------------
# 1. Upload backup script
# ---------------------------------------------------------------------------
step "Uploading backup script"
$SCP "${PROJECT_DIR}/scripts/backup.sh" "${VPS_USER}@${VPS_IP}:/opt/aust/backup.sh"
$SSH chmod +x /opt/aust/backup.sh
ok "backup.sh uploaded"

# ---------------------------------------------------------------------------
# 2. Create /opt/aust/backups directory
# ---------------------------------------------------------------------------
step "Creating backup directory"
$SSH mkdir -p /opt/aust/backups
ok "Directory ready"

# ---------------------------------------------------------------------------
# 3. Install cron job (03:00 daily, logs to /var/log/aust-backup.log)
# ---------------------------------------------------------------------------
step "Installing cron job"
$SSH bash -s << 'REMOTE'
# Remove any existing aust backup cron entry, then add fresh one
crontab -l 2>/dev/null | grep -v "aust/backup.sh" | { cat; echo "0 3 * * * /opt/aust/backup.sh >> /var/log/aust-backup.log 2>&1"; } | crontab -
echo "  Cron entry:"
crontab -l | grep backup
REMOTE
ok "Cron job installed (daily 03:00)"

# ---------------------------------------------------------------------------
# 4. Run once now to verify it works
# ---------------------------------------------------------------------------
step "Running backup now to verify"
$SSH /opt/aust/backup.sh
ok "Initial backup successful"

echo -e "\n${GREEN}${BOLD}=============================="
echo "  Backup setup complete!"
echo "  Schedule: daily at 03:00 VPS time"
echo "  Location: /opt/aust/backups/"
echo "  Retention: 7 days"
echo "  Logs: /var/log/aust-backup.log"
echo "  Pull locally: bash scripts/pull-backups.sh"
echo -e "==============================${RESET}"
