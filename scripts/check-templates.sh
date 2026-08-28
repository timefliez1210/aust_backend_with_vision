#!/usr/bin/env bash
#
# Render the KVA templates the way production does, and assert they still fit.
#
# Why: the KVA's terms page is a text box inside templates/offer_template.xlsx whose
# "signature lines" are runs of underscore characters. Whether they fit depends on the
# rendering font and the box width, so a template edit can look fine in the XML and
# still ship a wrapped, broken page to a customer. That has now happened twice
# (2026-08-27: font substitution, then a line 40pt too wide for its box).
#
# The rendering must happen with production's LibreOffice *and* production's fonts,
# so this runs inside the backend image rather than against whatever the host has.
#
# Usage:
#   ./scripts/check-templates.sh              # uses aust_backend:latest
#   IMAGE=aust_backend:previous ./scripts/check-templates.sh
#
# Requires: docker, poppler-utils (pdftotext), python3.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="${IMAGE:-aust_backend:latest}"

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "Image '$IMAGE' not found locally."
    echo "Build it first:  docker build -f docker/Dockerfile.backend -t aust_backend:latest ."
    exit 1
fi

if ! command -v pdftotext >/dev/null 2>&1; then
    echo "pdftotext is required (apt install poppler-utils)."
    exit 1
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cp "$REPO_ROOT/templates/offer_template.xlsx" "$WORK/"
cp "$REPO_ROOT/templates/entruempelung_kva_seite2.pdf" "$WORK/"

echo ">>> Rendering offer_template.xlsx inside $IMAGE"
docker run --rm -v "$WORK:/w" -w /w "$IMAGE" \
    sh -c 'soffice --headless --calc --convert-to pdf offer_template.xlsx >/dev/null 2>&1'

if [ ! -f "$WORK/offer_template.pdf" ]; then
    echo "LibreOffice produced no PDF — the conversion itself failed."
    exit 1
fi

# The font substitution that broke the page once already: report it here too, since
# a wrong font is the most likely cause of any wrap the checker is about to find.
FONT="$(docker run --rm "$IMAGE" fc-match -f '%{family}' Calibri 2>/dev/null || echo '?')"
echo ">>> Calibri resolves to: $FONT"

python3 "$REPO_ROOT/scripts/check-templates.py" \
    "$WORK/offer_template.pdf" "$WORK/entruempelung_kva_seite2.pdf"
