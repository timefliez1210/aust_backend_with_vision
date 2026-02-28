#!/usr/bin/env bash
# Integration tests for the AUST backend API.
#
# Usage:
#   ./tests/integration/test_api.sh
#   STAGING_URL=http://staging.example.com ./tests/integration/test_api.sh
#
# Requirements:
#   - curl (required)
#   - python3 (required, for JWT generation)
#   - jq (optional; field-level response validation is skipped if absent)

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

STAGING_URL="${STAGING_URL:-http://localhost:8099}"
JWT_SECRET="staging-jwt-secret-do-not-use-in-production-min32chars"
TIMESTAMP=$(date +%s)
TEST_EMAIL="integration-test-${TIMESTAMP}@staging.test"

# ---------------------------------------------------------------------------
# Colors and counters
# ---------------------------------------------------------------------------

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0

# ---------------------------------------------------------------------------
# Result helpers
# ---------------------------------------------------------------------------

pass() {
    local name="$1"
    echo -e "  ${GREEN}PASS${NC}  $name"
    PASS_COUNT=$((PASS_COUNT + 1))
}

fail() {
    local msg="$1"
    echo -e "  ${RED}FAIL${NC}  $msg"
    FAIL_COUNT=$((FAIL_COUNT + 1))
}

skip() {
    local name="$1"
    local reason="${2:-}"
    if [ -n "$reason" ]; then
        echo -e "  ${YELLOW}SKIP${NC}  $name ($reason)"
    else
        echo -e "  ${YELLOW}SKIP${NC}  $name"
    fi
    SKIP_COUNT=$((SKIP_COUNT + 1))
}

# check <name> <expected_status> <actual_status> <body> [jq_expression]
#
# Validates that actual_status == expected_status.  If a jq_expression is
# provided and jq is installed, the expression is evaluated against <body>
# and the test fails when the result is null or empty.
check() {
    local name="$1"
    local expected_status="$2"
    local actual_status="$3"
    local body="$4"
    # optional field check: $5 = jq expression like ".total_distance_km"
    if [ "$actual_status" -eq "$expected_status" ]; then
        if [ -n "${5:-}" ] && command -v jq &>/dev/null; then
            local field_val
            field_val=$(echo "$body" | jq -r "${5}" 2>/dev/null)
            if [ "$field_val" = "null" ] || [ -z "$field_val" ]; then
                fail "$name: HTTP $actual_status OK but field ${5} missing in response"
                return
            fi
        fi
        pass "$name"
    else
        fail "$name: expected HTTP $expected_status, got $actual_status"
        if [ -n "$body" ]; then
            echo "    Response: ${body:0:200}"
        fi
    fi
}

# ---------------------------------------------------------------------------
# JWT generation (Python3 stdlib only — no PyJWT dependency needed)
#
# Generates an HS256 JWT with:
#   sub  = a deterministic test UUID
#   email = "integration-test@staging.test"
#   role  = "admin"
#   iat   = now
#   exp   = now + 3600 (1 hour)
#
# The token is signed with HMAC-SHA256 against JWT_SECRET.
# ---------------------------------------------------------------------------

generate_jwt() {
    python3 - <<PYEOF
import base64, hashlib, hmac, json, time, sys

secret = b"${JWT_SECRET}"
now = int(time.time())

header = {"alg": "HS256", "typ": "JWT"}
payload = {
    "sub": "018f0000-0000-7000-8000-000000000001",
    "email": "integration-test@staging.test",
    "role": "admin",
    "iat": now,
    "exp": now + 3600,
}

def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()

header_enc  = b64url(json.dumps(header,  separators=(",", ":")).encode())
payload_enc = b64url(json.dumps(payload, separators=(",", ":")).encode())
signing_input = f"{header_enc}.{payload_enc}".encode()

sig = hmac.new(secret, signing_input, hashlib.sha256).digest()
sig_enc = b64url(sig)

print(f"{header_enc}.{payload_enc}.{sig_enc}", end="")
PYEOF
}

# ---------------------------------------------------------------------------
# curl helper — returns "<status_code>|<body>"
# ---------------------------------------------------------------------------

