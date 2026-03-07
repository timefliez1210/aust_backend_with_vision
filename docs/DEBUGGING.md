# Debugging — Known Failure Patterns

> Recurring bugs and their fixes. Read this before investigating unexpected behavior.
> Architecture overview: [ARCHITECTURE.md](ARCHITECTURE.md)

---

## 1. Employee Hours Invisible After Assignment

**Symptom**: Assign a Mitarbeiter to an accepted inquiry, open the employee's hours dashboard for the current month → planned hours show 0 / assignment not listed.

**Root cause**: SQL filter uses `COALESCE(cb.booking_date, i.preferred_date::date) BETWEEN $start AND $end`. When a newly accepted inquiry has no calendar booking AND no preferred_date, COALESCE returns NULL → BETWEEN with NULL is always false → row silently excluded.

**Fix**: Add `ie.created_at::date` as third fallback:
```sql
COALESCE(cb.booking_date, i.preferred_date::date, ie.created_at::date) BETWEEN $1 AND $2
```

**Files**: `crates/api/src/routes/admin.rs` — `employee_hours_summary()` and `fetch_employee_month_hours()`.

---

## 2. Contact Form Customer Has No Name/Phone in DB

**Symptom**: A Kontakt form email arrives, a customer row is created in the DB, but `name`, `first_name`, `last_name`, and `phone` are all NULL.

**Root cause**: `find_or_create_thread()` in `email-agent/src/processor.rs` originally used `ON CONFLICT (email) DO NOTHING` with only the `(id, email)` columns → created a bare stub. Contact form inquiries never reach `orchestrator.rs::handle_complete_inquiry()` (which has the full upsert) because `is_complete()` returns false (no addresses).

**Fix**: Pass `MovingInquiry` to `find_or_create_thread()` and use a full `ON CONFLICT DO UPDATE SET ... = COALESCE(EXCLUDED.x, customers.x)` so name/salutation/phone are stored whenever the parsed email contains them.

**Files**: `crates/email-agent/src/processor.rs` — `find_or_create_thread()`.

---

## 3. Telegram Approval Loop Freezes / Duplicate Messages

**Symptom**: An approval message is sent to Telegram, but button presses have no effect, or the same draft appears twice.

**Root cause**: Two running instances of the backend are both polling `/getUpdates` with the same bot token. The Telegram API delivers each update to only one poller — they race, neither processes correctly.

**Fix**: Ensure only one instance runs at a time. Check with `systemctl status aust-backend` and kill any orphaned processes before starting a new one:
```bash
pkill -f "target/debug/aust_backend"
# or in production:
sudo systemctl restart aust-backend
```

**Files**: `crates/email-agent/src/telegram.rs` — polling loop.

---

## 4. XLSX Line Items Show Wrong Totals

**Symptom**: Offer PDF has incorrect netto totals, or line items that should be zero are contributing to the sum.

**Root cause**: The XLSX template has preset non-zero values in the E (quantity) and F (unit price) columns of rows 31-42. If the generator writes new items without first clearing these presets, the old values persist and sum into G44.

**Fix**: The generator must set all rows 31-42 columns E and F to 0 before writing any line item. Verify in `crates/offer-generator/src/xlsx.rs` that the clear loop runs before the write loop.

---

## 5. ORS Directions API Returns 406

**Symptom**: `DistanceError::Api` with HTTP 406 from OpenRouteService when calculating driving distances.

**Root cause**: The ORS Directions endpoint requires `Accept: application/geo+json;charset=UTF-8`. Sending `Accept: application/json` (the default) causes 406. Note: the Geocode endpoint uses `application/json` (correct).

**Fix**: In `crates/distance-calculator/src/router.rs`, ensure the directions request sets:
```rust
.header("Accept", "application/geo+json;charset=UTF-8")
```

---

## 6. Price Override Interpreted as Netto When It Should Be Brutto

**Symptom**: Alex types `"350 Euro"` on Telegram, the offer shows a different netto (e.g. 294.12) but customer sees 350 on the invoice.

**Root cause**: Alex always thinks in **Brutto** prices. The LLM parser correctly converts bare prices to netto by dividing by 1.19. This is expected behavior, not a bug.

**Pattern**: In `crates/api/src/orchestrator.rs` — `llm_parse_edit_instructions()`:
- `"350 Euro"` → netto = 350 / 1.19 = €294.12 → stored as `price_cents_netto = 29412`
- The PDF shows netto; the customer invoice with VAT shows 294.12 × 1.19 = €350.00

**Do not change this** without discussing with Alex.

---

## 7. IMAP Sender vs. Real Customer Email

**Symptom**: Customer in DB has email `angebot@aust-umzuege.de` (the company inbox) instead of the real customer email.

**Root cause**: All form submissions arrive via the company inbox. The IMAP sender is always the company email. The real customer email is inside the JSON attachment (field `email`) or the email body.

**Fix**: Already handled in `processor.rs` — after `merge_inquiry()`, the processor checks if `inquiry.email` (from parsed form data) differs from the IMAP sender and uses the parsed one. If you see this bug re-appear, check `find_or_create_thread()` is being called with `customer_email_final` (after the override), not the raw IMAP sender.

**Files**: `crates/email-agent/src/processor.rs` — `process_incoming_email()`, around the `find_or_create_thread` call.

---

## 8. SQLx Compile-Time Query Errors After Migration

**Symptom**: `cargo build` fails with `error: column "x" of relation "y" does not exist` or similar SQLx offline cache errors.

**Root cause**: SQLx validates queries at compile time against a cached schema snapshot (`.sqlx/` directory) or a live DB. If a migration was run but `.sqlx/` is stale, or the DB wasn't migrated, queries that reference new columns fail.

**Fix**:
```bash
# Option A: regenerate offline cache (requires live DB with migrations applied)
cargo sqlx prepare -- --lib

# Option B: run against live DB (disable offline mode)
DATABASE_URL=postgres://... cargo build
```

---

## 9. LibreOffice PDF Conversion Fails Silently

**Symptom**: Offer PDF is served as XLSX instead of PDF. No error in logs, but the downloaded file has an XLSX extension.

**Root cause**: LibreOffice (`soffice`) is not installed or not on PATH. `convert_xlsx_to_pdf()` in `crates/offer-generator/src/pdf_convert.rs` falls back to returning the XLSX bytes when the `soffice` process exits non-zero.

**Fix**: Install LibreOffice on the server:
```bash
sudo apt install libreoffice --no-install-recommends
which soffice  # verify
```

---

## 10. Calendar COALESCE NULL Pattern (General)

Several queries use `COALESCE(cb.booking_date, i.preferred_date::date)` to get an "effective date" for an inquiry. This pattern is fragile when:
- No calendar booking exists yet (newly accepted inquiry)
- `preferred_date` is NULL (inquiry from kontakt form, no date specified)

Both being NULL is the worst case — any `BETWEEN`, `ORDER BY`, or `WHERE` on the COALESCE result will silently drop or mis-order these rows.

**General fix pattern**: Always add a final non-null fallback:
```sql
COALESCE(cb.booking_date, i.preferred_date::date, ie.created_at::date)
-- or for inquiries without an ie row:
COALESCE(cb.booking_date, i.preferred_date::date, i.created_at::date)
```

Search for this pattern in new queries before shipping:
```bash
grep -rn "COALESCE.*booking_date.*preferred_date" crates/
```
