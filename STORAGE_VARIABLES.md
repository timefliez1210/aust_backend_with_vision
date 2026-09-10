# Database Storage Variables

All tables and columns persisted in PostgreSQL. Derived values (e.g. `actual_hours` from clock timestamps) are computed at query time and not stored.

---

## `customers`
| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK, time-ordered v7 |
| `email` | VARCHAR(255) | Unique. Primary contact identifier |
| `name` | VARCHAR(255) | Display name (legacy, kept for compat) |
| `first_name` | TEXT | Structured given name |
| `last_name` | TEXT | Structured family name |
| `salutation` | TEXT | "Herr" / "Frau" / "D" |
| `phone` | VARCHAR(50) | Contact phone number |
| `created_at` | TIMESTAMPTZ | Row creation time |
| `updated_at` | TIMESTAMPTZ | Last modification time (auto-trigger) |

---

## `addresses`
| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `street` | VARCHAR(255) | Street + house number |
| `city` | VARCHAR(100) | City name |
| `postal_code` | VARCHAR(20) | Postal code |
| `country` | VARCHAR(100) | Default: "Österreich" |
| `floor` | VARCHAR(50) | Floor label (e.g. "2", "EG") — affects labor cost |
| `elevator` | BOOLEAN | Whether elevator is available |
| `needs_parking_ban` | BOOLEAN | Whether a parking ban zone is needed |
| `latitude` | DOUBLE PRECISION | Geocoded latitude |
| `longitude` | DOUBLE PRECISION | Geocoded longitude |
| `created_at` | TIMESTAMPTZ | Row creation time |

---

## `inquiries`
Main entity tracking the full lifecycle of a moving job.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `customer_id` | UUID | FK → customers |
| `origin_address_id` | UUID | FK → addresses (pickup location) |
| `destination_address_id` | UUID | FK → addresses (delivery location) |
| `stop_address_id` | UUID | FK → addresses (optional intermediate stop) |
| `status` | VARCHAR(50) | Lifecycle state — see status machine below |
| `source` | VARCHAR(50) | How the inquiry arrived (direct_email, photo_webapp, mobile_app, etc.) |
| `estimated_volume_m3` | DOUBLE PRECISION | Total estimated move volume |
| `distance_km` | DOUBLE PRECISION | ORS-calculated route distance |
| `preferred_date` | TIMESTAMPTZ | **Deprecated** — kept but no longer read by app code (backfilled into `scheduled_date`, migration `20260401000000`). Use `scheduled_date`. |
| `scheduled_date` | DATE | Job start date (admin-confirmed or backfilled) |
| `end_date` | DATE | Job end date for multi-day moves; NULL for single-day jobs |
| `start_time` | TIME | Job start time, default 09:00 |
| `end_time` | TIME | Job end time, default 17:00 |
| `has_pauschale` | BOOLEAN | Whether the job is billed as a flat Umzugspauschale rather than hourly |
| `services` | JSONB | Boolean flags: packing, assembly, disassembly, storage, disposal, parking_ban_origin, parking_ban_destination |
| `notes` | TEXT | Internal admin notes |
| `customer_message` | TEXT | Original message from customer |
| `offer_sent_at` | TIMESTAMPTZ | When offer was emailed to customer |
| `accepted_at` | TIMESTAMPTZ | When customer accepted the offer |
| `created_at` | TIMESTAMPTZ | Row creation time |
| `updated_at` | TIMESTAMPTZ | Last modification time (auto-trigger) |

**Status machine:** `pending → info_requested → estimating → estimated → offer_ready → offer_sent → accepted | rejected | expired | cancelled → scheduled → completed → invoiced → paid`

---

## `inquiry_days`
Per-day records for multi-day moves.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `inquiry_id` | UUID | FK → inquiries |
| `day_date` | DATE | Calendar date for this day |
| `day_number` | SMALLINT | Sequential day index (1, 2, 3…) |
| `notes` | TEXT | Per-day notes |
| `created_at` | TIMESTAMPTZ | Row creation time |

---