http_get() {
    local url="$1"
    shift
    # remaining args are passed through (e.g. -H headers)
    local response
    response=$(curl -s -w "\n%{http_code}" "$url" "$@" 2>/dev/null)
    local body
    body=$(echo "$response" | head -n -1)
    local status
    status=$(echo "$response" | tail -n 1)
    echo "${status}|${body}"
}

http_post() {
    local url="$1"
    local data="$2"
    shift 2
    # remaining args are passed through (e.g. -H headers)
    local response
    response=$(curl -s -w "\n%{http_code}" -X POST \
        -H "Content-Type: application/json" \
        -d "$data" \
        "$url" "$@" 2>/dev/null)
    local body
    body=$(echo "$response" | head -n -1)
    local status
    status=$(echo "$response" | tail -n 1)
    echo "${status}|${body}"
}

http_delete() {
    local url="$1"
    shift
    local response
    response=$(curl -s -w "\n%{http_code}" -X DELETE "$url" "$@" 2>/dev/null)
    local body
    body=$(echo "$response" | head -n -1)
    local status
    status=$(echo "$response" | tail -n 1)
    echo "${status}|${body}"
}

# Split a "<status>|<body>" string into two variables.
# Usage: split_response "$raw" status body
split_response() {
    local raw="$1"
    # Use local -n for namerefs (bash 4.3+), but for broader compatibility:
    eval "$2=$(echo "$raw" | cut -d'|' -f1)"
    eval "$3=$(echo "$raw" | cut -d'|' -f2-)"
}

# ---------------------------------------------------------------------------
# Startup
# ---------------------------------------------------------------------------

echo ""
echo "AUST Backend — Integration Tests"
echo "Target: ${STAGING_URL}"
echo "Timestamp: ${TIMESTAMP}"
echo ""

# Generate JWT once
if ! command -v python3 &>/dev/null; then
    echo "ERROR: python3 is required for JWT generation but was not found."
    exit 2
fi

echo "Generating admin JWT..."
ADMIN_JWT=$(generate_jwt)
if [ -z "$ADMIN_JWT" ]; then
    echo "ERROR: JWT generation failed."
    exit 2
fi
echo "JWT generated (${#ADMIN_JWT} chars)"
echo ""

if ! command -v jq &>/dev/null; then
    echo "NOTE: jq not found — field-level validation will be skipped."
    echo ""
fi

# State shared across test groups
TEST_CUSTOMER_ID=""
TEST_QUOTE_ID=""

# ---------------------------------------------------------------------------
# Group 1: Health
# ---------------------------------------------------------------------------

echo "=== Group 1: Health ==="

raw=$(http_get "${STAGING_URL}/health")
split_response "$raw" hs hb
# Accept "ok" as literal body or as a JSON field
if [ "$hs" -eq 200 ]; then
    if echo "$hb" | grep -qi "ok"; then
        pass "GET /health → 200 with 'ok'"
    else
        fail "GET /health: HTTP 200 OK but body does not contain 'ok'"
        echo "    Response: ${hb:0:200}"
    fi
else
    fail "GET /health: expected HTTP 200, got $hs"
    [ -n "$hb" ] && echo "    Response: ${hb:0:200}"
fi

raw=$(http_get "${STAGING_URL}/ready")
split_response "$raw" hs hb
check "GET /ready → 200" 200 "$hs" "$hb"

echo ""

# ---------------------------------------------------------------------------
# Group 2: Public Endpoints (no auth)
# ---------------------------------------------------------------------------

echo "=== Group 2: Public Endpoints ==="

# Distance
raw=$(http_post "${STAGING_URL}/api/v1/distance/calculate" \
    '{"addresses":["Borsigstr 6, 31135 Hildesheim","Marktplatz 1, 30159 Hannover"]}')
split_response "$raw" hs hb
check "POST /api/v1/distance/calculate → 200 with total_distance_km" \
    200 "$hs" "$hb" ".total_distance_km"

# Calendar availability
raw=$(http_get "${STAGING_URL}/api/v1/calendar/availability?date=2026-06-15")
split_response "$raw" hs hb
check "GET /api/v1/calendar/availability → 200 with available field" \
    200 "$hs" "$hb" ".available"

