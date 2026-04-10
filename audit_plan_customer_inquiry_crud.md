# Backend & Frontend Audit: Customer/Inquiry CRUD & Type Conversion

## Executive Summary

The recent migration (`20260410000000_unified_inquiry_customer_model.sql`) added:
- **Customer types**: `customer_type` ('private' | 'business') + `company_name`
- **Inquiry types**: `service_type` (8 types) + `submission_mode` + `recipient_id` + `billing_address_id`

However, **CRUD is incomplete** — fields exist in DB but aren't fully editable/visible across all views.

---

## 🔍 Current State Analysis

### ✅ Working Well

| Feature | Status | Notes |
|---------|--------|-------|
| DB Schema | ✅ Complete | Migration applied, constraints in place |
| CustomerRepo::CustomerRow | ✅ Complete | Has customer_type, company_name |
| Submission forms | ✅ Working | `customer_type`, `company_name` handled in submissions.rs |
| CreateInquiryModal | ✅ Working | Has customer type toggle + company name field |
| Inquiry list service_type badges | ✅ Working | SERVICE_TYPE_LABELS display correctly |
| XLSX Offer salutation logic | ✅ Working | Business customers get company name in salutation |

### ❌ Gaps Identified

| Gap | Severity | Location |
|-----|----------|------------|
| **Admin customer CRUD missing customer_type** | 🔴 High | `admin_customers.rs` - UpdateCustomerRequest doesn't include customer_type/company_name |
| **Admin customer list missing types** | 🔴 High | `admin_repo.rs` - CustomerListItem missing customer_type |
| **Frontend customer view doesn't show/edit type** | 🔴 High | `customers/[id]/+page.svelte` - no customer_type UI |
| **No customer type in inquiry detail** | 🟡 Medium | Inquiry [id] page shows service_type but not customer_type |
| **SERVICE_TYPE_LABELS duplicated** | 🟡 Medium | Defined in +page.svelte, +page.svelte [id], CreateInquiryModal.svelte |
| **Customer list doesn't show business/private** | 🟡 Medium | Can't distinguish at a glance |
| **No type conversion UX** | 🟡 Medium | Can't convert private ↔ business inline |
| **Missing customer_type in calendar side panel** | 🟢 Low | Panel shows inquiry but not customer type |

---

## 🎯 The Core Problem: Partial CRUD Implementation

```
                    Backend                                    Frontend
┌─────────────────────────────────────────────┐    ┌─────────────────────────────┐
│ DB:        ✅ customer_type, company_name   │    │ CreateInquiryModal:     ✅   │
│ CustomerRow: ✅ Has fields                   │    │ Inquiry detail:        ⚠️   │
│ Upsert:    ✅ Handles both types            │    │ Inquiry list:          ⚠️   │
│ Admin Repo: ❌ Missing in list/update/create │    │ Customer detail:       ❌   │
└─────────────────────────────────────────────┘    │ Customer list:         ❌   │
                                                   └─────────────────────────────┘

Legend: ✅ Complete | ⚠️ Partial | ❌ Missing
```

---

## 📋 Implementation Plan

### Phase 1: Backend CRUD Completion (Foundation)

#### 1.1 Extend Admin Customer Repo
crates/api/src/repositories/admin_repo.rs
```rust
// Add to CustomerListItem:
pub customer_type: Option<String>,  // NEW
pub company_name: Option<String>,   // NEW

// Add to CustomerListResponse structs

// Update queries to SELECT customer_type, company_name
// Update update_customer to accept customer_type, company_name
// Update create_customer to accept customer_type, company_name
```

**Files to modify:**
- `crates/api/src/repositories/admin_repo.rs`
- `crates/api/src/routes/admin_customers.rs` - add to UpdateCustomerRequest, CreateCustomerRequest, response structs

#### 1.2 Update Inquiry Builder for Customer Type
crates/api/src/services/inquiry_builder.rs
```rust
// Add customer_type and company_name to CustomerSnapshot
// Already has CustomerSnapshot, just extend it
```

---