## `volume_estimations`
Result of any volume estimation run (LLM vision, depth sensor, video, manual inventory).

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `inquiry_id` | UUID | FK → inquiries |
| `method` | VARCHAR(50) | "vision", "depth_sensor", "video", "inventory" |
| `status` | VARCHAR(50) | "processing", "completed", "failed" |
| `source_data` | JSONB | Raw input data (image keys, depth maps, etc.) |
| `result_data` | JSONB | Parsed items list with names, volumes, quantities, confidence |
| `total_volume_m3` | DOUBLE PRECISION | Summed volume of all detected items |
| `confidence_score` | DOUBLE PRECISION | Overall estimation confidence 0–1 |
| `created_at` | TIMESTAMPTZ | Row creation time |

---

## `offers`
Generated price offer (Kostenvoranschlag) for an inquiry.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `inquiry_id` | UUID | FK → inquiries |
| `offer_number` | TEXT | Human-readable offer number (sequence-based) |
| `status` | VARCHAR(50) | "draft", "sent", "accepted", "rejected", "cancelled" — **not reliably maintained**; most historical rows are stuck at "draft" even after the offer was sent/accepted, so don't treat this column as source of truth for lifecycle state. `inquiries.status` is authoritative. |
| `price_cents` | BIGINT | Total netto price in cents |
| `currency` | VARCHAR(3) | Always "EUR" |
| `persons` | INTEGER | Number of movers calculated |
| `hours_estimated` | DOUBLE PRECISION | Estimated job duration |
| `rate_per_hour_cents` | BIGINT | Hourly rate per person in cents |
| `line_items_json` | JSONB | All line items with labels, quantities, prices |
| `fahrt_override_cents` | INTEGER | Manual override for Fahrkostenpauschale; if set, ORS recalc is skipped |
| `pdf_storage_key` | VARCHAR(255) | S3 key of the generated PDF |
| `valid_until` | DATE | Offer expiry date |
| `sent_at` | TIMESTAMPTZ | When the offer PDF was emailed to the customer |
| `followup_last_pinged_on` | DATE | Calendar day (Europe/Berlin) the KVA follow-up nag last fired; dedupes to one ping/day |
| `followup_muted` | BOOLEAN | Set to stop the follow-up nag for this offer |
| `created_at` | TIMESTAMPTZ | Row creation time |
| `updated_at` | TIMESTAMPTZ | Last modification time (auto-trigger) |

A KVA replaces its predecessor **in place** — every active-offer query filters out
`status = 'superseded'` so regenerating an offer doesn't leave duplicates visible.

---

## `invoices`
Formal invoice document attached to a completed inquiry.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `inquiry_id` | UUID | FK → inquiries |
| `invoice_number` | TEXT | Unique invoice number, format `YYYY-NN` (e.g. "2026-87"), zero-padded to a two-digit minimum and restarting at `-01` each January — see `invoice_number_counters` below |
| `invoice_type` | VARCHAR(20) | "full", "partial_first" (Anzahlung), "partial_final" (Restbetrag) |
| `partial_group_id` | UUID | Links the two invoices in a partial pair |
| `partial_percent` | INTEGER | Downpayment percentage (e.g. 30) — only on partial_first |
| `status` | VARCHAR(20) | "draft", "ready", "sent", "paid" |
| `payment_method` | VARCHAR(50) | Zahlungsart — free text, but the register only ever writes "EC" or "BAR" (normalized from "EC-Karte"/"Bar" in migration `20260821100000`) |
| `paid_amount_cents` | BIGINT | Amount actually received; NULL means "no partial payment recorded" — fully paid is `paid_at IS NOT NULL`, not this column |
| `extra_services` | JSONB | Additional line items not in the offer (e.g. Klaviertransport) |
| `pdf_s3_key` | TEXT | S3 key of the generated PDF |
| `sent_at` | TIMESTAMPTZ | When invoice was sent to customer |
| `paid_at` | TIMESTAMPTZ | When payment was confirmed |
| `created_at` | TIMESTAMPTZ | Row creation time |

---

## `invoice_number_counters`
Per-calendar-year invoice number allocator (migration `20260821100000`), replacing the
older global `invoice_number_seq` so numbering restarts each January the way Alex's
Excel register does. `invoices` and `storage_invoices` share the same number space.

| Column | Type | Notes |
|--------|------|-------|
| `year` | INT | PK |
| `last_value` | BIGINT | Highest number handed out for that year; next allocation is `last_value + 1` |

`invoice_number_seq` is still present (unused by new allocations) — it is left in place
rather than dropped because dropping it would touch an already-applied migration's effects.