# Calendar schedule
raw=$(http_get "${STAGING_URL}/api/v1/calendar/schedule?from=2026-06-01&to=2026-06-07")
split_response "$raw" hs hb
if [ "$hs" -eq 200 ]; then
    # Response should be an array
    if command -v jq &>/dev/null; then
        if echo "$hb" | jq -e 'type == "array"' &>/dev/null; then
            pass "GET /api/v1/calendar/schedule → 200 array"
        else
            fail "GET /api/v1/calendar/schedule: HTTP 200 but response is not an array"
            echo "    Response: ${hb:0:200}"
        fi
    else
        pass "GET /api/v1/calendar/schedule → 200"
    fi
else
    fail "GET /api/v1/calendar/schedule: expected HTTP 200, got $hs"
    [ -n "$hb" ] && echo "    Response: ${hb:0:200}"
fi

echo ""

# ---------------------------------------------------------------------------
# Group 3: Auth
# ---------------------------------------------------------------------------

echo "=== Group 3: Auth ==="

# Wrong credentials → 401
raw=$(http_post "${STAGING_URL}/api/v1/auth/login" \
    '{"email":"wrong@test.com","password":"wrong"}')
split_response "$raw" hs hb
check "POST /api/v1/auth/login (bad credentials) → 401" 401 "$hs" "$hb"

# Protected endpoint without auth → 401
raw=$(http_get "${STAGING_URL}/api/v1/admin/dashboard")
split_response "$raw" hs hb
check "GET /api/v1/admin/dashboard (no auth) → 401" 401 "$hs" "$hb"

echo ""

# ---------------------------------------------------------------------------
# Group 4: Admin API (uses generated JWT)
# ---------------------------------------------------------------------------

echo "=== Group 4: Admin API ==="

AUTH_HEADER="-H \"Authorization: Bearer ${ADMIN_JWT}\""

# Helper that passes the auth header without eval gymnastics
admin_get() {
    local url="$1"
    local response
    response=$(curl -s -w "\n%{http_code}" \
        -H "Authorization: Bearer ${ADMIN_JWT}" \
        "$url" 2>/dev/null)
    local body
    body=$(echo "$response" | head -n -1)
    local status
    status=$(echo "$response" | tail -n 1)
    echo "${status}|${body}"
}

