#!/bin/bash
# staging.sh — Manage the AUST backend staging environment lifecycle
# Usage: ./scripts/staging.sh <up|down|test|logs|status|clean|rebuild>
set -euo pipefail

# ---------------------------------------------------------------------------
# Trap: print which line failed
# ---------------------------------------------------------------------------
trap 'echo "${RED}[staging.sh] Error on line ${LINENO}${RESET}" >&2' ERR

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FRONTEND_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/frontend"
COMPOSE_FILE="${PROJECT_ROOT}/docker/docker-compose.staging.yml"
INTEGRATION_TESTS="${PROJECT_ROOT}/tests/integration/test_api.sh"

# ---------------------------------------------------------------------------
# Staging environment constants
# ---------------------------------------------------------------------------
STAGING_BACKEND_URL="http://localhost:8099"
STAGING_FRONTEND_URL="http://localhost:4173"
STAGING_POSTGRES_HOST="localhost"
STAGING_POSTGRES_PORT="5435"
STAGING_POSTGRES_USER="aust"
STAGING_POSTGRES_PASSWORD="aust_staging"
STAGING_POSTGRES_DB="aust_staging"
STAGING_TEST_DB="aust_backend_test"
STAGING_JWT_SECRET="staging-jwt-secret-do-not-use-in-production-min32chars"

HEALTH_POLL_INTERVAL=3
HEALTH_TIMEOUT=120

# ---------------------------------------------------------------------------
# Color output (only when stdout is a terminal)
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

info()    { printf "${YELLOW}[staging] %s${RESET}\n" "$*"; }
success() { printf "${GREEN}[staging] %s${RESET}\n" "$*"; }
error()   { printf "${RED}[staging] ERROR: %s${RESET}\n" "$*" >&2; }
header()  { printf "\n${BOLD}%s${RESET}\n" "$*"; }

# ---------------------------------------------------------------------------
# Helper: docker compose wrapper
# ---------------------------------------------------------------------------
dc() {
    docker compose -f "${COMPOSE_FILE}" "$@"
}

# ---------------------------------------------------------------------------
# Helper: generate a HS256 JWT for admin integration tests
# ---------------------------------------------------------------------------
generate_test_jwt() {
    python3 - <<'PYEOF'
import base64, hashlib, hmac, json, time
secret = "staging-jwt-secret-do-not-use-in-production-min32chars"
header = base64.urlsafe_b64encode(json.dumps({"alg":"HS256","typ":"JWT"}).encode()).rstrip(b"=").decode()
payload = base64.urlsafe_b64encode(json.dumps({
    "sub": "00000000-0000-0000-0000-000000000001",
    "email": "staging@test.com",
    "role": "admin",
    "exp": int(time.time()) + 86400,
    "iat": int(time.time())
}).encode()).rstrip(b"=").decode()
msg = f"{header}.{payload}".encode()
sig = base64.urlsafe_b64encode(
    hmac.new(secret.encode(), msg, hashlib.sha256).digest()
).rstrip(b"=").decode()
print(f"{header}.{payload}.{sig}")
PYEOF
}

# ---------------------------------------------------------------------------
# Helper: wait for the backend /health endpoint to respond
# ---------------------------------------------------------------------------
wait_for_backend() {
    info "Waiting for backend to become healthy (timeout: ${HEALTH_TIMEOUT}s)..."
    local elapsed=0
    while true; do
        if curl -sf "${STAGING_BACKEND_URL}/health" >/dev/null 2>&1; then
            success "Backend is healthy."
            return 0
        fi
        if [ "${elapsed}" -ge "${HEALTH_TIMEOUT}" ]; then
            error "Backend did not become healthy within ${HEALTH_TIMEOUT}s."
            error "Check logs: ./scripts/staging.sh logs"
            return 1
        fi
        sleep "${HEALTH_POLL_INTERVAL}"
        elapsed=$((elapsed + HEALTH_POLL_INTERVAL))
        info "  Still waiting... (${elapsed}s / ${HEALTH_TIMEOUT}s)"
    done
}