---

## `invoice_reminders`
Dunning (Mahnwesen) state for one unpaid invoice. Created automatically when an invoice
is marked sent.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `invoice_id` | UUID | FK → invoices, UNIQUE (one reminder row per invoice) |
| `level` | INT | 1 = Zahlungserinnerung, 2 = 1. Mahnung, 3 = 2. Mahnung |
| `status` | TEXT | "pending", "sent", "snoozed", "closed" |
| `remind_after` | DATE | Reminder becomes actionable on/after this date (+7 days per level by default) |
| `created_at` / `updated_at` | TIMESTAMPTZ | |

---

## `review_requests`
Tracks whether a Google-review request email has been sent after an inquiry completes.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `inquiry_id` | UUID | FK → inquiries, UNIQUE |
| `status` | TEXT | "pending", "sent", "skipped" |
| `remind_after` | DATE | When "Später" was chosen, the date to resurface the reminder |
| `sent_at` | TIMESTAMPTZ | |
| `created_at` / `updated_at` | TIMESTAMPTZ | |

---

## `storage_contracts` / `storage_invoices`
"Lagerung" (storage rental) side business — deliberately isolated from the
inquiry→offer→invoice pipeline. The only shared resource is the invoice number
space (`invoice_number_counters` above).

**`storage_contracts`**

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `customer_id` | UUID | FK → customers |
| `billing_address_id` | UUID | FK → addresses, optional override |
| `contract_start` / `contract_end` | DATE | `contract_end` NULL = open-ended |
| `sqm` | NUMERIC(6,1) | Rented square metres, printed on the invoice line item |
| `monthly_netto_cents` | BIGINT | Stored netto; entered brutto in the admin UI (÷1.19 on the way in) |
| `billing_day` | SMALLINT | 1–28, anniversary billing day, derived from `contract_start` |
| `status` | VARCHAR(20) | "active", "ended", "cancelled" |
| `note` | TEXT | |
| `created_at` / `updated_at` | TIMESTAMPTZ | |

**`storage_invoices`** — one per contract per calendar month, generated by an hourly
background tick (`storage_billing_service`), gated on `pending_approval` until an admin
approves it.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `contract_id` | UUID | FK → storage_contracts |
| `invoice_number` | TEXT | UNIQUE, shares the `invoice_number_counters` space |
| `period_year` / `period_month` | INT | Billed calendar month |
| `netto_cents` | BIGINT | |
| `pdf_s3_key` | TEXT | |
| `status` | VARCHAR(20) | "pending_approval", "sent", "paid", "cancelled" |
| `payment_method` | TEXT | Parity with `invoices.payment_method` |
| `paid_amount_cents` | BIGINT | Parity with `invoices.paid_amount_cents` |
| `notes` | TEXT | |
| `created_at` / `approved_at` / `sent_at` | TIMESTAMPTZ | |

Idempotency: `UNIQUE (contract_id, period_year, period_month)`, relied on by the billing
tick's `INSERT ... ON CONFLICT DO NOTHING`.

---

## `vehicles` / `vehicle_reminders`
Fleet management. Each vehicle carries an open list of free-form due-date reminders
(TÜV, Ölwechsel, …); a background tick pings the admin Telegram chat at 21/14/7 days
out, then daily from 7 days out through the due date and while overdue, until the
reminder is marked done or dismissed.

| Table | Key columns |
|-------|-------------|
| `vehicles` | `id`, `label`, `created_at`, `updated_at` |
| `vehicle_reminders` | `id`, `vehicle_id` (FK), `label`, `due_date`, `active` (BOOLEAN — false once done/dismissed), `completed_at`, `last_pinged_on` (DATE, dedupes to one ping/day) |

---

## `notes`
General-purpose admin notepad (unrelated to `inquiries.notes`).

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `title` / `content` | VARCHAR / TEXT | |
| `color` | VARCHAR(20) | UI tag color |
| `pinned` | BOOLEAN | Pinned notes sort first |
| `created_at` / `updated_at` | TIMESTAMPTZ | |

---

