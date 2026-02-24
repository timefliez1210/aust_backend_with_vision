#!/usr/bin/env bash
set -euo pipefail

# Deploy script: backup DB → pull → build → restart → health check
# Usage: ./scripts/deploy.sh

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "${PROJECT_DIR}"

HEALTH_URL="http://localhost:8080/health"
SERVICE_NAME="aust-backend"
MAX_RETRIES=3
RETRY_DELAY=2

echo "========================================="
echo "  AUST Backend Deploy"
echo "  $(date '+%Y-%m-%d %H:%M:%S')"
echo "========================================="
echo ""

# 1. Pre-flight checks
echo "[1/6] Pre-flight checks..."

if ! docker info >/dev/null 2>&1; then
    echo "  ERROR: Docker is not running"
    exit 1
fi
echo "  Docker: OK"

if ! docker exec aust_postgres pg_isready -U aust -d aust_backend >/dev/null 2>&1; then
    echo "  ERROR: PostgreSQL is not ready"
    exit 1
fi
echo "  PostgreSQL: OK"

if ! command -v cargo >/dev/null 2>&1; then
    echo "  ERROR: cargo not found"
    exit 1
fi
echo "  Cargo: OK"

# Check for uncommitted changes
if [ -n "$(git status --porcelain)" ]; then
    echo "  ERROR: Uncommitted local changes detected. Commit or stash first."
    git status --short
    exit 1
fi
echo "  Working tree: clean"
echo ""

# 2. Backup database
echo "[2/6] Backing up database..."
bash "${PROJECT_DIR}/scripts/backup-db.sh"
echo ""

# 3. Git pull
echo "[3/6] Pulling latest changes..."
BEFORE=$(git rev-parse HEAD)
git pull --ff-only
AFTER=$(git rev-parse HEAD)

if [ "${BEFORE}" = "${AFTER}" ]; then
    echo "  Already up to date (${BEFORE:0:7})"
else
    echo "  Updated: ${BEFORE:0:7} → ${AFTER:0:7}"
    git log --oneline "${BEFORE}..${AFTER}"
fi
echo ""

# 4. Build release
echo "[4/6] Building release binary..."
cargo build --release 2>&1
echo "  Build: OK"
echo ""

# 5. Restart service
echo "[5/6] Restarting ${SERVICE_NAME}..."
sudo systemctl restart "${SERVICE_NAME}"
echo "  Service restarted"
echo ""

# 6. Health check
echo "[6/6] Health check..."
for i in $(seq 1 "${MAX_RETRIES}"); do
    sleep "${RETRY_DELAY}"
    if curl -sf "${HEALTH_URL}" >/dev/null 2>&1; then
        echo "  Health check passed (attempt ${i}/${MAX_RETRIES})"
        break
    fi
    if [ "${i}" -eq "${MAX_RETRIES}" ]; then
        echo "  ERROR: Health check failed after ${MAX_RETRIES} attempts"
        echo "  Check logs: journalctl -u ${SERVICE_NAME} -n 50"
        exit 1
    fi
    echo "  Attempt ${i}/${MAX_RETRIES} failed, retrying in ${RETRY_DELAY}s..."
done

echo ""
echo "========================================="
echo "  Deploy complete!"
echo "  Commit:  $(git rev-parse --short HEAD)"
echo "  Status:  $(systemctl is-active ${SERVICE_NAME})"
echo "  Backup:  $(ls -1t backups/db/aust_backend_*.sql.gz 2>/dev/null | head -1)"
echo "========================================="
