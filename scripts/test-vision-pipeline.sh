#!/usr/bin/env bash
# test-vision-pipeline.sh — End-to-end foto analysis test against REAL Modal.
#
# Usage:
#   AUST_DEV_VISION=1 bash scripts/dev-up.sh              # start backend WITH vision
#   bash scripts/test-vision-pipeline.sh [photo|video|ar] # run the test
#
# What it does:
#   1. Generates a synthetic room photo (or accepts a real image path)
#   2. POSTs it to the local backend submission endpoint
#   3. Polls the local DB until the estimation completes/fails/times out
#   4. Reports whether Modal was reached, the result, and any errors
#
# Requires:
#   - Local backend running on :8080   (dev-up.sh)
#   - Local postgres on :5435          (dev-up.sh)
#   - Python3 + Pillow (for synthetic image generation)
#   - curl + jq

set -euo pipefail

API_BASE="${API_BASE:-http://localhost:8080}"
DB_URL="${AUST__DATABASE__URL:-postgres://aust_staging:aust_staging_password@localhost:5435/aust_staging}"
TEST_MODE="${1:-photo}"
TEST_IMAGE="${TEST_IMAGE:-}"
TEST_VIDEO="${TEST_VIDEO:-}"
POLL_INTERVAL="${POLL_INTERVAL:-5}"
MAX_POLLS="${MAX_POLLS:-120}"   # 120 × 5s = 10 min ceiling

GREEN="\033[0;32m"; RED="\033[0;31m"; YELLOW="\033[0;33m"; BOLD="\033[1m"; RESET="\033[0m"
ok()   { echo -e "  ${GREEN}OK${RESET}  ${1}"; }
warn() { echo -e "  ${YELLOW}WARN${RESET} ${1}"; }
fail() { echo -e "  ${RED}FAIL${RESET} ${1}" >&2; exit 1; }
step() { echo -e "\n${BOLD}>>> ${1}${RESET}"; }

# ---------------------------------------------------------------------------
# 0. Validate environment
# ---------------------------------------------------------------------------
step "Validating test environment"

if ! pg_isready -d "$DB_URL" >/dev/null 2>&1; then
    fail "Postgres not reachable at $DB_URL — is dev-up.sh running?"
fi
ok "Postgres reachable"

if ! curl -sf "$API_BASE/health" >/dev/null 2>&1; then
    fail "Backend not reachable at $API_BASE — is dev-up.sh running?"
fi
ok "Backend reachable"

# Check vision service is actually enabled in the running backend
VISION_ENABLED=$(psql "$DB_URL" -Atqc "SELECT 1 LIMIT 1" 2>/dev/null && echo "db_ok" || echo "db_fail")
if [[ "$VISION_ENABLED" != "db_ok" ]]; then
    fail "Cannot query DB"
fi
ok "DB queryable"

# Check the backend .env / config to see if vision is enabled
# (We can't query the backend config directly, but we can check env)
step "Checking vision service configuration"
if [[ -f ".env" ]]; then
    VISION_CFG=$(grep "AUST__VISION_SERVICE__ENABLED" .env 2>/dev/null || echo "")
    if [[ "$VISION_CFG" == *"true"* ]]; then
        ok "Vision service enabled in .env"
    else
        warn "Vision service appears DISABLED in .env"
        warn "You probably need to restart with: AUST_DEV_VISION=1 bash scripts/dev-up.sh"
    fi
else
    warn "No .env found at project root"
fi

# ---------------------------------------------------------------------------
# 1. Generate or locate test media
# ---------------------------------------------------------------------------
step "Preparing test media (mode: $TEST_MODE)"

if [[ "$TEST_MODE" == "photo" || "$TEST_MODE" == "ar" ]]; then
    if [[ -n "$TEST_IMAGE" && -f "$TEST_IMAGE" ]]; then
        ok "Using provided image: $TEST_IMAGE"
    else
        TEST_IMAGE="/tmp/aust_test_room.jpg"
        python3 - <<'PY' || fail "Failed to generate synthetic test image (needs: python3 -m pip install Pillow)"
