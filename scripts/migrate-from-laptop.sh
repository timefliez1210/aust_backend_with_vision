#!/usr/bin/env bash
# migrate-from-laptop.sh — copy live laptop data (Postgres + MinIO) to a fresh VPS
#
# Pre-req: scripts/bootstrap-new-vps.sh already ran on the target. The compose
# stack on the VPS must be up so that aust_postgres and aust_minio exist.
#
# Run from your LOCAL machine:
#   bash scripts/migrate-from-laptop.sh <VPS_IP> [VPS_USER]

set -euo pipefail

VPS_IP="${1:?usage: $0 <VPS_IP> [VPS_USER]}"
VPS_USER="${2:-root}"
SSH="ssh -o StrictHostKeyChecking=accept-new ${VPS_USER}@${VPS_IP}"
SCP="scp -o StrictHostKeyChecking=accept-new"

GREEN="\033[0;32m"; BOLD="\033[1m"; RESET="\033[0m"
step() { echo -e "\n${BOLD}>>> ${1}${RESET}"; }
ok()   { echo -e "  ${GREEN}OK${RESET}  ${1}"; }

TMP=/tmp/aust-handover-$$
mkdir -p "${TMP}"
trap 'rm -rf "${TMP}"' EXIT

step "Stopping local backend + flash-bot so no new writes during snapshot"
sudo systemctl stop aust-backend aust-flash-bot

step "Dumping local Postgres"
docker exec aust_postgres pg_dump -U aust -d aust_backend --clean --if-exists --no-owner > "${TMP}/db.sql"
ok "DB dump: $(wc -c < "${TMP}/db.sql") bytes"

step "Snapshotting local MinIO data"
rm -rf "${TMP}/minio"
docker cp aust_minio:/data/aust-uploads "${TMP}/minio"
tar czf "${TMP}/minio.tgz" -C "${TMP}/minio" .
ok "MinIO tar: $(wc -c < "${TMP}/minio.tgz") bytes"

step "Bringing local services back up (cutover stays on this laptop until cloudflared is repointed)"
sudo systemctl start aust-backend aust-flash-bot

step "Ensuring VPS docker stack is running"
${SSH} 'cd /opt/aust && docker compose up -d postgres minio minio-setup'
sleep 5

step "Restoring Postgres on VPS"
cat "${TMP}/db.sql" | ${SSH} 'docker exec -i aust_postgres psql -U aust -d postgres -c "DROP DATABASE IF EXISTS aust_backend WITH (FORCE); CREATE DATABASE aust_backend OWNER aust;" && docker exec -i aust_postgres psql -U aust -d aust_backend' > /dev/null
ok "DB restored on VPS"

step "Restoring MinIO on VPS"
${SCP} "${TMP}/minio.tgz" "${VPS_USER}@${VPS_IP}:/tmp/minio.tgz"
${SSH} 'docker exec aust_minio sh -c "rm -rf /data/aust-uploads/* && mkdir -p /data/aust-uploads"'
# Extract on the VPS host (MinIO container has no tar)
${SSH} 'mkdir -p /tmp/minio_extract && tar xzf /tmp/minio.tgz -C /tmp/minio_extract && docker cp /tmp/minio_extract/. aust_minio:/data/aust-uploads && rm -rf /tmp/minio_extract /tmp/minio.tgz'
ok "MinIO restored on VPS"

echo -e "\n${GREEN}${BOLD}Data on the VPS now matches laptop.${RESET}"
echo "Cutover when ready:"
echo "  1. Stop laptop services:   sudo systemctl stop aust-backend aust-flash-bot"
echo "  2. Stop laptop cloudflared:sudo systemctl stop cloudflared"
echo "  3. Confirm VPS cloudflared is serving (token already installed by bootstrap)"
echo "  4. Verify: curl https://api.aufraeumhelden.com/health"
