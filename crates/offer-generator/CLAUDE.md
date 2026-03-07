# crates/offer-generator — Pricing & XLSX/PDF Offer Generation

> XLSX template layout + two separate hours systems: [../../docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md#offer-xlsx-template-layout)
> Recurring bugs (XLSX preset values, LibreOffice missing, brutto/netto): [../../docs/DEBUGGING.md](../../docs/DEBUGGING.md)

Generates moving offers from an XLSX template, converts to PDF via LibreOffice, and calculates pricing.

## Key Files

- `src/pricing.rs` - PricingEngine: labor cost from volume/distance/floors, date surcharges
- `src/xlsx.rs` - XlsxGenerator: fills XLSX template with offer data, manages print area
- `src/pdf_convert.rs` - LibreOffice-based XLSX → PDF conversion
- `src/error.rs` - OfferError enum
- `src/lib.rs` - Re-exports

## Pricing Engine

Calculates from `PricingInput` → `PricingResult`:

- **Persons**: `max(2, 2 + floor_extra_origin + floor_extra_dest)` — floors without elevator add extra helpers
- **Hours**: `ceil(volume_m3 / (persons × 2.0))` — 2 m³/person/hour throughput
- **Labor**: `persons × hours × €30/hr` (rate_per_person_hour = 3000 cents)
- **Distance**: `distance_km × €1.50/km` (rate_per_km = 150 cents)
- **Date adjustment**: Saturday +€50

### Floor Parsing

German floor strings → numeric: `"Erdgeschoss"` → 0, `"1. Stock"` → 1, ..., `"Höher als 6. Stock"` → 7.

## XLSX Generation

Uses `umya-spreadsheet` (v2.3.3) to fill an XLSX template (`templates/Angebot_Vorlage.xlsx`).

### Template Structure (Sheet "Tabelle1")

| Cell/Row | Content |
|----------|---------|
| A8-A11 | Customer address block (salutation, name, street, city) |
| G14 | Date (replaces TODAY() formula with actual date) |
| A16 | Title: "Unverbindlicher Kostenvoranschlag {offer_number}" |
| B17 | Moving date |
| B18, F18 | Phone, Email |
| A20 | Greeting |
| A26-A28 | Origin address (street, city, floor) |
| F26-F28 | Destination address (street, city, floor) |
| A29 | Volume description: "Umzugspauschale X.X m³" |
| **31-42** | **Line items (see below)** |
| G44 | **Netto total** (sum formula) |
| J50 | Number of persons (used by G38 formula) |

### Line Item Rows (31-42)

All preset values in columns E (quantity) and F (unit price) are cleared to 0 before writing. Only explicit line items contribute to the netto total.

| Row | Description | E (Qty) | F (Price) | Notes |
|-----|------------|---------|-----------|-------|
| 31 | De/Montage | 1.0 | €50 | If montage/demontage service requested |
| 32 | Halteverbotszone | 1-2 | €100 | Count of parking ban locations |
| 33 | Umzugsmaterial + Einpackservice | 1.0 | €30 | If packing service requested |
| 38 | N Umzugshelfer | hours | rate/hr | Labor: G38 = E38 × F38 × J50 |
| 39 | Transporter | 1-2 | €60 | 2 trucks if volume > 30m³ |
| 42 | Anfahrt/Abfahrt | 1.0 | €30+km×1.50 | Distance-based |

### Print Area

Set to `'Tabelle1'!$A$1:$H$120` — excludes internal calculation columns I-P from the PDF.

### Items Sheet ("Erfasste Gegenstände")

If `detected_items` is non-empty, a second sheet is created with:
- Headers: Gegenstand, Volumen (m³), Maße (L×B×H), Konfidenz
- One row per detected/parsed item
- Bold total row at bottom

## PDF Conversion

`convert_xlsx_to_pdf()` writes XLSX to a temp file, invokes LibreOffice in headless mode (`--convert-to pdf`), reads the resulting PDF. Falls back to serving XLSX directly if LibreOffice is unavailable.

## Rate Back-Calculation

When Alex overrides the total price via Telegram edit, the hourly rate is back-calculated:

```
other_items_netto = sum of non-labor line items (rows 31-42 except 38)
labor_netto = target_netto - other_items_netto
rate = labor_netto / (persons × hours)
```

This ensures the XLSX formula in G38 produces the correct netto total in G44.

## Dependencies

umya-spreadsheet, aust-storage (for PDF upload), chrono, uuid, serde.