## `feedback_reports`
Bug/feature reports submitted via the admin dashboard (the internal issue tracker for
this codebase — see `AGENT_API.md`).

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `report_type` | VARCHAR(20) | "bug", "feature" |
| `priority` | VARCHAR(20) | "low", "medium", "high", "critical" |
| `title` / `description` / `location` | TEXT | |
| `attachment_keys` | TEXT[] | S3 keys of attached screenshots |
| `status` | VARCHAR(20) | "open", "in_progress", "resolved" |
| `created_at` / `updated_at` | TIMESTAMPTZ | |

The list endpoint sits behind roughly a minute of edge caching; the by-id GET is the
source of truth if a just-created or just-updated report doesn't show up in a list yet.

---

## `customer_addresses`
Per-customer reusable address book (migration `20260703000000`), independent of the
`addresses` table. Deliberately **not** a foreign key into `addresses`: those rows are
per-inquiry snapshots that get mutated in place, and the book must survive that.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `customer_id` | UUID | FK → customers |
| `street`, `house_number`, `postal_code`, `city`, `country`, `floor`, `elevator`, `parking_ban`, `latitude`, `longitude` | — | Same shape as `addresses` |
| `label` | TEXT | Optional human label ("Alte Wohnung", "Firma") shown in the picker |
| `source` | VARCHAR(20) | "inquiry" (harvested), "manual", "email" |
| `last_used_at` | TIMESTAMPTZ | Drives most-recently-used sort in the picker |
| `created_at` | TIMESTAMPTZ | |

Dedup: unique index on `(customer_id, lower(street), coalesce(house_number,''), coalesce(postal_code,''), lower(city))`.

---

## `inquiry_appointments` / `inquiry_appointment_employees`
Zusatztermine linked to an inquiry on their own dates outside the main move
(Besichtigung, Halteverbotszonen-Aufbau, etc.) — the move itself stays a contiguous
`[scheduled_date, end_date]` range on `inquiries`.

**`inquiry_appointments`**

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `inquiry_id` | UUID | FK → inquiries |
| `kind` | VARCHAR(50) | Free text, default "besichtigung" (deliberately no CHECK — labels have churned before) |
| `scheduled_date`, `start_time`, `end_time` | DATE/TIME | |
| `assignee_id` | UUID | FK → employees, optional single assignee |
| `address_id` | UUID | FK → addresses, optional structured address (falls back to free-text `location`) |
| `location`, `description`, `notes`, `employee_notes` | TEXT | |
| `status` | VARCHAR(50) | "scheduled", "done", "cancelled" |
| `created_at` / `updated_at` | TIMESTAMPTZ | |

**`inquiry_appointment_employees`** — full crew+hours junction promoted in migration
`20260722120000` (e.g. paid Halteverbotszonen-Aufbau), mirroring `calendar_item_employees`
minus `job_date` (an appointment is always one day): `planned_hours`, `start_time`,
`end_time`, `break_minutes`, `actual_hours`, `clock_in`/`clock_out`,
`employee_clock_in`/`employee_clock_out`, `employee_break_minutes`, `notes`,
`transport_mode`, `travel_costs_cents`.

---

## Assistant subsystem tables (Josie)

The Telegram assistant ("Josie") runs in-process inside `aust_backend` (see
`crates/assistant/AGENTS.md`) and owns its own set of tables, not detailed column-by-column
here to avoid drift with that crate's own docs:

`agent_sessions`, `agent_episodes`, `agent_memory`, `agent_actions`, `agent_todos`,
`agent_reminders`, `agent_briefing_log`, `pending_actions`, `pending_memory_proposals`,
`telegram_chat_bindings`, `offer_observations`, `vision_revision_requests`,
`domain_events`, `domain_events_archive`, `payment_records`, `hours_adjustments`, `settings`.

Privilege note: these tables (and every other business table) are reachable from the
**same** API connection pool the assistant uses — the least-privilege `aust_assistant`
DB role that migration `20260609000008` created has had all its grants revoked
(`20260609000027`) and is not enforced today. See
[docs/assistant_db_role_risk.md](docs/assistant_db_role_risk.md).

---

## `email_threads`
Groups email messages belonging to one conversation.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `customer_id` | UUID | FK → customers |
| `inquiry_id` | UUID | FK → inquiries (nullable) |
| `subject` | VARCHAR(500) | Email subject line |
| `created_at` | TIMESTAMPTZ | Row creation time |
| `updated_at` | TIMESTAMPTZ | Last message time (auto-trigger) |

