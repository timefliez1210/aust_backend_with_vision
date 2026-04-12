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
- Fahrkostenpauschale: computed via ORS round-trip distance × `fahrt_rate_per_km`

## XLSX Generator (`src/xlsx.rs`)

Modifies `templates/Angebot_Vorlage.xlsx` at runtime using `umya-spreadsheet`.

### Template Cell Map

| Cell/Row | Content |
|----------|---------|
| A8-A11 | Customer address block (salutation, name, street, city) |
| G14 | Date (replaces TODAY() formula) |
| A16 | Title: "Unverbindlicher Kostenvoranschlag {offer_number}" |
| B17 | Moving date |
| B18, F18 | Phone, Email |
| A20 | Greeting |
| A26-A28 | Origin address (street, city, floor) |
| F26-F28 | Destination address (street, city, floor) |
| A29 | Volume description: "Umzugspauschale X.X m³" |
| **31-42** | **Line items (max 12, warn! if exceeded — L1)** |
| G44 | **Netto total** (SUM formula) |
| J50 | Number of persons (used by G38 labor formula) |

### Print Area

Set to `'Tabelle1'!$A$1:$H$120` — columns I-P (internal calculations) are excluded from PDF.

### Items Sheet ("Erfasste Gegenstände")

If `detected_items` is non-empty, a second sheet is created with item name, volume, dimensions, confidence, and total row.

## PDF Conversion

`convert_xlsx_to_pdf()` writes XLSX to temp file, invokes LibreOffice headless (`--convert-to pdf`), reads resulting PDF. Falls back to serving XLSX directly if LibreOffice unavailable.

## Rate Back-Calculation (Telegram Edit Flow)

When Alex overrides the total price:
```
other_items_netto = sum of non-labor line items
labor_netto = target_netto - other_items_netto
rate = labor_netto / (persons × hours)
```

## Testing

`PricingEngine::new()` for defaults. `PricingEngine::with_rate(rate, surcharge)` for config-driven. `ServicePrices::from_config(config)` or `ServicePrices::defaults()` for line items.
## ⚠️ Connected Changes

| If you change... | ...also verify |
|---|---|
| Pricing formula or rates | `CompanyConfig` in core, `PricingEngine::with_rate()` call sites, `ServicePrices.from_config()`, XLSX template pricing cells, unit tests |
| XLSX template (rows, columns) | `xlsx.rs` row/col references, line item max (12), `offer_builder.rs` line item output order, `generate_offer_xlsx()` |
| Line item order or max items | XLSX rows 31–42, `warn!` threshold at line_items.len() > 12, `ServicePrices` config values |
| `build_line_items()` or service prices | foto-angebot form submission, admin dashboard service toggles, `Services` struct in core |