### Phase 2: Frontend Shared Library (Deduplication)

#### 2.1 Create Shared Constants
crates/frontend/src/lib/utils/constants.ts
```typescript
export const SERVICE_TYPE_LABELS: Record<string, string> = {
    privatumzug: 'Privatumzug',
    firmenumzug: 'Firmenumzug',
    seniorenumzug: 'Seniorenumzug',
    umzugshelfer: 'Umzugshelfer',
    montage: 'Montage',
    haushaltsaufloesung: 'Haushaltsaufloesung',
    entruempelung: 'Entruempelung',
    lagerung: 'Lagerung',
};

export const CUSTOMER_TYPE_LABELS: Record<string, string> = {
    private: 'Privatkunde',
    business: 'Firmenkunde',
};

export const CUSTOMER_TYPE_ICONS: Record<string, string> = {
    private: 'User',
    business: 'Building2',
};
```

#### 2.2 Create Shared Customer Type Badge Component
frontend/src/lib/components/admin/CustomerTypeBadge.svelte
```svelte
<!-- Shows icon + label, supports editing via dropdown -->
```

---

### Phase 3: Frontend Customer CRUD Enhancement

#### 3.1 Customer List Enhancement
frontend/src/routes/admin/customers/+page.svelte
```svelte
<!-- Add filter for customer_type -->
<!-- Add customer_type badge in list rows -->
```

#### 3.2 Customer Detail Page Upgrade
frontend/src/routes/admin/customers/[id]/+page.svelte
```svelte
<!-- NEW: Toggle/Select for customer_type (private ↔ business) -->
<!-- NEW: Company name field (visible when business) -->
<!-- NEW: UI for billing_address (if set) -->
```

**Mockup:**
```
┌────────────────────────────────────────┐
│ Kundendaten                            │
├────────────────────────────────────────┤
│ Kundentyp: [Privatkunde ▼]            │
│                                        │
│ Name:     [________________]           │
│ Firma:    [________________]  ← conditional│
│ E-Mail:   [________________]           │
│ Telefon:  [________________]           │
│                                        │
│ Rechnungsadresse: [Anders ▼]           │
│   [________________]                    │
│                                        │
│ [Speichern]                            │
└────────────────────────────────────────┘
```

---

### Phase 4: Inquiry Context Visibility

#### 4.1 Inquiry List Card Enhancement
frontend/src/routes/admin/inquiries/+page.svelte
```svelte
<!-- Show customer_type badge next to customer name -->
<!-- Enhance service_type badge display -->
```

#### 4.2 Inquiry Detail Customer Section
frontend/src/routes/admin/inquiries/[id]/+page.svelte
```svelte
<!-- In Customer card: add customer_type badge -->
<!-- Add "Umwandeln" button to convert private ↔ business -->
```

#### 4.3 Calendar Side Panel Enhancement
frontend/src/routes/admin/calendar/CalendarSidePanel.svelte
```svelte
<!-- Add customer_type indicator to inquiry panel -->
<!-- Show company_name if business customer -->
```

---

### Phase 5: Address Flexibility (1 Address vs Multiple)

Current model already supports:
- `origin_address_id`, `destination_address_id`, `stop_address_id`
- Some inquiry types only need 1 address (e.g., `lagerung`, `entruempelung`)
- Some need 2+ (standard move)

#### 5.1 Address Type Requirements by Service Type
| Service Type | Origin Required | Destination Required | Stop Optional |
|--------------|-----------------|----------------------|---------------|
| privatumzug | ✅ | ✅ | ✅ |
| firmenumzug | ✅ | ✅ | ✅ |
| lagerung | ✅ (Einlagerung) | ❌ | ❌ |
| entruempelung | ✅ (Pick up) | ❌ | ❌ |
| montage | ✅ (Location) | ❌ | ❌ |
| seniorenumzug | ✅ | ✅ | ✅ |

