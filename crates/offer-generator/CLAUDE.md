# crates/offer-generator — Pricing & PDF Generation

Generates moving offers with pricing calculation and PDF output using Typst.

## Key Files

- `src/pricing.rs` - PricingEngine: base price, surcharges, volume/distance pricing
- `src/pdf.rs` - PdfGenerator: Typst-based PDF rendering
- `src/templates.rs` - German offer letter Typst template
- `src/error.rs` - OfferError enum

## Pricing Engine

Calculates final price from:
- Base price
- Volume-based pricing (per m3)
- Distance-based pricing (per km)
- Floor/elevator surcharges
- Packing service fees

## PDF Generation

Uses Typst to render a German offer letter template with:
- Company branding
- Customer details
- Moving details (origin, destination, date)
- Itemized pricing breakdown
- Terms and conditions

Output stored in S3 via StorageProvider.

## Status

Partially implemented — template and pricing logic need finalization for production use.

## Dependencies

typst, typst-pdf, aust-storage (for PDF upload), chrono, uuid.
