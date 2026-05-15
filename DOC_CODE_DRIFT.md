# AUST Backend — Doc / Code Drift Audit Report

**Date:** 2026-05-15  
**Auditor:** Hermes Agent  
**Scope:** All `AGENTS.md`, `README.md`, `CLAUDE.md`, `ARCHITECTURE.md` files across backend crates, frontend submodule, app submodule, and Python vision service.

---

## Summary Table

| # | Doc File | Claim | Actual State | Severity |
|---|----------|-------|-------------|----------|
| 1 | `AGENTS.md:122` | Status state machine: `pending → estimating` directly, no `info_requested` | `InquiryStatus::InfoRequested` exists; `info_requested` is a valid state between `pending` and `estimating` | MODERATE |
| 2 | `AGENTS.md:127-128` | `is_locked_for_modifications()` prevents volume/address edits after `offer_ready` | Function **does not exist** in `crates/core/src/models/inquiry.rs`. PATCH handler does not implement locking. | HIGH |
| 3 | `AGENTS.md:141` | "219 tests" for `cargo test --lib --workspace` | Actual count: 242 tests (151 + 20 + 0 + 19 + 5 + 2 + 44 + 2 + 0) | LOW |
| 4 | `AGENTS.md:150` | Connected changes table says check `is_locked_for_modifications()` when changing status | Function does not exist. The 3-place enforcement claim is false. | HIGH |
| 5 | `AGENTS.md:152` | Connected changes: `ServicePrices::from_config()` | Method **does not exist** in code. Actual method is `ServicePrices::from_pricing()` in `offer_builder.rs:916-939`. | MODERATE |
| 6 | `README.md:165` | Crate overview lists `aust-calendar` | Directory `crates/calendar/` **does not exist**. Calendar logic lives in `aust-api` (`calendar_repo.rs`, `calendar_item_repo.rs`). | MODERATE |
| 7 | `README.md:180` | Database table list includes `quotes` | Table `quotes` **does not exist**. It was renamed to `inquiries` (migration `20260410000000_unified_inquiry_customer_model.sql`). | HIGH |
| 8 | `README.md:185` | Database table list includes `calendar_bookings`, `calendar_capacity_overrides` | Both tables **were dropped** in migration `20260307000000_drop_calendar_bookings.sql`. Current calendar uses `calendar_items` and `settings`. | HIGH |
| 9 | `README.md:197` | API endpoint group "Quotes" at `/api/v1/quotes/` | No such route exists. Quote/inquiry endpoints are under `/api/v1/inquiries/`. | MODERATE |
| 10 | `docs/ARCHITECTURE.md:16` | Dependency graph shows `crates/calendar` as a crate | `crates/calendar/` does not exist. | MODERATE |
| 11 | `docs/ARCHITECTURE.md:181` | Table list includes `calendar_bookings`, `calendar_capacity_overrides` | Tables were dropped (see #8). | HIGH |
| 12 | `crates/api/AGENTS.md:3` | "18 route files, 16 repository modules, 8 service modules" | Routes: 19 `.rs` files (including `mod.rs`). Repos: 18 `.rs` files (including `mod.rs`). Services: 10 `.rs` files (including `mod.rs`). | LOW |
| 13 | `crates/api/AGENTS.md:11-23` | Route table lists 13 files; omits 5 others | Missing: `distance.rs`, `flash_contact.rs`, `health.rs`, `offers.rs`, `shared.rs` | LOW |
| 14 | `crates/api/AGENTS.md:29-43` | Repo table lists 15 modules; omits `settings_repo.rs` | `settings_repo.rs` exists (6.7KB). Total: 16 repo modules including `mod.rs`. | LOW |
| 15 | `crates/api/AGENTS.md:49-56` | Services table lists 8 files; omits 2 | Missing: `flash_contact_service.rs` (8.4KB) and `vision.rs` (7.3KB). | LOW |
| 16 | `crates/api/AGENTS.md:77-78` | "`InquiryStatus::is_locked_for_modifications()` returns true for `offer_ready` through `paid`... PATCH rejects changes to volume/address" | Function does not exist. `inquiries.rs` PATCH handler (`lines 384-521`) has **no locking logic** for volume/address fields. | HIGH |
| 17 | `crates/api/AGENTS.md:90-92` | "`ServicePrices::from_config(config)`" | Method does not exist. Actual: `ServicePrices::from_pricing()` in `offer_builder.rs:916-939`. | MODERATE |
| 18 | `crates/api/AGENTS.md:99` | Test helpers mention factories for "day, day-employee" | `inquiry_days` and `inquiry_day_employees` tables **were dropped** (migration `20260601000000_simplify_scheduling.sql`). Factories no longer insert days. | MODERATE |
| 19 | `crates/core/AGENTS.md:10` | "`InquiryStatus`... `can_transition_to()` + `is_locked_for_modifications()`" | `is_locked_for_modifications()` **does not exist** in `core/src/models/inquiry.rs`. | HIGH |
| 20 | `crates/core/AGENTS.md:26` | "`is_locked_for_modifications()` — returns true for `offer_ready` through `paid`" | Function does not exist. | HIGH |
| 21 | `crates/core/AGENTS.md:52` | Connected changes: "`can_transition_to()`, `is_locked_for_modifications()`" | `is_locked_for_modifications()` does not exist. | HIGH |
| 22 | `crates/core/AGENTS.md:14` | `UserRole` enum lists variants: `Admin, Buerokraft, Employee` | Actual variants in `core/src/models/user.rs:11-22`: `Admin`, `Buerokraft`, `Operator`. No `Employee` variant — `Operator` is the third. | MODERATE |
| 23 | `crates/offer-generator/AGENTS.md:33` | Template file: `templates/Angebot_Vorlage.xlsx` | Actual file is named `templates/offer_template.xlsx`. `Angebot_Vorlage.xlsx` does not exist. | MODERATE |
| 24 | `crates/offer-generator/AGENTS.md:51` | Print area set to `'Tabelle1'!$A$1:$H$120` | Not directly verified in code; `fix_print_area()` exists but sheet name may differ. | LOW |
| 25 | `crates/offer-generator/AGENTS.md:80` | "`ServicePrices::from_config(config)` or `ServicePrices::defaults()`" | `from_config()` does not exist. `defaults()` was not found either — `from_pricing()` is the constructor. | MODERATE |
| 26 | `crates/storage/AGENTS.md` | `StorageProvider` trait lists `exists()` method | `crates/storage/src/traits.rs:38` defines `upload`, `download`, `delete` — **no `exists()` method**. | MODERATE |
| 27 | `crates/email-agent/AGENTS.md:43` | "IMAP sender for form submissions is always the company inbox (`umzug@example.com`)" | Actual company inbox is `angebot@aust-umzuege.de` (per `processor.rs:339-343` and `core/src/models/inquiry.rs:201-202`). | LOW |
| 28 | `frontend/AGENTS.md` | Lists `calendar_bookings` and `calendar_capacity_overrides` in DB tables | Both tables were dropped (see #8). | MODERATE |
| 29 | `frontend/README.md:39-40` | "10 service pages" in `leistungen/`, "4 guide/blog articles" in `ratgeber/` | `leistungen/` has **16** subdirectories (16 service pages). `ratgeber/` has **11** subdirectories (11 guide articles). | LOW |
| 30 | `frontend/AGENTS.md` | Worker routes: `/worker/jobs/[id]` | Actual route is `/worker/jobs/` (no dynamic `[id]` param found in app directory). | LOW |

---

## Detailed Findings

### 🔴 HIGH Severity

#### Finding 1: `is_locked_for_modifications()` is a Ghost Function

**Files affected:**
- `AGENTS.md:127-128`
- `crates/api/AGENTS.md:77-78`
- `crates/core/AGENTS.md:10, 26, 52`

**The claim:** `InquiryStatus::is_locked_for_modifications()` exists and returns `true` for `offer_ready` through `paid`, preventing volume/address/service edits after an offer is generated.

**The reality:**
```bash
$ grep -n 'fn is_locked_for_modifications' crates/core/src/models/inquiry.rs
# (no output — function does not exist)

$ grep -n 'is_locked_for_modifications' crates/api/src/routes/inquiries.rs
# (no output — not used in PATCH handler)
```

The only place locking is tested is in the integration test `locked_inquiry_rejects_volume_change` at `integration_tests.rs:597`, but the production PATCH handler (`inquiries.rs:384-521`) does **not** call any lock check. The `can_transition_to()` function exists and returns `true` for all transitions, but the locking layer is completely absent.

**Impact:** Developers reading AGENTS.md believe edits are protected after `offer_ready`. In production, an admin can accidentally change volume or addresses on a sent offer, causing invoice/offers drift.

**Fix:** Either implement `is_locked_for_modifications()` and wire it into the PATCH handler, or remove all references from docs and Connected Changes tables.

---

#### Finding 2: README Lists `quotes` Table That Does Not Exist

**File:** `README.md:180`

**Claim:** ```| `quotes` | Moving quote requests, status tracking, volume and distance |```

**Reality:** The `quotes` table was renamed to `inquiries` in migration `20260410000000_unified_inquiry_customer_model.sql`. All code references `inquiries`.

**Fix:** Replace `quotes` with `inquiries` in README line 180.

---

#### Finding 3: README Lists Dropped Calendar Tables

**File:** `README.md:185-186` (older version); ARCHITECTURE.md also references them.

**Claim:** `calendar_bookings` and `calendar_capacity_overrides` are key tables.

**Reality:** Migration `20260307000000_drop_calendar_bookings.sql` dropped both tables. The current schema uses `calendar_items` (non-inquiry work blocks + jobs) and capacity is managed via the `settings` table.

**Fix:** Replace with `calendar_items` and `settings`.

---

### 🟡 MODERATE Severity

#### Finding 4: `ServicePrices::from_config()` Does Not Exist

**Files:** `crates/api/AGENTS.md:90-92`, `crates/offer-generator/AGENTS.md:80`, `AGENTS.md:152`

**Claim:** `ServicePrices::from_config(config)` is the constructor used in non-test code.

**Reality:** `grep -rn 'ServicePrices::from_config' crates/` returns zero results. The actual constructor is:
```rust
// crates/api/src/services/offer_builder.rs:916-939
impl ServicePrices {
    pub fn from_pricing(pricing: &PricingResult, config: &CompanyConfig) -> Self { ... }
    pub fn defaults() -> Self { ... }
}
```

**Fix:** Replace all doc references from `from_config()` to `from_pricing()`.

---

#### Finding 5: `aust-calendar` Crate Listed But Missing

**Files:** `README.md:165`, `docs/ARCHITECTURE.md:16`

**Claim:** `aust-calendar` is a workspace crate responsible for booking + capacity management.

**Reality:** `crates/calendar/` does not exist. Calendar logic lives inside `aust-api` (`calendar_repo.rs`, `calendar_item_repo.rs`). `Cargo.toml` workspace members do not include `crates/calendar`.

**Fix:** Remove `aust-calendar` from README crate overview and ARCHITECTURE.md dependency graph.

---

#### Finding 6: UserRole Enum Has `Operator`, Not `Employee`

**File:** `crates/core/AGENTS.md:14`

**Claim:** `UserRole` variants are `Admin, Buerokraft, Employee`.

**Reality:**
```rust
// crates/core/src/models/user.rs:11-22
pub enum UserRole {
    Admin,
    #[default]
    Buerokraft,
    Operator,   // ← NOT "Employee"
}
```

`Operator` is the legacy alias for `Buerokraft`. There is no `Employee` variant in `UserRole`.

**Fix:** Change `Employee` to `Operator` in `crates/core/AGENTS.md`.

---

#### Finding 7: Offer Template Filename Wrong

**File:** `crates/offer-generator/AGENTS.md:33`

**Claim:** Template file is `templates/Angebot_Vorlage.xlsx`.

**Reality:**
```bash
$ ls templates/*.xlsx
offer_template.xlsx
Rechnung_Vorlage.xlsx
Rechnung_Vorlage_v2.xlsx
...
```
`Angebot_Vorlage.xlsx` does not exist. The code likely loads `offer_template.xlsx` (verify via `include_bytes!` or `File::open`).

**Fix:** Update doc to reference `offer_template.xlsx`.

---

#### Finding 8: `StorageProvider` Trait Missing `exists()`

**File:** `crates/storage/AGENTS.md`

**Claim:** `StorageProvider` trait includes `exists(key: &str) -> Result<bool, StorageError>`.

**Reality:**
```rust
// crates/storage/src/traits.rs:38
pub trait StorageProvider: Send + Sync {
    async fn upload(...) -> Result<String, StorageError>;
    async fn download(...) -> Result<Vec<u8>, StorageError>;
    async fn delete(...) -> Result<(), StorageError>;
}
```
No `exists()` method is defined. `offer_builder.rs` uses upload/download/delete only.

**Fix:** Remove `exists()` from trait documentation or implement the method.

---

#### Finding 9: Test Helpers Refer to Dropped Day Tables

**File:** `crates/api/AGENTS.md:99`

**Claim:** Test helpers create factories for "customer, address, inquiry, employee, **day, day-employee**, estimation".

**Reality:** The `inquiry_days` and `inquiry_day_employees` tables were dropped in migration `20260601000000_simplify_scheduling.sql`. `test_helpers.rs` has `insert_test_inquiry_employee` (flat table) but no day/day-employee factories.

**Fix:** Update doc to match actual factory list.

---

#### Finding 10: Status State Machine Omits `info_requested`

**File:** `AGENTS.md:119-124`

**Claim:** State machine shows `pending → estimating → estimated → ...`

**Reality:** `core/src/models/inquiry.rs:19` defines `InfoRequested` as a valid status. AGENTS.md skips it in the ASCII diagram. The actual state machine is:
```
pending → info_requested → estimating → estimated → offer_ready → offer_sent
```

**Fix:** Add `info_requested` to the ASCII state machine.

---

#### Finding 11: Frontend README Undercounts Pages

**File:** `frontend/README.md:39-40`

**Claim:** "10 service pages" and "4 guide/blog articles".

**Reality:**
- `leistungen/`: 16 subdirectories (16 service pages)
- `ratgeber/`: 11 subdirectories (11 guide articles)

**Fix:** Update counts or remove them.

---

### 🟢 LOW Severity

#### Finding 12: API Module Counts Slightly Off

**File:** `crates/api/AGENTS.md:3`

**Claim:** "18 route files, 16 repository modules, 8 service modules"

**Reality:** 19 route .rs files, 18 repo .rs files, 10 service .rs files (including `mod.rs`, `flash_contact_service.rs`, `vision.rs`).

**Fix:** Update counts or note "excluding `mod.rs`".

---

#### Finding 13: API Route Table Omissions

**File:** `crates/api/AGENTS.md:11-23`

**Claim:** 13 route files listed.

**Reality:** 18 route .rs files exist. Missing from table: `distance.rs`, `flash_contact.rs`, `health.rs`, `offers.rs`, `shared.rs`. These are legitimate routes (e.g. `/api/v1/distance/` handles ORS proxy, `health.rs` serves `/health` and `/ready`).

**Fix:** Add missing routes to table.

---

#### Finding 14: Test Count Off

**File:** `AGENTS.md:141`

**Claim:** "219 tests" for `cargo test --lib --workspace`.

**Reality:** 242 tests pass. The count has grown since the doc was written.

**Fix:** Update count or remove specific number.

---

#### Finding 15: Email Agent Inbox Placeholder

**File:** `crates/email-agent/AGENTS.md:43`

**Claim:** IMAP sender is always `umzug@example.com`.

**Reality:** Actual inbox is `angebot@aust-umzuege.de`.

**Fix:** Replace placeholder with actual address or use `<company-inbox>`.

---

## Recommended Fixes Priority

| Priority | Fix | Effort |
|----------|-----|--------|
| **Immediate** | Remove/replace all `is_locked_for_modifications()` references OR implement the function | 1-2 hrs |
| **Immediate** | Fix README table: `quotes` → `inquiries`, remove dropped calendar tables | 10 min |
| **High** | Fix `from_config()` → `from_pricing()` in 3 docs | 5 min |
| **High** | Remove `aust-calendar` crate reference, fix ARCHITECTURE.md graph | 10 min |
| **High** | Fix `UserRole` variant `Employee` → `Operator` | 2 min |
| **High** | Fix template filename: `Angebot_Vorlage.xlsx` → `offer_template.xlsx` | 2 min |
| **Medium** | Fix state machine diagram: add `info_requested` | 2 min |
| **Medium** | Remove `exists()` from `StorageProvider` doc or implement it | 15 min |
| **Medium** | Update test helper factory list | 5 min |
| **Low** | Update route/service/repo counts and tables | 10 min |
| **Low** | Update frontend page counts or remove them | 2 min |
| **Low** | Update test count (242) | 1 min |
