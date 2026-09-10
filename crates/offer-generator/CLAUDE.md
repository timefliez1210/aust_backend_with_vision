# crates/offer-generator — Pricing & XLSX/PDF Offer Generation

> **Full context**: [AGENTS.md](AGENTS.md)

Pricing engine (configurable rates) + XLSX template manipulation + LibreOffice PDF conversion.

**Pricing**: All rates in `CompanyConfig` (€30/hr labor, €25 assembly, €100 parking ban, €30 packing, €50 Saturday, €1/km travel). `PricingEngine::with_rate()` + `ServicePrices::from_pricing()`.

**XLSX**: rows 31-50 for line items (max 20, `warn!` if exceeded). Cell map in AGENTS.md.

See [AGENTS.md](AGENTS.md) for: formula, cell map, rate back-calculation, line items.