#!/usr/bin/env bash
# End-to-end volume estimation test.
# Creates a FRESH inquiry via the public submit endpoint, polls until done,
# prints itemised breakdown and compares to baseline.
#
# Usage:
#   ./scripts/e2e_test.sh                          # uses defaults
#   ./scripts/e2e_test.sh /path/to/images 70.0     # custom image dir + baseline
#
# Images are downloaded from MinIO automatically if IMAGE_DIR is not provided.

set -euo pipefail

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------
API_BASE="${API_BASE:-https://api.aufraeumhelden.com}"
BASELINE_M3="${2:-70.0}"
TOLERANCE_PCT=25
POLL_INTERVAL=20   # seconds
POLL_TIMEOUT=1800  # 30 min

# Source images from Heike Lübben's original submission (52 photos in MinIO)
MINIO_ENDPOINT="${MINIO_ENDPOINT:-http://localhost:9000}"
MINIO_BUCKET="aust-uploads"
EST_PREFIX="estimates/019cf613-658b-7740-907b-f362c4b90074/019cf613-658c-7020-bedf-0623fce78a40"
IMAGE_COUNT=52

IMAGE_DIR="${1:-/tmp/e2e_test_images}"
TEST_EMAIL="e2e-test-$(date +%s)@test.invalid"

