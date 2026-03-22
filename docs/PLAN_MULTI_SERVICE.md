# Multi-Service Type + Business Customer Support — Implementation Plan

## Context

The foto-angebot form will replace kostenloses-angebot as the primary inquiry entry point. The entire pipeline currently assumes every inquiry is a moving job (Umzug). We need to support multiple service types with conditional form rendering, business customer support, and parameterized offer generation — all while keeping backwards compatibility.

---

## Key Design Decisions

1. **All 10 service types as separate enum variants** — maximum flexibility for future pricing/templates/reporting
2. **Keep email pipeline for termin/manuell modes** — only foto/video go to API directly; termin/manuell stay on /send-mail.php → email-agent. Email parser needs to learn `service_type` field.
3. **Same pricing formula for all services** — volume → persons → hours → rate. Single-address services (Entrümpelung, Haushaltsauflösung) get a "destination" field for the disposal site (e.g. ZAH Heinde). Fahrkostenpauschale calculates route including disposal site.
4. **Parameterize XLSX labels** — single template, swap labels per service_type (e.g. "Räumungspauschale" instead of "Umzugspauschale")
5. **Billing address**: Private customers → auto-default (Einzugsadresse for moves, origin for single-address). Business customers → always show explicit billing address field. Stored as `billing_address_id` on inquiries.

---

## Service Types & Data Requirements

| Service Type | ID | Origin Addr | Destination Addr | Volume? | Fahrt? |
|---|---|---|---|---|---|
| Privatumzug | `privatumzug` | Auszugsadresse | Einzugsadresse (+ optional stop) | Yes | depot→origin→[stop]→dest→depot |
| Firmenumzug | `firmenumzug` | Auszugsadresse | Einzugsadresse | Yes | depot→origin→dest→depot |
| Seniorenumzug | `seniorenumzug` | Auszugsadresse | Einzugsadresse (+ optional stop) | Yes | depot→origin→[stop]→dest→depot |
| Haushaltsauflösung | `haushaltsaufloesung` | Räumungsadresse | Entsorgungsstelle (e.g. ZAH) | Yes | depot→origin→disposal→depot |
| Entrümpelung | `entruempelung` | Räumungsadresse | Entsorgungsstelle (e.g. ZAH) | Yes | depot→origin→disposal→depot |
| Einlagerung | `einlagerung` | Abholadresse | Lager (our warehouse) | Yes | depot→origin→warehouse→depot |
| Halteverbot | `halteverbot` | Adresse 1 | Adresse 2 (optional) | No | No |
| Demontage & Montage | `demontage_montage` | Einsatzadresse | — | No | depot→address→depot |
| Umzugsberatung | `umzugsberatung` | optional | — | No | No |
| Umzugshelfer | `umzugshelfer` | Auszugsadresse | Einzugsadresse | No | depot→origin→dest→depot |

### Behavioral Methods on `ServiceType` Enum

These methods drive conditional logic across the entire codebase — forms, validation, pricing, offer generation:

| Method | Description | True for |
|---|---|---|
| `needs_destination()` | Requires a second address | Privatumzug, Firmenumzug, Seniorenumzug, Umzugshelfer |
| `needs_volume()` | Volume estimation relevant | All Umzug types + Haushaltsaufloesung, Entruempelung, Einlagerung |
| `needs_distance()` | Route calculation needed | All except Halteverbot, Umzugsberatung |
| `is_move_type()` | Traditional moving job | Privatumzug, Firmenumzug, Seniorenumzug |
| `supports_stop_address()` | Optional intermediate stop | Privatumzug, Seniorenumzug |
| `has_disposal_destination()` | Destination is disposal site | Haushaltsaufloesung, Entruempelung |
| `german_label()` | Display name | "Privatumzug", "Entrümpelung", etc. |
| `volume_label()` | XLSX label for volume line | "Umzugspauschale" / "Räumungspauschale" / "Einlagerungspauschale" |
| `helper_label()` | XLSX label for labor line | "Umzugshelfer" / "Räumungshelfer" / "Helfer" |

---

## Phase 1: Database Migration

