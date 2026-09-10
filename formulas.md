# Pricing Formulas

Every number here is sourced from code as of 2026-09-10. Money is stored in cents;
`offers.price_cents` and `pricing.rs` internals are **netto** (pre-VAT). Alex thinks
in brutto — the Telegram edit flow converts brutto input to netto before storing.

## VAT

```
brutto = netto × 1.19
netto  = brutto / 1.19
```
19% German Mehrwertsteuer, hardcoded as the literal `1.19` at every conversion site
(`crates/api/src/services/telegram_service.rs`, `crates/api/src/routes/invoices.rs`).
Not configurable.

## Labor Pricing (`crates/offer-generator/src/pricing.rs::PricingEngine`)

```
persons_base           = max(2, ceil(volume_m3 / 5.0))
highest_floor           = max floor without elevator, across origin/destination/stop
extra_workers           = max(0, highest_floor - 1)     // floor 1 = no extra
total_persons           = persons_base + extra_workers
hours                   = max(1.0, volume_m3 / (total_persons × 0.625))
base_labor_cents        = round(total_persons × hours × rate_per_person_hour_cents)
date_adjustment_cents   = saturday_surcharge_cents if scheduled_date is a Saturday, else 0
total_price_cents       = base_labor_cents + date_adjustment_cents
```

`0.625` m³/worker/hour is a fixed constant (5 m³ per 8h per worker) — not in `CompanyConfig`.
There is **no Sunday surcharge or "Möbellift" (moving-lift) surcharge in code** — an
earlier draft of this file proposed one for the 3rd/4th floor and above; it was never
implemented. If that's still wanted, it needs to be built, not just documented.

## Rates (`CompanyConfig`, `crates/core/src/config.rs`, DB-backed via `settings_repo.rs`)

| Rate | Field | Default |
|---|---|---|
| Labor, per person-hour | `rate_per_person_hour_cents` | 3000 (€30.00) |
| Saturday surcharge | `saturday_surcharge_cents` | 5000 (€50.00) |
| Demontage/Montage | `assembly_price` | €25.00 |
| Halteverbotszone (per zone) | `parking_ban_price` | €100.00 |
| Umzugsmaterial | `packing_price` | €30.00 |
| 3,5t Transporter m. Koffer | `transporter_price` | €60.00 |
| Fahrkostenpauschale, per km | `fahrt_rate_per_km` | €1.00 |

All seven are DB-overridable via the settings endpoints (`PricingSettings` in
`settings_repo.rs`), falling back to `CompanyConfig`'s hardcoded defaults above when
no DB row exists.

## Fahrkostenpauschale (`offer_builder.rs::build_fahrt_item`)

```
flat_total = ORS_round_trip_km × fahrt_rate_per_km
route      = depot → origin → [stop] → destination → depot   (via distance-calculator)
fallback   = distance_km × 2.0 × fahrt_rate_per_km            (if ORS call fails, or addresses missing)
```
See `crates/distance-calculator/AGENTS.md` — this crate's own `PRICE_PER_KM_CENTS`
constant (also €1.00/km) is a separate, unused-for-pricing code path; only
`fahrt_rate_per_km` from settings drives what customers are actually charged.

## Rate Back-Calculation (Telegram price override, `offer_builder.rs::calculate_rate_override`)

When Alex overrides the total netto price via Telegram:
```
other_items_netto = Σ flat_total || (quantity × unit_price), over all non-labor line items
labor_netto        = max(0, target_netto - other_items_netto)
rate               = labor_netto / (persons × hours)
```

## Not Sourced From Code (deleted from this file — do not reintroduce without building them)

The previous version of this file carried freehand notes that have no corresponding
implementation. Listed here once for the record, not as pending work:
- A floor-based "Möbellift" (furniture lift) surcharge of +€300 at the 4th floor and
  above, optional at the 3rd, +1.5h if used.
- "Über 42 Kubikmeter nachfragen" — a manual prompt to ask about truck capacity
  (7.5-tonner) above 42 m³. No such threshold or prompt exists.
- A statistical analysis feature for past jobs ("Statistische Analyse für Aufträge").
- Removing "garbage collector" (unclear referent) and making the database
  permanent, plus a DSGVO/AGB review — these are policy/ops items, not formulas.
