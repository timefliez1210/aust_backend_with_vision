#!/usr/bin/env bash
# staging-up.sh — One-command wrapper for the AUST local staging stack.
#
# Usage:
#   bash scripts/staging-up.sh                # start stack (no rebuild, no restore)
#   bash scripts/staging-up.sh --rebuild      # force --build on backend + frontend
#   bash scripts/staging-up.sh --restore      # pull latest VPS backup then restore
#   bash scripts/staging-up.sh --restore-only # restore from existing local backup (skip pull)
#   bash scripts/staging-up.sh --down         # stop containers (preserve volumes)
#   bash scripts/staging-up.sh --nuke         # stop and DELETE all staging volumes
#   bash scripts/staging-up.sh --logs         # tail backend + frontend logs

set -euo pipefail

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
COMPOSE_FILE="${PROJECT_ROOT}/docker/docker-compose.staging.yml"

# ---------------------------------------------------------------------------
# Colors (only when stdout is a terminal)
# ---------------------------------------------------------------------------
if [ -t 1 ]; then
    GREEN="\033[0;32m"
    RED="\033[0;31m"
    YELLOW="\033[0;33m"
    BOLD="\033[1m"
    RESET="\033[0m"
else
    GREEN=""
    RED=""
    YELLOW=""
    BOLD=""
    RESET=""
fi

step() { echo -e "\n${BOLD}>>> ${1}${RESET}"; }
ok()   { echo -e "  ${GREEN}OK${RESET}  ${1}"; }
warn() { echo -e "  ${YELLOW}WARN${RESET} ${1}"; }
fail() { echo -e "  ${RED}FAIL${RESET} ${1}" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
BACKEND_URL="http://localhost:8099"
FRONTEND_URL="http://localhost:4173"
MAILPIT_URL="http://localhost:8025"
MINIO_URL="http://localhost:9011"
HEALTH_TIMEOUT=120
HEALTH_INTERVAL=5

# Container names (from docker-compose.staging.yml)
BACKEND_CONTAINER="aust_staging_backend"
POSTGRES_CONTAINER="aust_staging_postgres"

# ---------------------------------------------------------------------------
# Helper: docker compose wrapper
# ---------------------------------------------------------------------------
dc() {
    docker compose -f "${COMPOSE_FILE}" "$@"
}

# ---------------------------------------------------------------------------
# Helper: wait for a container's docker healthcheck to report "healthy"
# ---------------------------------------------------------------------------
wait_healthy() {
    local container="$1"
    local label="$2"
    local elapsed=0

    while true; do
        local status
        status=$(docker inspect --format '{{.State.Health.Status}}' "${container}" 2>/dev/null || echo "missing")

        case "${status}" in
            healthy)
                ok "${label} is healthy"
                return 0
                ;;
            unhealthy)
                fail "${label} reported unhealthy — check: bash scripts/staging-up.sh --logs"
                ;;
        esac

        if [ "${elapsed}" -ge "${HEALTH_TIMEOUT}" ]; then
            fail "${label} did not become healthy within ${HEALTH_TIMEOUT}s"
        fi

        echo -e "  ${YELLOW}...${RESET} waiting for ${label} (${elapsed}s / ${HEALTH_TIMEOUT}s)"
        sleep "${HEALTH_INTERVAL}"
        elapsed=$((elapsed + HEALTH_INTERVAL))
    done
}

# ---------------------------------------------------------------------------
# Helper: print URL summary box
# ---------------------------------------------------------------------------
print_summary() {
    local line="+-------------------------------------------------+"
    echo -e "\n${GREEN}${BOLD}${line}${RESET}"
    printf "${GREEN}${BOLD}|  %-47s |${RESET}\n" "Staging stack is ready"
    printf "${GREEN}${BOLD}|  %-47s |${RESET}\n" ""
    printf "${GREEN}${BOLD}|  %-47s |${RESET}\n" "Backend   : ${BACKEND_URL}"
    printf "${GREEN}${BOLD}|  %-47s |${RESET}\n" "Frontend  : ${FRONTEND_URL}"
    printf "${GREEN}${BOLD}|  %-47s |${RESET}\n" "Mailpit   : ${MAILPIT_URL}"
    printf "${GREEN}${BOLD}|  %-47s |${RESET}\n" "MinIO UI  : ${MINIO_URL}"
    echo -e "${GREEN}${BOLD}${line}${RESET}\n"
}