**New file**: `migrations/YYYYMMDD000000_service_types.sql`

```sql
-- Service type on inquiries (backwards-compat default)
ALTER TABLE inquiries
    ADD COLUMN IF NOT EXISTS service_type VARCHAR(50) NOT NULL DEFAULT 'privatumzug',
    ADD COLUMN IF NOT EXISTS billing_address_id UUID REFERENCES addresses(id);

CREATE INDEX idx_inquiries_service_type ON inquiries(service_type);

-- Business customer fields
ALTER TABLE customers
    ADD COLUMN IF NOT EXISTS is_business BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS company_name VARCHAR(255);
```

**No backfill needed** — existing rows get defaults automatically. All existing inquiries become `privatumzug`.

---

## Phase 2: Core Model Changes (Rust)

### 2a. ServiceType Enum

**File**: `crates/core/src/models/inquiry.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ServiceType {
    #[default]
    Privatumzug,
    Firmenumzug,
    Seniorenumzug,
    Haushaltsaufloesung,
    Entruempelung,
    Einlagerung,
    Halteverbot,
    DemontageMontage,
    Umzugsberatung,
    Umzugshelfer,
}
```

- Add all behavioral methods
- Add `service_type: ServiceType` to `MovingInquiry`
- Update `missing_fields()` to dispatch on `service_type`:
  - `needs_destination()` false → skip arrival_address requirement
  - `needs_volume()` false → skip volume-related fields
  - Umzugsberatung → only name + email required

### 2b. Customer Model

**File**: `crates/core/src/models/customer.rs`

- Add `is_business: bool` and `company_name: Option<String>` to `Customer`, `CreateCustomer`, `UpdateCustomer`

### 2c. Snapshot/Response Types

**File**: `crates/core/src/models/snapshots.rs`

- Add `service_type: String` to `InquiryResponse` and `InquiryListItem`
- Add `billing_address: Option<AddressSnapshot>` to `InquiryResponse`
- Add `is_business: bool`, `company_name: Option<String>` to `CustomerSnapshot`

### 2d. DB Row Type

**File**: `crates/core/src/models/quote.rs`

- Add `service_type: String` and `billing_address_id: Option<Uuid>` to `Quote`

---

## Phase 3: API Changes (Rust)

### 3a. Inquiry CRUD

**File**: `crates/api/src/routes/inquiries.rs`

- `CreateInquiryRequest`: add `service_type: Option<String>` (default "privatumzug")
- `UpdateInquiryRequest`: add `service_type: Option<String>`, `billing_address_id: Option<Uuid>`
- `ParsedInquiryForm`: add `service_type`, `is_business`, `company_name`, billing address fields
- `parse_inquiry_form()`: match new field names:
  - `"service_type" | "leistung"` → service_type
  - `"is_business" | "geschaeftskunde"` → is_business
  - `"company_name" | "firmenname"` → company_name
  - billing address fields: `"rechnungsadresse"`, `"rechnungs_plz"`, `"rechnungs_ort"`
- `handle_submission()`: relax validation — arrival_address only required when `needs_destination()`
- Create/update handlers: include `service_type`, `billing_address_id` in SQL
- `ListInquiriesQuery`: add `service_type` filter param

### 3b. Inquiry Builder

**File**: `crates/api/src/services/inquiry_builder.rs`

- Add `service_type`, `billing_address_id` to all SELECT queries
- Fetch billing address (same pattern as stop_address)
- Add `is_business`, `company_name` to customer SELECT + `CustomerSnapshot`
- Map into response types
- Add `service_type` filter to list query WHERE clause

### 3c. Shared Row Type

**File**: `crates/api/src/routes/shared.rs`

- Add `service_type` and `billing_address_id` to `InquiryRow`

### 3d. Offer Generation — Parameterize Labels

**File**: `crates/api/src/routes/offers.rs`

- Look up `service_type` from the inquiry when building offer
- Pass to XLSX generator for label selection
- Use `service_type.volume_label()` instead of hardcoded "Umzugspauschale"
- Use `service_type.helper_label()` instead of hardcoded "Umzugshelfer"

