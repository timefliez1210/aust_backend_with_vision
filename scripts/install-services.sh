#!/usr/bin/env bash
set -euo pipefail

# One-time setup: install systemd units for aust-backend and daily backups.
# Run this after any fresh deploy or if the service files change.
#
# Usage: sudo ./scripts/install-services.sh

if [[ $EUID -ne 0 ]]; then
    echo "ERROR: run with sudo"
    echo "  sudo ./scripts/install-services.sh"
    exit 1
fi

SCRIPTS_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "Installing systemd units..."

cp "${SCRIPTS_DIR}/aust-backup.service" /etc/systemd/system/
cp "${SCRIPTS_DIR}/aust-backup.timer"   /etc/systemd/system/
cp "${SCRIPTS_DIR}/../docker/aust-backend.service" /etc/systemd/system/ 2>/dev/null || true

systemctl daemon-reload

# Backup timer
systemctl enable --now aust-backup.timer
echo "  aust-backup.timer enabled (runs daily at 03:00)"
systemctl list-timers aust-backup --no-pager

# Backend service
if systemctl list-unit-files aust-backend.service >/dev/null 2>&1; then
    systemctl enable aust-backend
    echo "  aust-backend.service enabled"
fi

echo ""
echo "Done. Verify with:"
echo "  systemctl status aust-backend"
echo "  systemctl list-timers aust-backup"