#### 5.2 Validation Updates
```rust
// Add to CreateInquiryRequest validation
fn validate_addresses_for_service_type(service_type: &str, origin: Option<AddressInput>, destination: Option<AddressInput>) -> Result<(), ApiError> {
    match service_type {
        "lagerung" | "entruempelung" | "montage" => {
            // At least origin required
        },
        _ => {
            // Both required for standard moves
        }
    }
}
```

---

## 🔧 Backend Simplification Opportunities

### Simplification 1: Merge Customer Salutation Logic
Current state:
- `CustomerRow::formal_greeting()` in customer_repo.rs
- `resolve_greeting()` in customer.rs (core)
- `detect_salutation_from_name()` in customer.rs

**Issue**: Duplicated logic between core and api crates

**Fix**: Single source of truth in core, used by both

### Simplification 2: Unified Address Editor Endpoint
Create reusable `PUT /api/v1/admin/addresses/{id}` (already exists) 
but enhance to handle validation based on linked inquiry service_type

### Simplification 3: Service Type Validation Centralization
```rust
// crates/core/src/models/service_config.rs
pub struct ServiceTypeConfig {
    pub requires_origin: bool,
    pub requires_destination: bool,
    pub allows_stop: bool,
    pub pricing_formula: PricingFormula,
}

pub fn get_service_config(service_type: &str) -> Option<ServiceTypeConfig> {
    match service_type {
        "privatumzug" => ..., 
        "lagerung" => ...,
    }
}
```

---

## 📊 Implementation Priority Matrix

| Priority | Task | Effort | Impact |
|----------|------|--------|--------|
| P0 | Backend: Add customer_type/company_name to admin customer CRUD | 2h | 🔥 Critical - data integrity |
| P0 | Frontend: Customer detail type conversion UI | 3h | 🔥 Critical - user need |
| P1 | Frontend: Customer list show type badges | 1h | High visibility |
| P1 | Frontend: Extract SERVICE_TYPE_LABELS to shared module | 30min | Code quality |
| P2 | Frontend: Inquiry detail show customer type | 1h | Nice to have |
| P2 | Frontend: Calendar panel show customer type | 1h | Consistency |
| P3 | Backend: Address requirements by service type | 2h | Data validation |
| P3 | Refactor: Move salutation logic to shared | 2h | Code quality |

---

## ✅ Acceptance Criteria

### Customer Type Conversion Feature
- [ ] Admin can view customer_type on customer list
- [ ] Admin can view customer_type on customer detail
- [ ] Admin can edit customer_type (private ↔ business) on customer detail
- [ ] When switching to business, company_name field appears
- [ ] When switching from business to private, company_name cleared
- [ ] Customer type conversion persists to DB
- [ ] Customer type visible in inquiry context (list, detail, calendar)

### Inquiry Address Flexibility
- [ ] Service types requiring only 1 address validate correctly
- [ ] Service types requiring 2 addresses validate correctly
- [ ] Stop address is truly optional for all service types
- [ ] Address editor shows/hides fields based on service type

### Code Quality
- [ ] SERVICE_TYPE_LABELS exists in only one file
- [ ] CUSTOMER_TYPE_LABELS exists in only one file
- [ ] No TypeScript errors
- [ ] Backend clippy clean

---

## 🏁 Recommended Execution Order

```
Day 1: Backend Foundation
  ├── Add customer_type/company_name to admin_repo.rs
  ├── Update admin_customers.rs routes
  └── Test: API accepts and returns types

Day 2: Frontend Customer CRUD
  ├── Create shared constants.ts
  ├── Update customer list
  └── Update customer detail with conversion UI

Day 3: Frontend Inquiry Context
  ├── Update inquiry list
  ├── Update inquiry detail customer section
  └── Update calendar side panel

Day 4: Polish & Validation
  ├── Deduplicate labels
  ├── Address requirements by service type
  └── Final testing
```

---

## 💡 Additional Feature Ideas (Post-MVP)

1. **Customer Search by Type**: Filter customers list by private/business
2. **Bulk Type Conversion**: Convert multiple customers at once (rarely needed)
3. **Customer Type History**: Track when a customer converted (overkill for now)
4. **Business Customer VAT ID**: Add `vat_id` field for German business customers