from PIL import Image, ImageDraw, ImageFont
from pathlib import Path
img = Image.new("RGB", (1280, 960), color="#e8dcc5")
draw = ImageDraw.Draw(img)
# Floor
draw.rectangle([0, 700, 1280, 960], fill="#8B7355", outline="black", width=3)
# Sofa
draw.rectangle([120, 450, 520, 700], fill="#8B4513", outline="black", width=3)
draw.rectangle([120, 500, 520, 550], fill="#5C3317", outline="black", width=2)
# Table
draw.rectangle([600, 580, 900, 800], fill="#DEB887", outline="black", width=3)
draw.rectangle([640, 650, 690, 800], fill="#8B4513", outline="black", width=2)
draw.rectangle([810, 650, 860, 800], fill="#8B4513", outline="black", width=2)
# Wardrobe
draw.rectangle([800, 200, 1100, 700], fill="#556B2F", outline="black", width=3)
# Plant
draw.ellipse([950, 750, 1100, 920], fill="#228B22", outline="black", width=2)
draw.line([(1025, 920), (1025, 960)], fill="#8B4513", width=6)
# Boxes
draw.rectangle([150, 720, 280, 870], fill="#D2691E", outline="black", width=2)
# Save
p = Path("/tmp/aust_test_room.jpg")
img.save(p, quality=70)
print(f"Generated {p} ({p.stat().st_size / 1024:.0f} kB)")
PY
        ok "Generated synthetic room photo: $TEST_IMAGE"
    fi
elif [[ "$TEST_MODE" == "video" ]]; then
    if [[ -n "$TEST_VIDEO" && -f "$TEST_VIDEO" ]]; then
        ok "Using provided video: $TEST_VIDEO"
    else
        fail "VIDEO TEST_MODE requires TEST_VIDEO=/path/to/video.mp4 environment variable"
    fi
else
    fail "Unknown test mode: $TEST_MODE. Use: photo, video, ar"
fi

# ---------------------------------------------------------------------------
# 2. Submit inquiry
# ---------------------------------------------------------------------------
step "Submitting test inquiry to $API_BASE/api/v1/submit/$TEST_MODE"

if [[ "$TEST_MODE" == "photo" ]]; then
    SUBMIT_RESP=$(curl -s -w "\n%{http_code}" -X POST "$API_BASE/api/v1/submit/photo" \
        -F "name=Vision Test" \
        -F "email=vision-test$(date +%s)@aust.test" \
        -F "departure_address=Musterstr. 1, 31157 Sarstedt" \
        -F "arrival_address=Berlinstr. 5, 10115 Berlin" \
        -F "images=@$TEST_IMAGE")
elif [[ "$TEST_MODE" == "ar" ]]; then
    SUBMIT_RESP=$(curl -s -w "\n%{http_code}" -X POST "$API_BASE/api/v1/submit/mobile/ar" \
        -F "name=AR Vision Test" \
        -F "email=vision-test$(date +%s)@aust.test" \
        -F "departure_address=Musterstr. 1, 31157 Sarstedt" \
        -F "arrival_address=Berlinstr. 5, 10115 Berlin" \
        -F "images=@$TEST_IMAGE" \
        -F "images=@$TEST_IMAGE" \
        -F "item_manifest=[{\"label\":\"Sofa\",\"frame_count\":2}]")
elif [[ "$TEST_MODE" == "video" ]]; then
    SUBMIT_RESP=$(curl -s -w "\n%{http_code}" -X POST "$API_BASE/api/v1/submit/video" \
        -F "name=Video Vision Test" \
        -F "email=vision-test$(date +%s)@aust.test" \
        -F "departure_address=Musterstr. 1, 31157 Sarstedt" \
        -F "arrival_address=Berlinstr. 5, 10115 Berlin" \
        -F "video=@$TEST_VIDEO")
fi

HTTP_CODE=$(echo "$SUBMIT_RESP" | tail -n1)
BODY=$(echo "$SUBMIT_RESP" | sed '$d')

echo "HTTP $HTTP_CODE"
echo "Body: $(echo "$BODY" | python3 -m json.tool 2>/dev/null || echo "$BODY")"

if [[ "$HTTP_CODE" != "200" && "$HTTP_CODE" != "201" && "$HTTP_CODE" != "202" ]]; then
    fail "Submission failed with HTTP $HTTP_CODE"
fi

INQUIRY_ID=$(echo "$BODY" | python3 -c "import sys,json; print(json.load(sys.stdin)['inquiry_id'])" 2>/dev/null || echo "")
CUSTOMER_ID=$(echo "$BODY" | python3 -c "import sys,json; print(json.load(sys.stdin)['customer_id'])" 2>/dev/null || echo "")
STATUS=$(echo "$BODY" | python3 -c "import sys,json; print(json.load(sys.stdin).get('status',''))" 2>/dev/null || echo "")

if [[ -z "$INQUIRY_ID" ]]; then
    fail "Could not extract inquiry_id from response"
fi

ok "Inquiry created: $INQUIRY_ID (status=$STATUS)"
echo "  Customer: $CUSTOMER_ID"
echo "  Inquiry : $INQUIRY_ID"

# ---------------------------------------------------------------------------
# 3. Poll DB for estimation result
# ---------------------------------------------------------------------------
step "Polling DB for estimation result (max ${MAX_POLLS} polls × ${POLL_INTERVAL}s)"

ESTIMATION_ID=""
LAST_EST_STATUS=""
LAST_LOG_LINE=""
POLL_START=$(date +%s)