### 3e. XLSX Generator — Parameterize

**File**: `crates/offer-generator/src/xlsx.rs`

- Accept service_type labels in `XlsxData` / `OfferData`
- Write `volume_label` into cell A29 instead of hardcoded "Umzugspauschale X.X m³"
- Write `helper_label` into the labor row instead of "N Umzugshelfer"

### 3f. Orchestrator

**File**: `crates/api/src/orchestrator.rs`

- Store `service_type` in INSERT INTO inquiries
- Pass `is_business`, `company_name` to customer upsert
- For single-address services without user-specified destination: leave destination NULL (admin can add disposal site later)
- Include `service_type.german_label()` in Telegram caption

### 3g. Email Parser (for termin/manuell mode submissions)

**File**: `crates/email-agent/src/parser.rs`

- Add `service_type: Option<String>` to `FormSubmission` struct
- Parse from JSON attachment field `"leistung"` or `"service_type"`
- Pass through to `MovingInquiry` (defaults to `privatumzug` if absent — backwards compat)

### 3h. Admin Endpoints

**File**: `crates/api/src/routes/admin.rs`

- Add `is_business`, `company_name` to customer create/update DTOs and queries

---

## Phase 4: Frontend — foto-angebot Form

**File**: `frontend/src/routes/foto-angebot/+page.svelte`

### 4a. Service Type Selector (New First Step)

Card/button group with icon + label for each service type, grouped visually:

**Umzüge:**
- Privatumzug — "Privater Wohnungsumzug"
- Firmenumzug — "Büro- oder Firmenumzug"
- Seniorenumzug — "Seniorengerechter Umzug"

**Räumungen:**
- Haushaltsauflösung — "Kompletträumung einer Wohnung"
- Entrümpelung — "Entrümpelung und Entsorgung"

**Weitere Leistungen:**
- Einlagerung — "Möbel sicher einlagern"
- Halteverbot — "Halteverbotszonen beantragen"
- Demontage & Montage — "Möbel ab- und aufbauen"
- Umzugsberatung — "Kostenlose Beratung"
- Umzugshelfer — "Helfer ohne Transporter"

Selection determines which form sections render below.

### 4b. Business Customer Toggle

- Checkbox "Geschäftskunde?" in contact section
- When checked: reveal `Firmenname` text field + full billing address fields (Straße, Nr, PLZ, Ort)
- Auto-switch Privatumzug ↔ Firmenumzug when business toggle changes (if current selection is one of these)

### 4c. Conditional Form Sections

| Condition | Controls |
|---|---|
| `needsDestination` | Show arrival address section |
| `needsVolume` | Show mode selector (termin/manuell/foto/video) |
| `showStopAddress` | Show intermediate stop toggle |
| `showDisposalDest` | Show "Entsorgungsstelle" destination field |
| `showStorageDest` | Show "Lageradresse" destination field |
| `minimalForm` (Umzugsberatung) | Contact + message only, hide all address/volume sections |
| `noVolumeSimple` (Halteverbot, D&M, Helfer) | Addresses + contact, no volume calc/mode selector |

### 4d. Address Labels Per Service Type

| Service Type | Origin Label | Destination Label |
|---|---|---|
| *umzug types | Auszugsadresse | Einzugsadresse |
| Haushaltsauflösung | Räumungsadresse | Entsorgungsstelle (optional) |
| Entrümpelung | Räumungsadresse | Entsorgungsstelle (optional) |
| Einlagerung | Abholadresse | Lageradresse (optional) |
| Halteverbot | Adresse 1 | Adresse 2 (optional) |
| Demontage/Montage | Einsatzadresse | — |
| Umzugshelfer | Startadresse | Zieladresse |
| Umzugsberatung | — | — |

### 4e. Submission Flow

- **Photo/video modes**: append `service_type`, `is_business`, `company_name`, billing address fields to multipart FormData → backend API
- **Termin/manuell modes**: append same fields to /send-mail.php form data → email pipeline → email-agent