---

## `email_messages`
Individual email messages within a thread.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `thread_id` | UUID | FK → email_threads |
| `direction` | VARCHAR(10) | "inbound" (from customer) or "outbound" (to customer) |
| `from_address` | VARCHAR(255) | Sender email address |
| `to_address` | VARCHAR(255) | Recipient email address |
| `subject` | VARCHAR(500) | Message subject |
| `body_text` | TEXT | Plain text body |
| `body_html` | TEXT | HTML body |
| `message_id` | VARCHAR(255) | IMAP/SMTP Message-ID header for deduplication |
| `status` | VARCHAR(50) | Message state ("draft", "sent", "delivered") |
| `llm_generated` | BOOLEAN | Whether body was drafted by the LLM |
| `created_at` | TIMESTAMPTZ | Row creation time |

---

## `employees`
Employee profiles for moving staff.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `salutation` | VARCHAR(10) | "Herr", "Frau", "D" |
| `first_name` | VARCHAR(255) | Given name |
| `last_name` | VARCHAR(255) | Family name |
| `email` | VARCHAR(255) | Unique. Used for OTP login to worker portal |
| `phone` | VARCHAR(50) | Contact phone |
| `monthly_hours_target` | DECIMAL(6,2) | Expected hours per month, default 160 |
| `active` | BOOLEAN | Soft-delete flag — inactive employees are hidden |
| `arbeitsvertrag_key` | TEXT | S3 key for uploaded employment contract PDF |
| `mitarbeiterfragebogen_key` | TEXT | S3 key for uploaded employee questionnaire PDF |
| `created_at` | TIMESTAMPTZ | Row creation time |
| `updated_at` | TIMESTAMPTZ | Last modification time (auto-trigger) |

---

## `inquiry_employees`
Junction table: employee assigned to a moving inquiry.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `inquiry_id` | UUID | FK → inquiries |
| `employee_id` | UUID | FK → employees |
| `planned_hours` | DECIMAL(6,2) | Admin-set expected hours; auto-derived from clock_in/clock_out when both are set |
| `clock_in` | TIMESTAMPTZ | Admin-set actual start time |
| `clock_out` | TIMESTAMPTZ | Admin-set actual end time |
| `employee_clock_in` | TIMESTAMPTZ | Employee self-reported start time (via worker portal) |
| `employee_clock_out` | TIMESTAMPTZ | Employee self-reported end time (via worker portal) |
| `notes` | TEXT | Per-assignment notes |
| `created_at` | TIMESTAMPTZ | Row creation time |
| `updated_at` | TIMESTAMPTZ | Last modification time (auto-trigger) |

> `actual_hours` and `employee_actual_hours` are **derived** at query time: `EXTRACT(EPOCH FROM (clock_out - clock_in)) / 3600.0`

---

## `calendar_items`
Internal work events (training, vehicle maintenance, etc.) that need employee assignment.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `title` | VARCHAR(255) | Event name |
| `description` | TEXT | Optional longer description |
| `category` | VARCHAR(50) | Type of event (internal, training, etc.) |
| `location` | TEXT | Where the event takes place |
| `customer_id` | UUID | FK → customers (optional; null for internal events) |
| `scheduled_date` | DATE | Date of the event |
| `start_time` | TIME | Start time, default 09:00 |
| `end_time` | TIME | End time (optional) |
| `duration_hours` | NUMERIC(5,2) | Planned total duration |
| `status` | VARCHAR(50) | "scheduled", "completed", "cancelled" |
| `created_at` | TIMESTAMPTZ | Row creation time |
| `updated_at` | TIMESTAMPTZ | Last modification time (auto-trigger) |

---

## `calendar_item_days`
Per-day records for multi-day calendar items.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `calendar_item_id` | UUID | FK → calendar_items |
| `day_date` | DATE | Calendar date for this day |
| `day_number` | SMALLINT | Sequential day index (1, 2, 3…) |
| `notes` | TEXT | Per-day notes |
| `created_at` | TIMESTAMPTZ | Row creation time |

---

