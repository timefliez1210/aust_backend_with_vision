#!/usr/bin/env bash
# setup-local-backup-cron.sh — Install daily local cron to pull VPS backups
#
# Run from your LOCAL dev machine:
#   bash scripts/setup-local-backup-cron.sh
#
# Installs a user-level cron entry (04:00 daily) that:
#   1. Runs pull-backups.sh to rsync backups from VPS → ~/aust-backups/
#   2. Prunes local backups older than 60 days

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PULL_SCRIPT="${SCRIPT_DIR}/pull-backups.sh"
LOCAL_DIR="${HOME}/aust-backups"
RETAIN_LOCAL_DAYS=60
LOG_FILE="${HOME}/aust-backups/pull-cron.log"

GREEN="\033[0;32m"; BOLD="\033[1m"; YELLOW="\033[1;33m"; RESET="\033[0m"
ok()   { echo -e "  ${GREEN}OK${RESET}  ${1}"; }
warn() { echo -e "  ${YELLOW}WARN${RESET} ${1}"; }
step() { echo -e "\n${BOLD}>>> ${1}${RESET}"; }

echo -e "${BOLD}=============================="
echo "  AUST Local Backup Cron Setup"
echo "  Pull script: ${PULL_SCRIPT}"
echo "  Local dir:   ${LOCAL_DIR}"
echo "  Retention:   ${RETAIN_LOCAL_DAYS} days"
echo -e "==============================${RESET}"

if [[ ! -f "${PULL_SCRIPT}" ]]; then
    echo "ERROR: pull-backups.sh not found at ${PULL_SCRIPT}"
    exit 1
fi

chmod +x "${PULL_SCRIPT}"
mkdir -p "${LOCAL_DIR}"

# ---------------------------------------------------------------------------
# Check for existing cron entry — do not duplicate
# ---------------------------------------------------------------------------
step "Checking for existing cron entries"
EXISTING=$(crontab -l 2>/dev/null | grep -F "pull-backups.sh" || true)
if [[ -n "${EXISTING}" ]]; then
    warn "A cron entry for pull-backups.sh already exists:"
    echo "  ${EXISTING}"
    echo ""
    echo "No changes made. Remove the existing entry first if you want to reinstall."
    exit 0
fi
ok "No existing entry found"

# ---------------------------------------------------------------------------
# Build the cron command:
#   - run pull-backups.sh
#   - prune local backups older than RETAIN_LOCAL_DAYS days
#   - log output
# ---------------------------------------------------------------------------
PRUNE_CMD="find ${LOCAL_DIR} -maxdepth 1 \\( -name 'postgres_*.sql.gz' -o -name 'minio_*.tar.gz' \\) -mtime +${RETAIN_LOCAL_DAYS} -delete"
CRON_CMD="0 4 * * * mkdir -p ${LOCAL_DIR} && bash ${PULL_SCRIPT} >> ${LOG_FILE} 2>&1 && ${PRUNE_CMD} >> ${LOG_FILE} 2>&1"

step "Installing cron entry (daily 04:00)"
(crontab -l 2>/dev/null; echo "${CRON_CMD}") | crontab -
ok "Cron entry installed"

echo ""
echo "Installed entry:"
crontab -l | grep "pull-backups.sh"

echo -e "\n${GREEN}${BOLD}=============================="
echo "  Local backup cron setup complete!"
echo "  Schedule:  daily at 04:00 local time"
echo "  Pulls to:  ${LOCAL_DIR}"
echo "  Retention: ${RETAIN_LOCAL_DAYS} days locally"
echo "  Logs:      ${LOG_FILE}"
echo ""
echo "  To pull right now:  bash ${PULL_SCRIPT}"
echo -e "==============================${RESET}"