log() { echo "[$(date +%H:%M:%S)] $*"; }
die() { echo "ERROR: $*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# 1. Download images if not already cached
# ---------------------------------------------------------------------------
if [[ ! -d "$IMAGE_DIR" ]] || [[ $(ls "$IMAGE_DIR"/*.jpg 2>/dev/null | wc -l) -lt $IMAGE_COUNT ]]; then
    log "Downloading $IMAGE_COUNT images from MinIO → $IMAGE_DIR"
    mkdir -p "$IMAGE_DIR"
    for i in $(seq 0 $((IMAGE_COUNT - 1))); do
        curl -sf "$MINIO_ENDPOINT/$MINIO_BUCKET/$EST_PREFIX/$i.jpg" \
            -o "$IMAGE_DIR/$i.jpg" \
            --aws-sigv4 "aws:amz:us-east-1:s3" \
            --user "minioadmin:minioadmin" \
            || die "Failed to download image $i"
        [[ $((i % 10)) -eq 9 ]] && log "  ... $((i+1))/$IMAGE_COUNT"
    done
    log "  ✓ All images cached in $IMAGE_DIR"
else
    log "Using cached images in $IMAGE_DIR ($(ls "$IMAGE_DIR"/*.jpg | wc -l) files)"
fi

# ---------------------------------------------------------------------------
# 2. Submit to public endpoint — creates a fresh inquiry
# ---------------------------------------------------------------------------
log "Submitting $IMAGE_COUNT images to POST $API_BASE/api/v1/submit/photo"

FORM_ARGS=(-F "email=$TEST_EMAIL" -F "departure_address=Teststraße 1, 30159 Hannover" -F "arrival_address=Zielstraße 2, 30159 Hannover")
for f in "$IMAGE_DIR"/*.jpg; do
    FORM_ARGS+=(-F "images=@$f")
done

RESPONSE=$(curl -sf -X POST "$API_BASE/api/v1/submit/photo" "${FORM_ARGS[@]}" \
    -H "Accept: application/json") \
    || die "Submit failed"

INQUIRY_ID=$(echo "$RESPONSE" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('inquiry_id') or d.get('id') or '')" 2>/dev/null)
[[ -z "$INQUIRY_ID" ]] && { echo "Submit response: $RESPONSE"; die "Could not parse inquiry_id from response"; }

log "  ✓ New inquiry created: $INQUIRY_ID"
log "  Test email: $TEST_EMAIL"

# ---------------------------------------------------------------------------
# 3. Poll GET /api/v1/inquiries/{id} until estimation completes
#    (uses admin token for polling — public submit, admin read)
# ---------------------------------------------------------------------------
log "Polling until estimation completes…"

# Get token (only needed for polling the admin inquiry endpoint)
TOKEN=$(curl -sf -X POST "$API_BASE/api/v1/auth/login" \
    -H "Content-Type: application/json" \
    -d '{"email":"info@aust-umzuege.de","password":"test1234"}' \
    | python3 -c "import json,sys; print(json.load(sys.stdin).get('access_token',''))" 2>/dev/null) || true

DEADLINE=$((SECONDS + POLL_TIMEOUT))
while [[ $SECONDS -lt $DEADLINE ]]; do
    sleep $POLL_INTERVAL

    if [[ -n "$TOKEN" ]]; then
        DATA=$(curl -sf "$API_BASE/api/v1/inquiries/$INQUIRY_ID" \
            -H "Authorization: Bearer $TOKEN") || continue
    else
        # Fallback: try customer endpoint or just wait
        DATA=$(curl -sf "$API_BASE/api/v1/inquiries/$INQUIRY_ID") || continue
    fi

    STATUS=$(echo "$DATA" | python3 -c "import json,sys; d=json.load(sys.stdin); e=d.get('estimation') or {}; print(e.get('status','none'))" 2>/dev/null)
    VOL=$(echo "$DATA" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('volume_m3') or 0)" 2>/dev/null)
    log "  status=$STATUS  volume=${VOL} m³"

    if [[ "$STATUS" == "completed" ]]; then
        echo "$DATA" > /tmp/e2e_result.json
        break
    elif [[ "$STATUS" == "failed" || "$STATUS" == "error" ]]; then
        die "Estimation failed: $STATUS"
    fi
done

[[ ! -f /tmp/e2e_result.json ]] && die "Timed out waiting for estimation"

# ---------------------------------------------------------------------------
# 4. Print report
# ---------------------------------------------------------------------------
python3 - "$BASELINE_M3" "$TOLERANCE_PCT" <<'PYEOF'
import json, sys

baseline = float(sys.argv[1])
tolerance = float(sys.argv[2])

with open("/tmp/e2e_result.json") as f:
    d = json.load(f)

items = d.get("items", [])
vol   = float(d.get("volume_m3") or 0)
est   = d.get("estimation") or {}

moveable     = [i for i in items if i.get("is_moveable", True) and not i.get("packs_into_boxes", False)]
box_items    = [i for i in items if i.get("is_moveable", True) and i.get("packs_into_boxes", False)]
non_moveable = [i for i in items if not i.get("is_moveable", True)]

def fmt_section(title, lst):
    if not lst:
        return
    print(f"\n  ── {title} ({len(lst)}) ──")
    for it in sorted(lst, key=lambda x: x.get("volume_m3", 0), reverse=True):
        src  = it.get("volume_source", "?")
        conf = it.get("confidence", 0)
        print(f"    {it.get('name','?'):<38} {it.get('volume_m3',0):6.3f} m³  conf={conf:.0%}  src={src}")
    sub = sum(i.get("volume_m3", 0) for i in lst)
    print(f"    {'SUBTOTAL':<38} {sub:6.3f} m³")

print()
print("=" * 68)
print(f"  E2E RESULT — fresh inquiry {d.get('id','?')[:8]}…")
print("=" * 68)
print(f"  Baseline (manual quote):  {baseline:.1f} m³")
print(f"  Pipeline total:           {vol:.2f} m³")
delta = (vol - baseline) / baseline * 100 if baseline else 0
sign = "+" if delta >= 0 else ""
print(f"  Delta vs baseline:        {sign}{delta:.1f}%")
print(f"  Items: {len(moveable)} moveable / {len(box_items)} box-packable / {len(non_moveable)} non-moveable")
print(f"  Processing time:          {est.get('processing_time_ms', '?')} ms")

fmt_section("Möbel & Gegenstände (zählt zum Volumen)", moveable)
fmt_section("Kartons & Kleinteile (zählt zum Volumen)", box_items)
fmt_section("Nicht transportiert  (ausgeschlossen)", non_moveable)

print()
print("=" * 68)
passed = abs(delta) <= tolerance
print(f"  RESULT: {'PASS ✓' if passed else f'FAIL ✗  (tolerance ±{tolerance:.0f}%)'}")
print("=" * 68)
print()
sys.exit(0 if passed else 1)
PYEOF