## `calendar_item_employees`
Junction table: employee assigned to a calendar item.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `calendar_item_id` | UUID | FK → calendar_items |
| `employee_id` | UUID | FK → employees |
| `planned_hours` | NUMERIC(5,2) | Admin-set expected hours; auto-derived from clock_in/clock_out when both are set |
| `clock_in` | TIMESTAMPTZ | Admin-set actual start time |
| `clock_out` | TIMESTAMPTZ | Admin-set actual end time |
| `employee_clock_in` | TIMESTAMPTZ | Employee self-reported start time (via worker portal) |
| `employee_clock_out` | TIMESTAMPTZ | Employee self-reported end time (via worker portal) |
| `notes` | TEXT | Per-assignment notes |
| `created_at` | TIMESTAMPTZ | Row creation time |

> `actual_hours` and `employee_actual_hours` are **derived** at query time.

---

## `calendar_capacity_overrides`
Per-date capacity overrides (default capacity is set in config).

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `override_date` | DATE | The specific date being overridden (unique) |
| `capacity` | INT | Max concurrent jobs for this date |
| `created_at` | TIMESTAMPTZ | Row creation time |

---

## `users`
Admin and operator users for the dashboard.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `email` | VARCHAR(255) | Unique login email |
| `password_hash` | VARCHAR(255) | Argon2id hash |
| `name` | VARCHAR(255) | Display name |
| `role` | VARCHAR(50) | "admin" or "operator" |
| `created_at` | TIMESTAMPTZ | Row creation time |
| `updated_at` | TIMESTAMPTZ | Last modification time (auto-trigger) |

---

## `admin_password_resets`
OTP tokens for admin password reset flow.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `user_id` | UUID | FK → users |
| `otp_hash` | TEXT | Hashed OTP code |
| `expires_at` | TIMESTAMPTZ | Token expiry |
| `used_at` | TIMESTAMPTZ | When the token was consumed (null = unused) |
| `created_at` | TIMESTAMPTZ | Row creation time |

---

## `customer_otps`
Short-lived 6-digit OTP codes for customer magic-link login.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `email` | VARCHAR(255) | Customer email the code was sent to |
| `code` | VARCHAR(6) | 6-digit OTP |
| `expires_at` | TIMESTAMPTZ | Code expiry (short-lived) |
| `used` | BOOLEAN | Consumed flag — codes are single-use |
| `created_at` | TIMESTAMPTZ | Row creation time |

---

## `customer_sessions`
Long-lived DB-backed session tokens for authenticated customers.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `customer_id` | UUID | FK → customers |
| `token` | VARCHAR(64) | Random opaque token sent in Authorization header |
| `expires_at` | TIMESTAMPTZ | Session expiry (30 days) |
| `created_at` | TIMESTAMPTZ | Row creation time |

---

## `employee_otps`
Short-lived 6-digit OTP codes for employee worker-portal login.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `email` | VARCHAR(255) | Employee email the code was sent to |
| `code` | VARCHAR(6) | 6-digit OTP |
| `expires_at` | TIMESTAMPTZ | Code expiry |
| `used` | BOOLEAN | Consumed flag — single-use |
| `created_at` | TIMESTAMPTZ | Row creation time |

---

## `employee_sessions`
Long-lived DB-backed session tokens for authenticated employees (worker portal).

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID | PK |
| `employee_id` | UUID | FK → employees |
| `token` | VARCHAR(64) | Random opaque token sent in Authorization header |
| `expires_at` | TIMESTAMPTZ | Session expiry (30 days) |
| `created_at` | TIMESTAMPTZ | Row creation time |

---

## Sequences

| Sequence | Format | Used by |
|----------|--------|---------|
| `offer_number_seq` | `{seq}{year}` e.g. "12026" | `offers.offer_number` |
| `invoice_number_seq` | `{seq}{year}` e.g. "12026" | Legacy — superseded by `invoice_number_counters` (migration `20260821100000`) for new allocations. Left in place, unused, rather than dropped. |

Current invoice numbering is per-calendar-year via the `invoice_number_counters` table
(see above), format `YYYY-NN`, restarting at `-01` every January.

---

## Dropped / Legacy

| Table | Status |
|-------|--------|
| `calendar_bookings` | Dropped in migration `20260307000000_drop_calendar_bookings.sql` — superseded by `inquiries.scheduled_date` + the calendar schedule endpoint |
| `quotes` | Renamed to `inquiries` in migration `20260301000000_inquiry_lifecycle.sql` |
