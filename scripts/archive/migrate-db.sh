#!/usr/bin/env bash
# migrate-db.sh — Dump local postgres and restore on VPS
#
# Run from your LOCAL machine:
#   bash scripts/migrate-db.sh

set -euo pipefail

VPS_IP="72.62.89.179"
VPS_USER="root"
SSH_KEY="$HOME/.ssh/id_ed25519"
SSH="ssh -i ${SSH_KEY} -o StrictHostKeyChecking=no ${VPS_USER}@${VPS_IP}"
SCP="scp -i ${SSH_KEY} -o StrictHostKeyChecking=no"

DUMP_FILE="/tmp/aust_backend_$(date +%Y%m%d_%H%M%S).sql.gz"

GREEN="\033[0;32m"; BOLD="\033[1m"; RESET="\033[0m"
ok() { echo -e "  ${GREEN}OK${RESET}  ${1}"; }
step() { echo -e "\n${BOLD}>>> ${1}${RESET}"; }

echo -e "${BOLD}=============================="
echo "  AUST DB Migration"
echo "  Source: local aust_postgres"
echo "  Target: ${VPS_IP}"
echo -e "==============================${RESET}"

# ---------------------------------------------------------------------------
# 1. Dump local database
# ---------------------------------------------------------------------------
step "Dumping local database"
docker exec aust_postgres pg_dump -U aust -d aust_backend | gzip > "${DUMP_FILE}"
DUMP_SIZE=$(du -sh "${DUMP_FILE}" | cut -f1)
ok "Dump created: ${DUMP_FILE} (${DUMP_SIZE})"

# ---------------------------------------------------------------------------
# 2. Upload dump to VPS
# ---------------------------------------------------------------------------
step "Uploading dump to VPS"
$SCP "${DUMP_FILE}" "${VPS_USER}@${VPS_IP}:/tmp/aust_restore.sql.gz"
ok "Uploaded"

# ---------------------------------------------------------------------------
# 3. Restore on VPS
# ---------------------------------------------------------------------------
step "Restoring on VPS"
$SSH bash -s << 'REMOTE'
set -euo pipefail
# Drop and recreate the database
docker exec -i aust_postgres psql -U aust -c "DROP DATABASE IF EXISTS aust_backend;" postgres
docker exec -i aust_postgres psql -U aust -c "CREATE DATABASE aust_backend;" postgres
# Restore
gunzip -c /tmp/aust_restore.sql.gz | docker exec -i aust_postgres psql -U aust -d aust_backend
rm /tmp/aust_restore.sql.gz
echo "  Restore complete"
REMOTE
ok "Database restored"

# ---------------------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------------------
rm "${DUMP_FILE}"

echo -e "\n${GREEN}${BOLD}DB migration complete!${RESET}"