for i in $(seq 1 $MAX_POLLS); do
    NOW=$(date +%s)
    ELAPSED=$((NOW - POLL_START))

    # Find the estimation row for this inquiry
    EST_ROW=$(psql "$DB_URL" -Atq -c "
        SELECT id::text, status, method, total_volume_m3, confidence_score,
               (result_data IS NOT NULL) as has_result,
               (source_data->>'error')::text as error_msg
        FROM volume_estimations
        WHERE inquiry_id = '$INQUIRY_ID'
        ORDER BY created_at DESC
        LIMIT 1
    " 2>/dev/null || echo "||||||")

    IFS='|' read -r EST_ID EST_STATUS EST_METHOD EST_VOLUME EST_CONF HAS_RESULT ERROR_MSG <<< "$EST_ROW"

    # Also check inquiry status
    INQ_STATUS=$(psql "$DB_URL" -Atq -c "SELECT status FROM inquiries WHERE id = '$INQUIRY_ID'" 2>/dev/null || echo "")

    if [[ -n "$EST_ID" && "$EST_ID" != "$ESTIMATION_ID" ]]; then
        ESTIMATION_ID="$EST_ID"
        ok "Estimation row created: $ESTIMATION_ID (method=$EST_METHOD)"
    fi

    # Format volume
    VOL_DISP="${EST_VOLUME:-n/a}"
    CONF_DISP="${EST_CONF:-n/a}"

    # Show current state (on change or every 12 polls ≈ 60s)
    CURRENT_LINE="[$i/${MAX_POLLS}] [${ELAPSED}s] inquiry=$INQ_STATUS estimation=$EST_STATUS${EST_METHOD:+($EST_METHOD)} volume=${VOL_DISP} confidence=${CONF_DISP}"
    if [[ "$CURRENT_LINE" != "$LAST_LOG_LINE" || $((i % 12)) -eq 0 ]]; then
        echo "  $CURRENT_LINE"
        LAST_LOG_LINE="$CURRENT_LINE"
    fi

    # Terminal states
    if [[ "$EST_STATUS" == "completed" && "$HAS_RESULT" == "t" ]]; then
        echo ""
        ok "Vision analysis SUCCEEDED after ${ELAPSED}s"
        echo "  Estimation ID: $ESTIMATION_ID"
        echo "  Volume      : ${EST_VOLUME} m³"
        echo "  Confidence  : ${EST_CONF}"
        echo "  Method      : ${EST_METHOD}"
        step "Result items"
        psql "$DB_URL" -c "
            SELECT
                jsonb_pretty(result_data) as result
            FROM volume_estimations
            WHERE id = '$ESTIMATION_ID'
        " 2>/dev/null || echo "  (could not fetch result_data)"
        exit 0
    fi

    if [[ "$EST_STATUS" == "failed" ]]; then
        echo ""
        fail "Vision analysis FAILED after ${ELAPSED}s"
        echo "  Estimation ID: $ESTIMATION_ID"
        echo "  Error        : ${ERROR_MSG:-(see logs)}"
        step "Backend logs since submission"
        echo "  Check: journalctl -u aust-backend -n 100 --no-pager"
        echo "  Or:    docker logs aust_staging_backend"
        exit 1
    fi

    if [[ "$INQ_STATUS" == "estimated" && -n "$EST_VOLUME" ]]; then
        echo ""
        ok "Inquiry reached 'estimated' status (volume=${EST_VOLUME}m³)"
        echo "  Estimation ID: $ESTIMATION_ID"
        echo "  Volume       : ${EST_VOLUME} m³"
        echo "  Confidence   : ${EST_CONF}"
        exit 0
    fi

    sleep "$POLL_INTERVAL"
done

# ---------------------------------------------------------------------------
# 4. Timeout
# ---------------------------------------------------------------------------
echo ""
fail "Timed out after ${MAX_POLLS} polls ($((${MAX_POLLS} * POLL_INTERVAL))s)"
echo "  Inquiry ID    : $INQUIRY_ID"
echo "  Estimation ID : ${ESTIMATION_ID:-(never created)}"
echo "  Last est status: ${EST_STATUS:-n/a}"
echo ""
step "Debugging hints"
echo "  1. Check the backend is processing: tail -f /tmp/aust_backend.log"
echo "  2. Does the vision_service client exist?"
echo "     psql $DB_URL -c \"SELECT id, status, method FROM volume_estimations WHERE inquiry_id = '$INQUIRY_ID'\""
echo "  3. Is Modal reachable from your machine?"
echo "     curl https://crfabig--aust-vision-serve.modal.run/health"
echo "  4. Check if the vision_semaphore is stuck (only 1 concurrent job):"
echo "     grep 'Vision semaphore' /tmp/aust_backend.log"