# ---------------------------------------------------------------------------
# Subcommand: up [--rebuild]
# Default: docker compose up -d (no --build).
# --rebuild: pass --build to force image rebuild.
#
# Note: We do NOT auto-detect mtime vs container build time — too fragile
# across layer caches and volume timestamps. Use --rebuild whenever you
# change backend or frontend source. The default (no flag) is intentionally
# fast: it just starts the already-built images.
# ---------------------------------------------------------------------------
cmd_up() {
    local rebuild="${1:-0}"

    step "Starting staging stack"

    if [ "${rebuild}" -eq 1 ]; then
        ok "Mode: force rebuild (--rebuild)"
        dc up -d --build staging-backend staging-frontend
    else
        ok "Mode: start existing images (pass --rebuild to force image build)"
        dc up -d
    fi

    step "Waiting for containers to become healthy"
    wait_healthy "${POSTGRES_CONTAINER}" "postgres"
    wait_healthy "${BACKEND_CONTAINER}"  "backend"

    print_summary
}

# ---------------------------------------------------------------------------
# Subcommand: down
# ---------------------------------------------------------------------------
cmd_down() {
    step "Stopping staging stack (volumes preserved)"
    dc down
    ok "All staging containers stopped"
}

# ---------------------------------------------------------------------------
# Subcommand: nuke (requires confirmation)
# ---------------------------------------------------------------------------
cmd_nuke() {
    echo -e "\n${RED}${BOLD}WARNING: This will DELETE all staging volumes (postgres + minio data).${RESET}"
    echo    "         There is no undo. Restore from backup if needed."
    echo ""
    read -rp "Type 'yes' to confirm: " reply
    [[ "${reply}" == "yes" ]] || fail "Aborted"

    step "Nuking staging stack and volumes"
    dc down -v
    ok "Staging stack destroyed (all volumes deleted)"
}

# ---------------------------------------------------------------------------
# Subcommand: restore (pull + restore)
# ---------------------------------------------------------------------------
cmd_restore() {
    local skip_pull="${1:-0}"

    if [ "${skip_pull}" -eq 0 ]; then
        step "Pulling latest backups from VPS"
        bash "${SCRIPT_DIR}/pull-backups.sh"
    else
        ok "Skipping VPS pull (--restore-only)"
    fi

    # Ensure the postgres + minio containers are running before restore
    step "Ensuring staging postgres + minio are running"
    dc up -d staging-postgres staging-minio staging-minio-setup
    wait_healthy "${POSTGRES_CONTAINER}" "postgres"

    step "Restoring backup into staging containers"
    bash "${SCRIPT_DIR}/restore-local.sh" -y
    ok "Restore complete"
}

# ---------------------------------------------------------------------------
# Subcommand: logs
# ---------------------------------------------------------------------------
cmd_logs() {
    step "Tailing staging backend + frontend logs (Ctrl+C to stop)"
    dc logs -f staging-backend staging-frontend
}

# ---------------------------------------------------------------------------
# Usage
# ---------------------------------------------------------------------------
usage() {
    printf "Usage: %s [FLAG]\n\n" "$(basename "$0")"
    printf "Flags:\n"
    printf "  (none)          Start stack using existing images, wait for healthy, print URLs\n"
    printf "  --rebuild       Force image rebuild before starting\n"
    printf "  --restore       Pull latest VPS backup then restore into staging containers\n"
    printf "  --restore-only  Restore from existing local backup (skip VPS pull)\n"
    printf "  --down          Stop containers (volumes preserved)\n"
    printf "  --nuke          Stop and DELETE all staging volumes (requires confirmation)\n"
    printf "  --logs          Tail backend + frontend logs\n"
}

# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------
FLAG="${1:-}"

case "${FLAG}" in
    "")             cmd_up 0 ;;
    --rebuild)      cmd_up 1 ;;
    --restore)      cmd_restore 0 && cmd_up 0 ;;
    --restore-only) cmd_restore 1 && cmd_up 0 ;;
    --down)         cmd_down ;;
    --nuke)         cmd_nuke ;;
    --logs)         cmd_logs ;;
    --help|-h)      usage ;;
    *)
        echo -e "  ${RED}FAIL${RESET} Unknown flag: '${FLAG}'" >&2
        echo ""
        usage
        exit 1
        ;;
esac
