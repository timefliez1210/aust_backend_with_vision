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


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: check-templates.py OFFER_TEMPLATE_PDF CLEARING_PAGE_PDF", file=sys.stderr)
        return 2

    offer_pdf, clearing_pdf = Path(sys.argv[1]), Path(sys.argv[2])
    problems = []
    problems += check_terms_page(offer_pdf, 2, "offer_template.xlsx page 2 (Umzug terms)")
    problems += check_terms_page(clearing_pdf, 1, "entruempelung_kva_seite2.pdf (clearing terms)")

    if problems:
        print("Template layout check FAILED:")
        for p in problems:
            print(f"  - {p}")
        return 1

    print("Template layout check OK (both terms pages render with intact signature rules)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
