#!/usr/bin/env python3
"""
Create templates/Rechnung_Vorlage_v2.xlsx by copying offer_template.xlsx
and patching the sharedStrings.xml to relabel KVA → Rechnung header cells.

Run from repo root:
    python3 scripts/make_rechnung_template.py

What it does:
- Copies offer_template.xlsx bytes into a new ZIP
- In xl/sharedStrings.xml, replaces only the four label strings:
    "Kostenvoranschlag"   → "Rechnung"
    "Angebots-Nr."        → "Rechnungs-Nr."
    "Angebotsdatum"       → "Rechnungsdatum"
    "Angebot gültig bis"  → "Zahlungsziel"
- Writes the patched ZIP to templates/Rechnung_Vorlage_v2.xlsx

The cell layout, styling, logo, totals block, and footer are preserved
bit-for-bit from the KVA template.
"""

import shutil
import zipfile
import io
import os

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(REPO_ROOT, "templates", "offer_template.xlsx")
DST = os.path.join(REPO_ROOT, "templates", "Rechnung_Vorlage_v2.xlsx")

REPLACEMENTS = [
    # Order matters: do longer/more-specific strings first
    ("Angebot g&#252;ltig bis", "Zahlungsziel"),   # XML-escaped ü
    ("Angebot gültig bis",      "Zahlungsziel"),    # plain ü (just in case)
    ("Angebotsdatum",           "Rechnungsdatum"),
    ("Angebots-Nr.",            "Rechnungs-Nr."),
    # "Kostenvoranschlag" appears in both the big title cell and possibly labels
    # We only want to rename the static label, NOT the dynamic title that the
    # Rust code overwrites at runtime (A16 / A22).  The sharedStrings entry
    # for the static header label can be renamed safely; the Rust generator
    # overwrites A16 with "Unverbindlicher Kostenvoranschlag …" anyway, and for
    # invoices it writes "Rechnung Nr. …" to A22, so the template string is
    # never printed.  We still rename it for visual correctness when opening
    # the template directly.
    ("Kostenvoranschlag",       "Rechnung"),
]

def patch_shared_strings(xml_bytes: bytes) -> bytes:
    text = xml_bytes.decode("utf-8")
    for old, new in REPLACEMENTS:
        text = text.replace(old, new)
    return text.encode("utf-8")

def main():
    with open(SRC, "rb") as f:
        src_bytes = f.read()

    src_zip = zipfile.ZipFile(io.BytesIO(src_bytes))
    out_buf = io.BytesIO()
    out_zip = zipfile.ZipFile(out_buf, "w", zipfile.ZIP_DEFLATED)

    patched_count = 0
    for item in src_zip.infolist():
        data = src_zip.read(item.filename)
        if item.filename == "xl/sharedStrings.xml":
            original = data
            data = patch_shared_strings(data)
            if data != original:
                patched_count += 1
                print(f"  Patched {item.filename}")
        out_zip.writestr(item, data)

    out_zip.close()
    src_zip.close()

    with open(DST, "wb") as f:
        f.write(out_buf.getvalue())

    print(f"Written: {DST}  (patched {patched_count} file(s))")

if __name__ == "__main__":
    main()
