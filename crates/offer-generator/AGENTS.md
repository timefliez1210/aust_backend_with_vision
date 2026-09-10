# crates/offer-generator — Pricing Engine + XLSX Template

Generates the offer PDF from inquiry data. Two main components: `PricingEngine` (pure math) and `generate_offer_xlsx` (template manipulation).

## Pricing Engine (`src/pricing.rs`)

**All rates are configurable** via `CompanyConfig` — passed through `PricingEngine::with_rate(rate_cents, saturday_surcharge_cents)`.

### Formula

```
persons_base = max(2, ceil(volume_m3 / 5.0))
floors_without_elevator = max floor without elevator across origin, destination, stop
extra_workers = max(0, highest_floor - 1)
total_persons = persons_base + extra_workers
hours = max(1.0, volume_m3 / (total_persons × 0.625))
base_labor_cents = total_persons × hours × rate_per_person_hour_cents
total = base_labor + date_adjustment
date_adjustment = saturday_surcharge_cents if Saturday, else 0
```

### Service Line Items (built in `offer_builder.rs`, not here)

Service line-item prices come from `ServicePrices` (also `CompanyConfig`-driven):
- Demontage/Montage: `assembly_price` (default €25)
- Halteverbotszone: `parking_ban_price` per zone (default €100)
- Umzugsmaterial: `packing_price` (default €30)
- 3,5t Transporter m. Koffer: `transporter_price` (default €60)
- Fahrkostenpauschale: ORS route `depot → origin → [stop] → destination → depot` (via `distance-calculator`) × `fahrt_rate_per_km` (default €1.00/km) — see `crates/distance-calculator/AGENTS.md` for why this ignores that crate's own `price_cents` field

## XLSX Generator (`src/xlsx.rs`)

Modifies `templates/offer_template.xlsx` at runtime via raw XML string surgery on
`xl/worksheets/sheet1.xml` (`apply_modifications`) — **not** via `umya-spreadsheet`
or any other spreadsheet library. The template's XML structure is stable enough for
targeted positional edits, and this avoids a full parse/reserialize round-trip that
would lose style indices and merge-cell info. `zip_util.rs` holds the shared
ZIP-read/rewrite plumbing this and the other `*_xlsx.rs` generators build on.

### Template Cell Map

The template was extended from 12 to 20 line-item slots on 2026-09-08, which shifted
everything below the item block down by 8 rows. **G44 and J50 are stale references —
the live cells are G52 and J58.**

| Cell/Row | Content |
|----------|---------|
| A8-A11 | Customer address block (salutation, name, street, city) |
| G14 | Date (replaces TODAY() formula) |
| A16 | Title: "Unverbindlicher Kostenvoranschlag {offer_number}" |
| B17 | Moving date |
| B18, F18 | Phone, Email |
| A20 | Greeting |
| A26-A28 | Origin address (street, city, floor) |
| F26-F28 | Destination address (street, city, floor); free column C between the two blocks carries the Zwischenstopp (see below) |
| A29 | Volume description: "Umzugspauschale X.X m³" |
| **31-50** | **Line items (max 20, `warn!` if exceeded)** |
| G52 | **Netto total** (`SUM(G31:G50)`) |
| J58 | Number of persons — each labor row's own formula is `IF(E{row}="", 0, F{row}*E{row}*J58)`, not a single fixed `G38` cell |

### Zwischenstopp (intermediate stop)

`OfferData` carries `stop_street`, `stop_city`, `stop_floor_info`. When any of the
three is non-empty, a "Zwischenstopp:" block is printed in the free column C between
the Belade- and Entladestelle blocks (`has_stop` gate in `xlsx.rs`). Feeds the same
fields into `build_fahrt_item` in `offer_builder.rs` so the ORS round trip routes
through the stop too (see `crates/distance-calculator/AGENTS.md`).

### Print Area

Set to `'Tabelle1'!$A$1:$H$120` — columns I-P (internal calculations) are excluded from PDF. Unchanged by the 12→20 line-item extension.

### Items Sheet ("Erfasste Gegenstände")

If `detected_items` is non-empty, a second sheet is created with item name, volume, dimensions, confidence, and total row.

## PDF Conversion

`convert_xlsx_to_pdf()` writes XLSX to temp file, invokes LibreOffice headless (`--convert-to pdf`), reads resulting PDF. Falls back to serving XLSX directly if LibreOffice unavailable.

### Terms Page (page 2) — Read Before Editing

Page 2 of the KVA is **not** in the sheet cells: it is one large text box in
`xl/drawings/drawing1.xml` inside `templates/offer_template.xlsx`. Its signature
"lines" are runs of underscore characters padded with spaces, so whether they fit
depends on the rendering font *and* the text box width. Two consequences:

- The image must carry a Calibri-metric font (`fonts-crosextra-carlito`, installed in
  `docker/Dockerfile.backend`). Without it fontconfig substitutes a wider face and every
  terms page re-wraps. `check_template_fonts()` logs the resolved family at startup.
