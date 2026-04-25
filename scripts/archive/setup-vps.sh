#!/usr/bin/env bash
# setup-vps.sh — One-time VPS provisioning (run once after fresh Debian 12 install)
#
# Installs: Docker, Docker Compose plugin, 4GB swap
# Creates:  /opt/aust directory structure
# Copies:   docker-compose.prod.yml + .env to server
# Starts:   postgres + minio containers
#
# Run from your LOCAL machine (not on the server):
#   bash scripts/setup-vps.sh

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
echo "  AUST VPS Setup"
echo "  Target: ${VPS_IP}"
echo -e "==============================${RESET}"

# ---------------------------------------------------------------------------
# 1. Install Docker
# ---------------------------------------------------------------------------
step "Installing Docker + Compose plugin"
$SSH bash -s << 'REMOTE'
set -euo pipefail
apt-get update -qq
apt-get install -y -qq ca-certificates curl gnupg lsb-release

install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://download.docker.com/linux/debian/gpg | gpg --dearmor -o /etc/apt/keyrings/docker.gpg
chmod a+r /etc/apt/keyrings/docker.gpg

echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] \
  https://download.docker.com/linux/debian $(lsb_release -cs) stable" \
  > /etc/apt/sources.list.d/docker.list

apt-get update -qq
apt-get install -y -qq docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin

systemctl enable docker --now
docker --version
docker compose version
REMOTE
ok "Docker installed"

# ---------------------------------------------------------------------------
# 2. Add 4GB swap
# ---------------------------------------------------------------------------
step "Adding 4GB swap"
$SSH bash -s << 'REMOTE'
set -euo pipefail
if [ -f /swapfile ]; then
    echo "  Swap already exists, skipping"
else
    fallocate -l 4G /swapfile
    chmod 600 /swapfile
    mkswap /swapfile
    swapon /swapfile
    echo '/swapfile none swap sw 0 0' >> /etc/fstab
    echo "  Swap created: $(free -h | grep Swap)"
fi
REMOTE
ok "Swap configured"

# ---------------------------------------------------------------------------
# 3. Create directory structure
# ---------------------------------------------------------------------------
step "Creating /opt/aust directory"
$SSH bash -s << 'REMOTE'
mkdir -p /opt/aust/{bin,backups,migrations}
REMOTE
ok "Directories created"

# ---------------------------------------------------------------------------
# 4. Upload docker-compose.prod.yml + config files
# ---------------------------------------------------------------------------
step "Uploading docker-compose.prod.yml"
$SCP "${PROJECT_DIR}/docker/docker-compose.prod.yml" "${VPS_USER}@${VPS_IP}:/opt/aust/docker-compose.yml"
ok "docker-compose.yml uploaded"

$SSH mkdir -p /opt/aust/config
$SCP "${PROJECT_DIR}/config/default.toml" "${PROJECT_DIR}/config/production.toml" "${VPS_USER}@${VPS_IP}:/opt/aust/config/"
ok "config files uploaded"

# ---------------------------------------------------------------------------
# 5. Upload .env (with production DATABASE_URL)
# ---------------------------------------------------------------------------
step "Uploading .env"
# Build a prod .env: swap DATABASE_URL to point at localhost postgres
ENV_SRC="${PROJECT_DIR}/.env"
ENV_TMP=$(mktemp)
# Copy .env but override the database URL to use Docker-internal postgres
grep -v "^AUST__DATABASE__URL=" "${ENV_SRC}" | grep -v "^VPS_PASS=" | grep -v "^AGENT_" > "${ENV_TMP}"
echo "AUST__DATABASE__URL=postgres://aust:aust_dev_password@localhost:5432/aust_backend" >> "${ENV_TMP}"
echo "RUN_MODE=production" >> "${ENV_TMP}" || true
$SCP "${ENV_TMP}" "${VPS_USER}@${VPS_IP}:/opt/aust/.env"
rm "${ENV_TMP}"
ok ".env uploaded"

# ---------------------------------------------------------------------------
# 6. Start postgres + minio
# ---------------------------------------------------------------------------
step "Starting postgres + minio containers"
$SSH bash -s << 'REMOTE'
cd /opt/aust
docker compose up -d postgres minio minio-setup
echo "  Waiting for postgres..."
until docker exec aust_postgres pg_isready -U aust -d aust_backend 2>/dev/null; do sleep 2; done
echo "  Postgres ready"
REMOTE
ok "Containers running"

echo -e "\n${GREEN}${BOLD}=============================="
echo "  VPS setup complete!"
echo "  Next steps:"
echo "    1. bash scripts/migrate-db.sh   — copy local DB to VPS"
echo "    2. bash scripts/deploy-binary.sh — build + push binary"
echo -e "==============================${RESET}"
