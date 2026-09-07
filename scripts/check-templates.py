#!/usr/bin/env python3
"""Assert that the KVA templates still lay out correctly once rendered.

Called by scripts/check-templates.sh with the rendered PDFs; not meant to be run
directly. See that script for why this exists.

The checks are deliberately about *layout*, not content: the templates are edited
as binary zips and their text boxes reflow silently, so what has to be pinned is
"does it still fit on one line", which only a rendered PDF can answer.
"""

import re
import subprocess
import sys
import tempfile
from pathlib import Path

# A signature rule that fits reads "____   ____" on one line. When the line is too
# wide for its text box, LibreOffice wraps it and the second group lands on a line
# of its own — a line consisting of nothing but underscores.
ONLY_UNDERSCORES = re.compile(r"^\s*_+\s*$")
TWO_GROUPS = re.compile(r"^\s*_+\s+_+\s*$")

# Every terms page carries these; their absence means the page was swapped or
# emptied rather than merely reflowed.
REQUIRED_TEXT = [
    "Datum, Unterschrift Kunde",
    "Datum, Unterschrift Umzugsunternehmen",
    "Der Auftrag wurde ordnungsgemäß nach Kostenvoranschlag",
]


def page_text(pdf: Path, page: int) -> str:
    """Extract one page as laid-out text (layout mode keeps the line breaks)."""
    return subprocess.run(
        ["pdftotext", "-layout", "-f", str(page), "-l", str(page), str(pdf), "-"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout


def check_terms_page(pdf: Path, page: int, label: str) -> list[str]:
    """Return a list of problems found on one terms page (empty when healthy)."""
    text = page_text(pdf, page)
    lines = text.splitlines()
    problems = []

    wrapped = [ln for ln in lines if ONLY_UNDERSCORES.match(ln)]
    if wrapped:
        problems.append(
            f"{label}: {len(wrapped)} signature rule(s) wrapped onto their own line — "
            f"the line is too wide for its text box"
        )

    intact = [ln for ln in lines if TWO_GROUPS.match(ln)]
    if len(intact) != 2:
        problems.append(
            f"{label}: expected 2 intact signature rules (customer + company, twice), found {len(intact)}"
        )

    for needle in REQUIRED_TEXT:
        if needle not in text:
            problems.append(f"{label}: missing expected text {needle!r}")

    return problems


# --- Logo -----------------------------------------------------------------
#
# The letterhead logo is a picture anchored to a spreadsheet column plus an
# offset, so where it lands depends on the *renderer's* column widths. In the
# production image Calibri falls back to Carlito, the columns come out wider,
# and the picture was pushed past the right print margin — customers received
# KVAs reading "Aust Umzüg" (reported 2026-09-07). The host's LibreOffice never
# showed it, which is exactly why this check renders inside the image.
#
# The logo is the only coloured element in the top quarter of the page, so it
# can be found by looking for saturated pixels there.
RENDER_DPI = 100
PAGE_MARGIN_IN = 0.7          # <pageMargins right="0.7"> in the template
LOGO_MIN_CLEARANCE_PT = 8.0   # keep a visible gap, not a hairline


def _render_page1_ppm(pdf: Path) -> tuple[int, int, bytes]:
    """Rasterise page 1 as raw RGB. PPM keeps this dependency-free (no Pillow).

    pdftoppm writes nothing when asked for stdout in some poppler builds, so the
    page goes through a temp file instead.
    """
    with tempfile.TemporaryDirectory() as tmp:
        prefix = Path(tmp) / "page"
        subprocess.run(
            ["pdftoppm", "-singlefile", "-r", str(RENDER_DPI),
             "-f", "1", "-l", "1", str(pdf), str(prefix)],
            check=True,
            capture_output=True,
        )
        raw = prefix.with_suffix(".ppm").read_bytes()
    if not raw.startswith(b"P6"):
        raise RuntimeError("pdftoppm did not return a P6 PPM")
    fields, pos = [], 2
    while len(fields) < 3:          # width, height, maxval
        while raw[pos : pos + 1].isspace():
            pos += 1
        if raw[pos : pos + 1] == b"#":
            pos = raw.index(b"\n", pos) + 1
            continue
        end = pos
        while not raw[end : end + 1].isspace():
            end += 1
        fields.append(int(raw[pos:end]))
        pos = end
    width, height, _ = fields
    return width, height, raw[pos + 1 :]


def check_logo(pdf: Path, label: str) -> list[str]:
    """Return problems with the letterhead logo (empty when it fits)."""
    width, height, pixels = _render_page1_ppm(pdf)
    band = height // 4                      # letterhead only; skips the orange banner
    right_ink = -1
    for y in range(band):
        row = y * width * 3
        for x in range(width):
            i = row + x * 3
            r, g, b = pixels[i], pixels[i + 1], pixels[i + 2]
            if max(r, g, b) - min(r, g, b) > 40:   # coloured, not black text
                right_ink = max(right_ink, x)

    if right_ink < 0:
        return [f"{label}: no logo found in the top quarter of page 1"]

    printable_right = width - PAGE_MARGIN_IN * RENDER_DPI
    clearance_pt = (printable_right - right_ink) / RENDER_DPI * 72
    if clearance_pt < LOGO_MIN_CLEARANCE_PT:
        return [
            f"{label}: the logo reaches {clearance_pt:.1f}pt of the right print margin "
            f"(needs {LOGO_MIN_CLEARANCE_PT:.0f}pt) — it is clipped or about to be. "
            f"Shrink the picture in xl/drawings/drawing1.xml; do not move it."
        ]
    return []


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: check-templates.py OFFER_TEMPLATE_PDF CLEARING_PAGE_PDF", file=sys.stderr)
        return 2

    offer_pdf, clearing_pdf = Path(sys.argv[1]), Path(sys.argv[2])
    problems = []
    problems += check_terms_page(offer_pdf, 2, "offer_template.xlsx page 2 (Umzug terms)")
    problems += check_terms_page(clearing_pdf, 1, "entruempelung_kva_seite2.pdf (clearing terms)")
    problems += check_logo(offer_pdf, "offer_template.xlsx page 1 (logo)")

    if problems:
        print("Template layout check FAILED:")
        for p in problems:
            print(f"  - {p}")
        return 1

    print("Template layout check OK (signature rules intact, logo inside the print area)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
