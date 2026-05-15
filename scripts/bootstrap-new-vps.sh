#!/usr/bin/env bash
# bootstrap-new-vps.sh — one-time setup for a brand-new VPS
#
# Installs Docker + Docker Compose, creates /opt/aust layout, drops in the
# production docker-compose.yml + .env template, installs cloudflared.
#
# Run from your LOCAL machine:
#   bash scripts/bootstrap-new-vps.sh <VPS_IP> [VPS_USER] [CLOUDFLARED_TOKEN]
#
# After it finishes:
#   1. Edit /opt/aust/.env on the VPS (real secrets)
#   2. Run scripts/migrate-from-laptop.sh to copy current data over
#   3. Run scripts/deploy-prod.sh (with VPS_IP override) for code updates

set -euo pipefail

VPS_IP="${1:?usage: $0 <VPS_IP> [VPS_USER] [CLOUDFLARED_TOKEN]}"
VPS_USER="${2:-root}"
CFD_TOKEN="${3:-}"

SSH="ssh -o StrictHostKeyChecking=accept-new ${VPS_USER}@${VPS_IP}"
SCP="scp -o StrictHostKeyChecking=accept-new"
PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

GREEN="\033[0;32m"; BOLD="\033[1m"; RESET="\033[0m"
step() { echo -e "\n${BOLD}>>> ${1}${RESET}"; }
ok()   { echo -e "  ${GREEN}OK${RESET}  ${1}"; }

step "Sanity: can we reach ${VPS_USER}@${VPS_IP}?"
${SSH} true
ok "SSH works"

step "Installing Docker + Compose + utilities"
${SSH} 'bash -s' <<'EOF'
set -e
export DEBIAN_FRONTEND=noninteractive
apt-get update -y
apt-get install -y ca-certificates curl gnupg lsb-release ufw
install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://download.docker.com/linux/debian/gpg | gpg --dearmor -o /etc/apt/keyrings/docker.gpg
chmod a+r /etc/apt/keyrings/docker.gpg
. /etc/os-release
echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/${ID} ${VERSION_CODENAME} stable" \
  > /etc/apt/sources.list.d/docker.list
apt-get update -y
apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
systemctl enable --now docker
EOF
ok "Docker installed"

step "Creating /opt/aust skeleton"
${SSH} 'mkdir -p /opt/aust/{migrations,backups,config,bin}'

step "Uploading compose file + migrations + backup script"
${SCP} "${PROJECT_DIR}/docker/docker-compose.prod.yml" "${VPS_USER}@${VPS_IP}:/opt/aust/docker-compose.yml" 2>/dev/null \
  || ${SCP} /tmp/aust-migration/prod-docker-compose.yml "${VPS_USER}@${VPS_IP}:/opt/aust/docker-compose.yml" 2>/dev/null \
  || echo "  (skip compose — provide one manually at /opt/aust/docker-compose.yml)"
${SCP} -r "${PROJECT_DIR}/migrations/." "${VPS_USER}@${VPS_IP}:/opt/aust/migrations/"
${SCP} "${PROJECT_DIR}/scripts/backup.sh" "${VPS_USER}@${VPS_IP}:/opt/aust/backup.sh"
${SSH} 'chmod +x /opt/aust/backup.sh'
ok "Compose + migrations + backup script in place"

step "Dropping .env template (FILL IN BEFORE FIRST BOOT)"
${SCP} "${PROJECT_DIR}/.env.example" "${VPS_USER}@${VPS_IP}:/opt/aust/.env.example"
${SSH} 'test -f /opt/aust/.env || cp /opt/aust/.env.example /opt/aust/.env'
echo "  Remember to edit /opt/aust/.env on the VPS before deploying."

step "Installing cloudflared"
${SSH} 'bash -s' <<'EOF'
set -e
if ! command -v cloudflared >/dev/null; then
  curl -fsSL https://pkg.cloudflare.com/cloudflare-main.gpg | tee /usr/share/keyrings/cloudflare-main.gpg >/dev/null
  echo "deb [signed-by=/usr/share/keyrings/cloudflare-main.gpg] https://pkg.cloudflare.com/cloudflared $(. /etc/os-release && echo "$VERSION_CODENAME") main" \
    > /etc/apt/sources.list.d/cloudflared.list
  apt-get update -y
  apt-get install -y cloudflared
fi
EOF
ok "cloudflared installed"

if [ -n "${CFD_TOKEN}" ]; then
  step "Registering cloudflared with provided tunnel token"
  ${SSH} "cloudflared service uninstall 2>/dev/null || true; cloudflared service install ${CFD_TOKEN}"
  ${SSH} 'systemctl enable --now cloudflared'
  ok "cloudflared running with new token"
else
  echo "  (no CLOUDFLARED_TOKEN passed — run 'cloudflared service install <token>' on the VPS manually)"
fi

step "Configuring firewall (ufw)"
${SSH} 'ufw allow OpenSSH; ufw --force enable' || true

echo -e "\n${GREEN}${BOLD}Bootstrap complete on ${VPS_IP}.${RESET}"
echo "Next steps:"
echo "  1. Edit /opt/aust/.env on the VPS"
echo "  2. bash scripts/migrate-from-laptop.sh ${VPS_IP} ${VPS_USER}"
echo "  3. VPS_IP=${VPS_IP} bash scripts/deploy-prod.sh"
