#!/usr/bin/env bash
# deploy-binary.sh — Build binary locally, push to VPS, restart service
#
# Replaces cargo build on the server (4GB RAM not enough for release build).
# Builds x86_64-unknown-linux-gnu locally, ships the binary over SSH.
#
# Run from your LOCAL machine:
#   bash scripts/deploy-binary.sh

set -euo pipefail

VPS_IP="72.62.89.179"
VPS_USER="root"
SSH_KEY="$HOME/.ssh/id_ed25519"
SSH="ssh -i ${SSH_KEY} -o StrictHostKeyChecking=no ${VPS_USER}@${VPS_IP}"
SCP="scp -i ${SSH_KEY} -o StrictHostKeyChecking=no"

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SERVICE_NAME="aust-backend"

GREEN="\033[0;32m"; BOLD="\033[1m"; RESET="\033[0m"
ok() { echo -e "  ${GREEN}OK${RESET}  ${1}"; }
step() { echo -e "\n${BOLD}>>> ${1}${RESET}"; }

echo -e "${BOLD}=============================="
echo "  AUST Binary Deploy"
echo "  Target: ${VPS_IP}"
echo "  $(date '+%Y-%m-%d %H:%M:%S')"
echo -e "==============================${RESET}"

# ---------------------------------------------------------------------------
# 1. Build release binary in Debian 12 container (matches VPS glibc)
# ---------------------------------------------------------------------------
step "Building release binary in Debian 12 container"
BUILD_DIR="/tmp/aust-vps-build"
mkdir -p "${BUILD_DIR}"

docker run --rm \
    -v "${PROJECT_DIR}:/workspace" \
    -v "${HOME}/.cargo/registry:/root/.cargo/registry" \
    -v "${BUILD_DIR}:/workspace/target" \
    -w /workspace \
    rust:1-bookworm \
    bash -c "apt-get install -y -qq pkg-config libssl-dev && cargo build --release --bin aust_backend"

BINARY="${BUILD_DIR}/release/aust_backend"
BINARY_SIZE=$(du -sh "${BINARY}" | cut -f1)
ok "Binary built: ${BINARY_SIZE}"

# ---------------------------------------------------------------------------
# 3. Copy migrations to VPS
# ---------------------------------------------------------------------------
step "Uploading migrations"
$SSH mkdir -p /opt/aust/migrations
$SCP -r "${PROJECT_DIR}/migrations/"* "${VPS_USER}@${VPS_IP}:/opt/aust/migrations/"
ok "Migrations uploaded"

# ---------------------------------------------------------------------------
# 4. Install systemd service (first deploy only)
# ---------------------------------------------------------------------------
step "Ensuring systemd service exists"
$SSH bash -s << 'REMOTE'
if [ ! -f /etc/systemd/system/aust-backend.service ]; then
    cat > /etc/systemd/system/aust-backend.service << 'SERVICE'
[Unit]
Description=AUST Moving Company Backend
After=network.target docker.service
Requires=docker.service

[Service]
Type=simple
User=root
WorkingDirectory=/opt/aust
EnvironmentFile=/opt/aust/.env
ExecStart=/opt/aust/bin/aust_backend
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
SERVICE
    systemctl daemon-reload
    systemctl enable aust-backend
    echo "  Service created and enabled"
else
    echo "  Service already exists"
fi
REMOTE
ok "Systemd service ready"

# ---------------------------------------------------------------------------
# 5. Upload binary
# ---------------------------------------------------------------------------
step "Uploading binary"
$SCP "${BINARY}" "${VPS_USER}@${VPS_IP}:/opt/aust/bin/aust_backend"
$SSH chmod +x /opt/aust/bin/aust_backend
ok "Binary uploaded"

# ---------------------------------------------------------------------------
# 6. Restart service
# ---------------------------------------------------------------------------
step "Restarting service"
$SSH systemctl restart aust-backend
ok "Service restarted"

# ---------------------------------------------------------------------------
# 7. Health check
# ---------------------------------------------------------------------------
step "Health check"
sleep 3
for i in $(seq 1 10); do
    if $SSH curl -sf http://localhost:8080/health >/dev/null 2>&1; then
        ok "Backend healthy (attempt ${i})"
        break
    fi
    if [ "${i}" -eq 10 ]; then
        echo "Health check failed — checking logs:"
        $SSH journalctl -u aust-backend -n 30 --no-pager
        exit 1
    fi
    echo "  Attempt ${i}/10 — retrying in 3s..."
    sleep 3
done

echo -e "\n${GREEN}${BOLD}=============================="
echo "  Deploy complete!"
echo "  Commit: $(git -C "${PROJECT_DIR}" rev-parse --short HEAD)"
echo -e "==============================${RESET}"