Field name mapping for form data:
- `service_type` / `leistung`
- `is_business` / `geschaeftskunde`
- `company_name` / `firmenname`
- `rechnungsadresse` (billing street + nr)
- `rechnungs_plz` (billing postal code)
- `rechnungs_ort` (billing city)

### 4f. Admin Dashboard Updates

**Files**: `frontend/src/routes/admin/inquiries/...`, `frontend/src/routes/admin/customers/...`

- Service type badge/column on inquiry list + filter dropdown
- Service type shown in inquiry detail
- Business customer badge + company name on customer list/detail
- `is_business` / `company_name` fields in customer edit form
- Billing address display in inquiry detail (if different from destination/origin)

---

## Phase 5: Mobile App Changes

**Files in**: `/media/timefliez/FileSystem/projects/alex_aust_app/src/`

- Add `service_type` selector to scan form (limited to move types initially: Privatumzug, Firmenumzug, Seniorenumzug — mobile app is photo-focused)
- Add `is_business` toggle + `company_name` field
- Include `service_type`, `is_business`, `company_name` in FormData submission
- **Old app versions** that don't send these → backend defaults to `privatumzug`, no breakage

---

## Backwards Compatibility Matrix

| Scenario | Behavior |
|---|---|
| Old mobile app (no service_type field) | Backend defaults to `privatumzug` |
| Email pipeline (no service_type in form JSON) | Parser defaults to `privatumzug` |
| Existing DB rows | Column default `'privatumzug'` applied by migration |
| Old admin dashboard (before frontend update) | service_type not shown but stored; no breakage |
| termin/manuell with service_type via email | Email parser extracts from JSON, passes through |
| Non-move inquiry without destination | `destination_address_id` stays NULL, offer gen still works (Fahrt = depot→origin→depot) |

---

## Implementation Order

| Step | Scope | Risk |
|---|---|---|
| 1. Migration | Add columns (all have defaults) | Zero risk |
| 2. Core models | ServiceType enum, extend structs | Compile-time only |
| 3. API layer | Extend parsers, handlers, builders, queries | Medium — many files |
| 4. Offer generation | Parameterize labels, pass service_type | Low — label swaps only |
| 5. Email parser | Add service_type field support | Low — backwards compat default |
| 6. Frontend foto-angebot | Service selector, conditional rendering, business fields | High — major form rework |
| 7. Admin dashboard | Display + filter new fields | Low |
| 8. Mobile app | Service type selector + business fields | Low |

Steps 1-5 (backend) can be done as one branch/PR. Steps 6-8 (frontend/app) can follow.

---

## Verification Checklist

- [ ] `\d inquiries` shows `service_type`, `billing_address_id`
- [ ] `\d customers` shows `is_business`, `company_name`
- [ ] `POST /api/v1/submit/photo` with `service_type=entruempelung` + origin only → 201
- [ ] `POST /api/v1/submit/photo` without `service_type` → defaults to `privatumzug`, pipeline works as before
- [ ] `GET /api/v1/inquiries` → response includes `service_type`
- [ ] `GET /api/v1/inquiries?service_type=entruempelung` → filter works
- [ ] Generate offer on Entrümpelung inquiry → XLSX says "Räumungspauschale" not "Umzugspauschale"
- [ ] Frontend: each service type → correct form sections show/hide
- [ ] Frontend: toggle Geschäftskunde → billing address + company name appear
- [ ] Existing inquiries in admin → all show as "Privatumzug"
- [ ] Old mobile app submission → still works, creates `privatumzug` inquiry

---

## Open Questions / Future Work

- **Default disposal destination**: Should Entrümpelung/Haushaltsauflösung auto-fill "ZAH Heinde" as destination, or leave blank for admin to fill?
- **Default warehouse address**: Should Einlagerung auto-fill our warehouse address?
- **Different pricing per service type**: Defer to Phase 2 — currently all use same formula
- **Separate XLSX templates**: Defer — parameterized labels sufficient for now
- **Einlagerung duration**: Monthly pricing / duration field — defer to Phase 2
- **Service-specific add-on checkboxes**: Should Entrümpelung show different service checkboxes than Umzug?