admin_post() {
    local url="$1"
    local data="$2"
    local response
    response=$(curl -s -w "\n%{http_code}" -X POST \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer ${ADMIN_JWT}" \
        -d "$data" \
        "$url" 2>/dev/null)
    local body
    body=$(echo "$response" | head -n -1)
    local status
    status=$(echo "$response" | tail -n 1)
    echo "${status}|${body}"
}

admin_post_no_body() {
    local url="$1"
    local response
    response=$(curl -s -w "\n%{http_code}" -X POST \
        -H "Authorization: Bearer ${ADMIN_JWT}" \
        "$url" 2>/dev/null)
    local body
    body=$(echo "$response" | head -n -1)
    local status
    status=$(echo "$response" | tail -n 1)
    echo "${status}|${body}"
}

# Dashboard
raw=$(admin_get "${STAGING_URL}/api/v1/admin/dashboard")
split_response "$raw" hs hb
if [ "$hs" -eq 200 ]; then
    # Check all four required fields
    dashboard_ok=true
    for field in ".open_quotes" ".pending_offers" ".todays_bookings" ".total_customers"; do
        if command -v jq &>/dev/null; then
            val=$(echo "$hb" | jq -r "$field" 2>/dev/null)
            if [ "$val" = "null" ] || [ -z "$val" ]; then
                fail "GET /api/v1/admin/dashboard: field $field missing"
                dashboard_ok=false
            fi
        fi
    done
    if [ "$dashboard_ok" = true ]; then
        pass "GET /api/v1/admin/dashboard → 200 with required fields"
    fi
else
    fail "GET /api/v1/admin/dashboard: expected HTTP 200, got $hs"
    [ -n "$hb" ] && echo "    Response: ${hb:0:200}"
fi

# Quotes list
raw=$(admin_get "${STAGING_URL}/api/v1/admin/quotes")
split_response "$raw" hs hb
if [ "$hs" -eq 200 ]; then
    quotes_ok=true
    if command -v jq &>/dev/null; then
        quotes_val=$(echo "$hb" | jq -r ".quotes" 2>/dev/null)
        total_val=$(echo "$hb" | jq -r ".total" 2>/dev/null)
        if [ "$quotes_val" = "null" ] || [ -z "$quotes_val" ]; then
            fail "GET /api/v1/admin/quotes: .quotes field missing"
            quotes_ok=false
        fi
        if [ "$total_val" = "null" ] || [ -z "$total_val" ]; then
            fail "GET /api/v1/admin/quotes: .total field missing"
            quotes_ok=false
        fi
    fi
    if [ "$quotes_ok" = true ]; then
        pass "GET /api/v1/admin/quotes → 200 with quotes array and total"
    fi
else
    fail "GET /api/v1/admin/quotes: expected HTTP 200, got $hs"
    [ -n "$hb" ] && echo "    Response: ${hb:0:200}"
fi

# Customers list
raw=$(admin_get "${STAGING_URL}/api/v1/admin/customers")
split_response "$raw" hs hb
check "GET /api/v1/admin/customers → 200 with customers array" \
    200 "$hs" "$hb" ".customers"

# Offers list
raw=$(admin_get "${STAGING_URL}/api/v1/admin/offers")
split_response "$raw" hs hb
check "GET /api/v1/admin/offers → 200 with offers array" \
    200 "$hs" "$hb" ".offers"

# Create customer
CUSTOMER_PAYLOAD="{\"email\":\"${TEST_EMAIL}\",\"name\":\"Integration Test\"}"
raw=$(admin_post "${STAGING_URL}/api/v1/admin/customers" "$CUSTOMER_PAYLOAD")
split_response "$raw" hs hb
if [ "$hs" -eq 201 ] || [ "$hs" -eq 200 ]; then
    if command -v jq &>/dev/null; then
        cust_id=$(echo "$hb" | jq -r ".id" 2>/dev/null)
        if [ "$cust_id" = "null" ] || [ -z "$cust_id" ]; then
            fail "POST /api/v1/admin/customers: HTTP $hs but .id missing"
        else
            TEST_CUSTOMER_ID="$cust_id"
            pass "POST /api/v1/admin/customers → ${hs} with id=${cust_id:0:8}..."
        fi
    else
        pass "POST /api/v1/admin/customers → ${hs}"
    fi
else
    fail "POST /api/v1/admin/customers: expected HTTP 201/200, got $hs"
    [ -n "$hb" ] && echo "    Response: ${hb:0:200}"
fi

# Create quote (requires customer_id)
if [ -z "$TEST_CUSTOMER_ID" ]; then
    skip "POST /api/v1/admin/quotes" "no customer id available (customer creation failed)"
    skip "GET /api/v1/admin/quotes/{id}" "no quote id available (customer creation failed)"
else
    QUOTE_PAYLOAD=$(cat <<JSONEOF
{
  "customer_id": "${TEST_CUSTOMER_ID}",
  "origin": {
    "street": "Borsigstr 6",
    "city": "Hildesheim",
    "postal_code": "31135",
    "floor": "EG",
    "elevator": false
  },
  "destination": {
    "street": "Marktplatz 1",
    "city": "Hannover",
    "postal_code": "30159",
    "floor": "1. OG",
    "elevator": true
  },
  "estimated_volume_m3": 15.0,
  "distance_km": 30.5,
  "notes": "Integration test quote"
}
JSONEOF
)

    raw=$(admin_post "${STAGING_URL}/api/v1/admin/quotes" "$QUOTE_PAYLOAD")
    split_response "$raw" hs hb
    if [ "$hs" -eq 201 ] || [ "$hs" -eq 200 ]; then
        if command -v jq &>/dev/null; then
            quote_id=$(echo "$hb" | jq -r ".id" 2>/dev/null)
            if [ "$quote_id" = "null" ] || [ -z "$quote_id" ]; then
                fail "POST /api/v1/admin/quotes: HTTP $hs but .id missing"
            else
                TEST_QUOTE_ID="$quote_id"
                pass "POST /api/v1/admin/quotes → ${hs} with id=${quote_id:0:8}..."
            fi
        else
            pass "POST /api/v1/admin/quotes → ${hs}"
        fi
    else
        fail "POST /api/v1/admin/quotes: expected HTTP 201/200, got $hs"
        [ -n "$hb" ] && echo "    Response: ${hb:0:200}"
    fi

    # Get quote detail
    if [ -z "$TEST_QUOTE_ID" ]; then
        skip "GET /api/v1/admin/quotes/{id}" "no quote id available (quote creation failed)"
    else
        raw=$(admin_get "${STAGING_URL}/api/v1/admin/quotes/${TEST_QUOTE_ID}")
        split_response "$raw" hs hb
        if [ "$hs" -eq 200 ]; then
            quote_ok=true
            if command -v jq &>/dev/null; then
                for field in ".status" ".customer_name" ".customer_email"; do
                    val=$(echo "$hb" | jq -r "$field" 2>/dev/null)
                    if [ "$val" = "null" ] || [ -z "$val" ]; then
                        fail "GET /api/v1/admin/quotes/{id}: field $field missing"
                        quote_ok=false
                    fi
                done
                # origin and destination may be objects (not strings) — check they exist and are not null
                origin_val=$(echo "$hb" | jq -r ".origin" 2>/dev/null)
                dest_val=$(echo "$hb"   | jq -r ".destination" 2>/dev/null)
                if [ "$origin_val" = "null" ]; then
                    fail "GET /api/v1/admin/quotes/{id}: .origin is null"
                    quote_ok=false
                fi
                if [ "$dest_val" = "null" ]; then
                    fail "GET /api/v1/admin/quotes/{id}: .destination is null"
                    quote_ok=false
                fi
                # latest_offer may be null — that is acceptable
                latest_offer=$(echo "$hb" | jq -r ".offer" 2>/dev/null)
                if [ "$latest_offer" = "null" ]; then
                    echo "    Note: .offer is null (no offer generated yet — OK)"
                fi
            fi
            if [ "$quote_ok" = true ]; then
                pass "GET /api/v1/admin/quotes/{id} → 200 with status, customer_name, origin, destination"
            fi
        else
            fail "GET /api/v1/admin/quotes/{id}: expected HTTP 200, got $hs"
            [ -n "$hb" ] && echo "    Response: ${hb:0:200}"
        fi
    fi
fi

echo ""

# ---------------------------------------------------------------------------
# Group 5: Cleanup
# ---------------------------------------------------------------------------

echo "=== Group 5: Cleanup ==="

if [ -z "$TEST_CUSTOMER_ID" ]; then
    skip "DELETE test customer" "no customer was created"
else
    # The admin router exposes POST /customers/{id}/delete (not DELETE method).
    raw=$(admin_post_no_body "${STAGING_URL}/api/v1/admin/customers/${TEST_CUSTOMER_ID}/delete")
    split_response "$raw" hs hb
    if [ "$hs" -eq 200 ] || [ "$hs" -eq 204 ]; then
        pass "POST /api/v1/admin/customers/{id}/delete → ${hs}"
    elif [ "$hs" -ge 400 ] && [ "$hs" -lt 500 ]; then
        # Foreign key constraint (customer has quotes) — acceptable
        skip "POST /api/v1/admin/customers/{id}/delete" \
            "HTTP ${hs} — customer may have dependent quotes (foreign key constraint)"
    else
        fail "POST /api/v1/admin/customers/{id}/delete: unexpected HTTP $hs"
        [ -n "$hb" ] && echo "    Response: ${hb:0:200}"
    fi
fi

echo ""

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo "================================="
echo "Integration Tests: ${PASS_COUNT} passed, ${FAIL_COUNT} failed"
if [ "$SKIP_COUNT" -gt 0 ]; then
    echo "                  ${SKIP_COUNT} skipped"
fi
echo "================================="
echo ""

if [ "$FAIL_COUNT" -gt 0 ]; then
    exit 1
fi

exit 0