- The box renders ~376–386pt wide, **not** the ~436pt its `<a:ext cx>` implies. Any
  line longer than that wraps. The current rules are 23 underscores + 23 spaces +
  28 underscores (336.6pt in Carlito at 11pt).

Clearing jobs (`entruempelung`, `haushaltsaufloesung`) do not use this page at all —
`substitute_clearing_page_2()` (`src/pdf_convert.rs`) splits the already-rendered PDF
with `pdfseparate`, swaps page 2 for the embedded `templates/entruempelung_kva_seite2.pdf`
(a static PDF that cannot reflow), and restitches with `pdfunite` (poppler-utils,
already in the backend image for the Telegram PDF pipeline). Before swapping it
confirms page 2 actually is the terms page by checking for the marker text "Bei
etwaigem Mehraufwand" — if a long line-item list ever pushes the layout onto an
extra page, this fails loudly instead of corrupting the KVA by substituting blindly.

### Letterhead Logo — Also Read Before Editing

The logo is a picture anchored to a spreadsheet column plus an offset in
`xl/drawings/drawing1.xml`, so where it lands depends on the *renderer's* column
widths, not the host's. In the production image Calibri falls back to Carlito,
the columns render wider, and the picture was pushed past the right print margin —
customers received KVAs reading "Aust Umzüg" (2026-09-07 incident, fixed 614507d).
**The fix is to shrink the picture, never to move its anchor** — moving it fixes the
symptom for one column-width outcome and breaks it for the next. `check-templates.py`
rasterises page 1, finds the logo as the only saturated-color element in the top
quarter, and fails unless its right edge stays 8pt clear of the print margin.

**After any template edit run `./scripts/check-templates.sh`.** It renders both terms
pages and the logo page with production's LibreOffice and fonts (inside
`aust_backend:latest`) and fails if a signature rule wrapped, expected text
disappeared, or the logo crept toward the margin. Editing the template and eyeballing
the XML is not enough — every one of these breakages looked fine in the XML.

## Rate Back-Calculation (Telegram Edit Flow)

When Alex overrides the total price:
```
other_items_netto = sum of non-labor line items
labor_netto = target_netto - other_items_netto
rate = labor_netto / (persons × hours)
```

## Other Document Generators

Three more XLSX generators live alongside the KVA one, each its own module with the
same "raw XML edits to an embedded template" approach (`zip_util.rs` shared):
- `invoice_xlsx.rs` — `generate_invoice_xlsx(&InvoiceData)`; `InvoiceType`, `InvoiceLineItem`, `ExtraService`
- `timesheet_xlsx.rs` — `generate_timesheet_xlsx(&TimesheetData)`; `TimesheetEntry`
- `travel_expense_xlsx.rs` — `generate_travel_expense_xlsx(&TravelExpenseData)`

These are not covered by `scripts/check-templates.sh` (KVA + Entrümpelung terms page
only) — a template edit to any of them needs its own manual/LibreOffice check.

## Testing

`PricingEngine::new()` for defaults. `PricingEngine::with_rate(rate, surcharge)` for config-driven. `ServicePrices::from_pricing()` or `ServicePrices::defaults()` for line items.

Several `tests/*.rs` files render real PDFs and are gated behind `--ignored` (or, for
`pdf_preview.rs`/`invoice_pdf_preview.rs`, simply require `libreoffice` on `PATH` to
pass) because they need LibreOffice — and `clearing_terms_page.rs` also needs
poppler-utils:
```
cargo test -p aust-offer-generator --test offer_zwischenstopp -- --ignored --nocapture
cargo test -p aust-offer-generator --test clearing_terms_page -- --ignored --nocapture
cargo test -p aust-offer-generator --test offer_pithan_salutation -- --ignored --nocapture
cargo test -p aust-offer-generator --test pdf_preview -- --nocapture
cargo test -p aust-offer-generator --test invoice_pdf_preview -- --nocapture
```

## ⚠️ Connected Changes

| If you change... | ...also verify |
|---|---|
| Pricing formula or rates | `CompanyConfig` in core, `PricingEngine::with_rate()` call sites, `ServicePrices.from_pricing()`, XLSX template pricing cells, unit tests |
| XLSX template (rows, columns) | `xlsx.rs` row/col references, line item max (20, rows 31-50), the shifted `G52`/`J58` cells, `offer_builder.rs` line item output order, `generate_offer_xlsx()` |
| Anything in the template's drawing/terms page or logo | `./scripts/check-templates.sh` (renders it in the prod image and checks the signature rules and logo margin) |
| Line item order or max items | XLSX rows 31–50, `warn!` threshold at `line_items.len() > 20`, `ServicePrices` config values |
| `build_line_items()` or service prices | foto-angebot form submission, admin dashboard service toggles, `Services` struct in core |