# ---------------------------------------------------------------------------
# Helper: assert staging is running (used by test command)
# ---------------------------------------------------------------------------
assert_staging_running() {
    if ! curl -sf "${STAGING_BACKEND_URL}/health" >/dev/null 2>&1; then
        error "Staging backend is not running at ${STAGING_BACKEND_URL}."
        error "Start it first: ./scripts/staging.sh up"
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# Helper: print a bordered URL box
# ---------------------------------------------------------------------------
print_url_box() {
    local border="+-----------------------------------------+"
    printf "\n${GREEN}%s${RESET}\n" "${border}"
    printf "${GREEN}|  %-39s |${RESET}\n" "Staging environment is ready"
    printf "${GREEN}|  %-39s |${RESET}\n" ""
    printf "${GREEN}|  %-39s |${RESET}\n" "Backend:   ${STAGING_BACKEND_URL}"
    printf "${GREEN}|  %-39s |${RESET}\n" "Frontend:  ${STAGING_FRONTEND_URL}"
    printf "${GREEN}|  %-39s |${RESET}\n" "Postgres:  ${STAGING_POSTGRES_HOST}:${STAGING_POSTGRES_PORT}/${STAGING_POSTGRES_DB}"
    printf "${GREEN}%s${RESET}\n\n" "${border}"
}

# ---------------------------------------------------------------------------
# Subcommand: up
# ---------------------------------------------------------------------------
cmd_up() {
    header "=== AUST Staging: UP ==="
    info "Building images and starting all containers..."

    dc up -d --build

    wait_for_backend
    print_url_box
}

# ---------------------------------------------------------------------------
# Subcommand: down
# ---------------------------------------------------------------------------
cmd_down() {
    header "=== AUST Staging: DOWN ==="
    info "Stopping all staging containers (volumes preserved)..."
    dc down
    success "Staging containers stopped."
}

# ---------------------------------------------------------------------------
# Subcommand: logs
# ---------------------------------------------------------------------------
cmd_logs() {
    header "=== AUST Staging: LOGS (Ctrl+C to stop) ==="
    dc logs -f
}

# ---------------------------------------------------------------------------
# Subcommand: status
# ---------------------------------------------------------------------------
cmd_status() {
    header "=== AUST Staging: STATUS ==="
    dc ps
}

# ---------------------------------------------------------------------------
# Subcommand: clean
# ---------------------------------------------------------------------------
cmd_clean() {
    header "=== AUST Staging: CLEAN ==="
    info "Stopping containers and DELETING all staging volumes..."
    dc down -v
    success "Staging environment fully reset (all data deleted)."
}

# ---------------------------------------------------------------------------
# Subcommand: rebuild
# ---------------------------------------------------------------------------
cmd_rebuild() {
    header "=== AUST Staging: REBUILD ==="
    cmd_down
    cmd_up
}

# ---------------------------------------------------------------------------
# Subcommand: test
# ---------------------------------------------------------------------------
cmd_test() {
    header "=== AUST Staging: TEST SUITE ==="

    # Track pass/fail for each group
    local backend_status="SKIP"
    local frontend_status="SKIP"
    local integration_status="SKIP"

    # 1. Assert staging is running
    info "Checking staging backend health..."
    assert_staging_running
    success "Staging backend is up."

    # 2. Create test database if it does not exist
    info "Ensuring test database '${STAGING_TEST_DB}' exists..."
    PGPASSWORD="${STAGING_POSTGRES_PASSWORD}" psql \
        -h "${STAGING_POSTGRES_HOST}" \
        -p "${STAGING_POSTGRES_PORT}" \
        -U "${STAGING_POSTGRES_USER}" \
        -c "CREATE DATABASE ${STAGING_TEST_DB};" 2>/dev/null || true
    success "Test database ready."

    # 3. Backend unit tests
    header "--- Backend unit tests ---"
    local backend_exit=0
    TEST_DATABASE_URL="postgres://${STAGING_POSTGRES_USER}:${STAGING_POSTGRES_PASSWORD}@${STAGING_POSTGRES_HOST}:${STAGING_POSTGRES_PORT}/${STAGING_TEST_DB}" \
        cargo test --manifest-path "${PROJECT_ROOT}/Cargo.toml" \
            -p aust-api -p aust-offer-generator --lib 2>&1 \
        || backend_exit=$?

    if [ "${backend_exit}" -eq 0 ]; then
        backend_status="PASS"
        success "Backend unit tests: PASS"
    else
        backend_status="FAIL"
        error "Backend unit tests: FAIL (exit code ${backend_exit})"
    fi

    # 4. Frontend unit tests
    header "--- Frontend unit tests ---"
    if [ ! -d "${FRONTEND_DIR}" ]; then
        error "Frontend directory not found: ${FRONTEND_DIR}"
        frontend_status="SKIP"
    else
        local frontend_exit=0
        (cd "${FRONTEND_DIR}" && npm run test) 2>&1 || frontend_exit=$?

        if [ "${frontend_exit}" -eq 0 ]; then
            frontend_status="PASS"
            success "Frontend unit tests: PASS"
        else
            frontend_status="FAIL"
            error "Frontend unit tests: FAIL (exit code ${frontend_exit})"
        fi
    fi

    # 5. Integration tests
    header "--- API integration tests ---"
    if [ ! -f "${INTEGRATION_TESTS}" ]; then
        error "Integration test script not found: ${INTEGRATION_TESTS}"
        integration_status="SKIP"
    else
        local integration_exit=0
        # Export JWT for use by the integration test script
        export TEST_JWT
        TEST_JWT="$(generate_test_jwt)"

        STAGING_URL="${STAGING_BACKEND_URL}" bash "${INTEGRATION_TESTS}" 2>&1 \
            || integration_exit=$?

        if [ "${integration_exit}" -eq 0 ]; then
            integration_status="PASS"
            success "API integration tests: PASS"
        else
            integration_status="FAIL"
            error "API integration tests: FAIL (exit code ${integration_exit})"
        fi
    fi

    # 6. Summary
    header "=== Test Summary ==="

    local overall=0

    _print_result() {
        local label="$1"
        local status="$2"
        case "${status}" in
            PASS) printf "  ${GREEN}%-30s PASS${RESET}\n" "${label}" ;;
            FAIL) printf "  ${RED}%-30s FAIL${RESET}\n" "${label}"; overall=1 ;;
            SKIP) printf "  ${YELLOW}%-30s SKIP${RESET}\n" "${label}" ;;
        esac
    }

    _print_result "Backend unit tests"     "${backend_status}"
    _print_result "Frontend unit tests"    "${frontend_status}"
    _print_result "API integration tests"  "${integration_status}"

    echo ""
    if [ "${overall}" -eq 0 ]; then
        success "All test groups passed."
    else
        error "One or more test groups failed."
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------
usage() {
    printf "Usage: %s <subcommand>\n\n" "$(basename "$0")"
    printf "Subcommands:\n"
    printf "  up       Build images, start containers, wait for healthy, print URLs\n"
    printf "  down     Stop all containers (volumes preserved)\n"
    printf "  test     Run full test suite: backend unit + frontend unit + API integration\n"
    printf "  logs     Follow all container logs (Ctrl+C to stop)\n"
    printf "  status   Show container status\n"
    printf "  clean    Stop and DELETE all staging volumes (full reset)\n"
    printf "  rebuild  down + up (full rebuild without cleaning data)\n"
}

if [ $# -lt 1 ]; then
    usage
    exit 1
fi

SUBCOMMAND="$1"
shift

case "${SUBCOMMAND}" in
    up)      cmd_up ;;
    down)    cmd_down ;;
    test)    cmd_test ;;
    logs)    cmd_logs ;;
    status)  cmd_status ;;
    clean)   cmd_clean ;;
    rebuild) cmd_rebuild ;;
    *)
        error "Unknown subcommand: '${SUBCOMMAND}'"
        echo ""
        usage
        exit 1
        ;;
esac
