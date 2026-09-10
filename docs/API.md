# AUST Backend — API Reference

Base URL: `http://localhost:8080` (development) / `https://<production-host>` (production)

All JSON request bodies must include `Content-Type: application/json`.

## Authentication

Most admin endpoints require a JWT Bearer token obtained from `POST /api/v1/auth/login`.

Pass it as:
```
Authorization: Bearer <access_token>
```

Public endpoints (health, `GET /api/v1/estimates/images/*` and `/api/v1/media/images/*`, `POST /api/v1/submit/*`, `POST /api/v1/distance/calculate`, `POST /api/v1/flash-contact`) require no token.

`/api/v1/customer/*` and `/api/v1/employee/*` (besides their `/auth/*` sub-paths) use a separate session-token scheme, not the admin JWT — see [Customer Portal](#customer-portal) and [Employee Portal](#employee-portal).

**Roles**: the JWT carries a `role` claim (`admin` | `buerokraft` | `operator` — `operator` is a legacy alias for `buerokraft`). All `/api/v1/admin/*` and inquiry endpoints accept any valid admin JWT regardless of role; a smaller set of destructive/sensitive handlers additionally call a `require_admin()` check in-handler and reject non-`admin` roles with `403`. Endpoints below are marked **Bearer JWT (admin)** where that extra check applies; everywhere else **Bearer JWT** means any authenticated admin-side user.

---

## Health

### GET /health

Liveness check. Returns 200 if the process is running.

**Auth**: None

**Response** `200 OK`
```json
{ "status": "ok" }
```

**Example**
```bash
curl http://localhost:8080/health
```

---

### GET /ready

Readiness check. Returns 200 only when the database connection pool is healthy.

**Auth**: None

**Response** `200 OK`
```json
{ "status": "ready", "database": "ok" }
```

**Response** `503 Service Unavailable` — database unreachable.

**Example**
```bash
curl http://localhost:8080/ready
```

---

## Auth

### POST /api/v1/auth/login

Authenticate with email and password. Returns JWT access and refresh tokens.

**Auth**: None

**Request body**
```typescript
{
  email: string;    // e.g. "admin@example.com"
  password: string;
}
```

**Response** `200 OK`
```typescript
{
  access_token: string;   // JWT, valid for jwt_expiry_hours (default 24h)
  refresh_token: string;  // JWT, valid for 7 days
  token_type: "Bearer";
  expires_in: number;     // seconds
}
```

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Login successful |
| 400 | Missing email or password |
| 401 | Invalid credentials |

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/auth/login \
  -H "Content-Type: application/json" \
  -d '{"email":"admin@example.com","password":"secret123"}'
```

---

### POST /api/v1/auth/refresh

Exchange a refresh token for a new access token pair.

**Auth**: None

**Request body**
```typescript
{
  refresh_token: string;
}
```

**Response** `200 OK` — same shape as `/login`.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Tokens refreshed |
| 401 | Refresh token invalid or expired |

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/auth/refresh \
  -H "Content-Type: application/json" \
  -d '{"refresh_token":"<refresh_token>"}'
```

---

### POST /api/v1/auth/register

Create a new admin user. Requires an existing admin JWT.

**Auth**: Bearer JWT (admin)

**Request body**
```typescript
{
  email: string;                        // valid email address
  password: string;                     // minimum 8 characters
  name: string;                         // non-empty display name
  role?: "admin" | "operator";          // default: "operator"
}
```

**Response** `200 OK`
```typescript
{
  id: string;       // UUID v7
  email: string;
  name: string;
  role: "admin" | "operator";
}
```

**Status codes**
| Code | Meaning |
|---|---|
| 200 | User created |
| 400 | Validation error (invalid email, short password, etc.) |
| 401 | Not authenticated |
| 422 | Email already in use |

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/auth/register \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"email":"operator@example.com","password":"secure123","name":"Operator"}'
```

---

### POST /api/v1/auth/change-password

Change the current user's password.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  current_password: string;
  new_password: string;   // minimum 8 characters
}
```

**Response** `200 OK`
```json
{ "ok": true }
```

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Password changed |
| 400 | New password too short |
| 401 | Current password incorrect |

---

## Inquiries

An inquiry represents a single moving request from initial contact through to completion and payment. It replaces the former "quote" concept and unifies the entire lifecycle under one resource.

`InquiryStatus` values: `pending` | `info_requested` | `estimating` | `estimated` | `offer_ready` | `offer_sent` | `accepted` | `rejected` | `expired` | `cancelled` | `scheduled` | `completed` | `invoiced` | `paid`

Status transitions are validated server-side by `can_transition_to()`.

### InquiryResponse type

This is the canonical shape, built by `inquiry_builder::build_inquiry_response()` (`crates/api/src/services/inquiry_builder.rs`) from `aust_core::models::InquiryResponse` (`crates/core/src/models/snapshots.rs`). Every field marked optional below is omitted from the JSON entirely when `None`/empty (`skip_serializing_if`), not sent as `null`.

```typescript
{
  id: string;
  status: InquiryStatus;
  source: string;                 // e.g. "direct_email" | "admin_dashboard" | "photo_webapp" | "mobile_app" | "manual"
  services: Services;
  volume_m3?: number;
  distance_km?: number;
  scheduled_date?: string;        // "YYYY-MM-DD"
  end_date?: string;              // multi-day inquiries only
  is_multi_day: boolean;
  has_pauschale: boolean;         // Verpflegungspauschale enabled for multi-day trips
  start_time: string;             // "HH:MM:SS"
  end_time: string;               // "HH:MM:SS"
  notes?: string;
  employee_notes?: string;
  customer_message?: string;
  created_at: string;
  updated_at: string;
  offer_sent_at?: string;
  accepted_at?: string;
  service_type?: string;
  submission_mode?: string;
  recipient?: CustomerSnapshot;           // alternate contact for the offer/documents, if set
  billing_address?: AddressSnapshot;      // explicit override, if set
  effective_billing_address?: AddressSnapshot; // override, or else the customer's default billing address
  customer?: CustomerSnapshot;
  origin_address?: AddressSnapshot;
  destination_address?: AddressSnapshot;
  stop_address?: AddressSnapshot;
  estimation?: EstimationSnapshot;        // the latest estimation
  estimations: EstimationSnapshot[];      // all estimations (processing/failed/completed), newest first
  items: ItemSnapshot[];
  offer?: OfferSnapshot;                  // the active offer
  employees: EmployeeAssignmentSnapshot[];
  appointments: AppointmentSnapshot[];    // Zusatztermine linked to this inquiry
  custom_fields: object;                  // admin-only free-form overrides (offer_headline_override, etc.); omitted when empty
}
```

`InquiryStatus`: `pending` | `info_requested` | `estimating` | `estimated` | `offer_ready` | `offer_sent` | `accepted` | `rejected` | `expired` | `cancelled` | `scheduled` | `completed` | `invoiced` | `paid`. Transitions are validated server-side by `InquiryStatus::can_transition_to()`.

`Services`:
```typescript
{
  packing: boolean;
  assembly: boolean;
  disassembly: boolean;
  storage: boolean;
  disposal: boolean;
  parking_ban_origin: boolean;
  parking_ban_destination: boolean;
  transporter: boolean;
}
```

`CustomerSnapshot`:
```typescript
{
  id: string;
  name: string | null;
  salutation: string | null;
  first_name: string | null;
  last_name: string | null;
  email: string | null;
  phone: string | null;
  customer_type?: string;     // "private" | "business"
  company_name?: string;
}
```

`AddressSnapshot`:
```typescript
{
  id: string;
  street: string;
  house_number?: string;
  city: string;
  postal_code: string;
  country: string;
  floor?: string;
  elevator?: boolean;
  needs_parking_ban?: boolean;
  parking_ban: boolean;
  latitude?: number;
  longitude?: number;
}
```

`EstimationSnapshot`:
```typescript
{
  id: string;
  method: string;             // "vision" | "inventory" | "depth_sensor" | "video" | "manual" | "ar_device"
  status: string;             // "processing" | "completed" | "failed"
  total_volume_m3: number | null;
  confidence_score: number | null;
  item_count: number;
  source_images: string[];    // storage keys — build a URL with `/api/v1/estimates/images/{key}`
  source_video?: string;      // single storage key, not an array
  created_at: string;
}
```

`ItemSnapshot`:
```typescript
{
  name: string;
  volume_m3: number;
  quantity: number;
  confidence: number;
  category?: string;
  dimensions?: object;
  crop_url?: string;
  crop_s3_key?: string;
  source_image_url?: string;
  bbox?: number[];
  bbox_image_index?: number;
  seen_in_images?: number[];
  is_moveable: boolean;        // default true; omitted from JSON when true
  packs_into_boxes: boolean;   // default false; omitted from JSON when false
}
```

`OfferSnapshot`:
```typescript
{
  id: string;
  offer_number: string | null;
  status: string;             // raw offers.status — "draft" | "sent" | "viewed" | "accepted" | "rejected" | "expired"
  persons: number;
  hours: number;
  rate_cents: number;         // per person per hour, in cents
  total_netto_cents: number;
  total_brutto_cents: number;
  line_items: LineItemSnapshot[];
  pdf_url?: string;
  valid_until?: string;
  created_at: string;
}
```

`LineItemSnapshot`:
```typescript
{
  label: string;
  remark: string | null;
  quantity: number;
  unit_price_cents: number;
  total_cents: number;
  is_labor: boolean;
  is_flat_total: boolean;
}
```

`EmployeeAssignmentSnapshot`:
```typescript
{
  employee_id: string;
  first_name: string;
  last_name: string;
  clock_in: string | null;             // admin-set, "HH:MM:SS"
  clock_out: string | null;
  start_time: string | null;           // planned
  end_time: string | null;
  break_minutes: number;
  actual_hours: number | null;         // derived from clock_in/clock_out minus break, or manual override
  employee_clock_in: string | null;    // self-reported via worker portal, RFC3339
  employee_clock_out: string | null;
  employee_actual_hours: number | null;
  notes: string | null;
  job_date?: string;                   // per-employee, per-day assignment date (multi-day inquiries)
  transport_mode?: string;
  travel_costs_cents?: number;
  accommodation_cents?: number;
  misc_costs_cents?: number;
  meal_deduction?: string;
}
```

`AppointmentSnapshot` (a Zusatztermin — see [Inquiry Appointments](#inquiry-appointments)):
```typescript
{
  id: string;
  kind: string;                 // e.g. "besichtigung", "halteverbot" — free text
  scheduled_date: string;
  start_time?: string;
  end_time?: string;
  assignee_id?: string;         // single assignee, for lightweight (unpaid) entries
  assignee_name?: string;
  location?: string;
  description?: string;
  address?: AddressSnapshot;    // structured own-address; falls back to `location`
  notes?: string;
  employee_notes?: string;
  employees: EmployeeAssignmentSnapshot[]; // paid crew, empty for lightweight entries
  status: string;                // "scheduled" | "done" | "cancelled"
  created_at: string;
}
```

---

### POST /api/v1/inquiries

Create a new inquiry. Automatically creates or upserts the customer (by email) and origin/destination addresses.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  customer_email: string;                 // required — used to upsert customer
  customer_name?: string;
  customer_phone?: string;
  origin_address?: string;                // free-text address string
  origin_floor?: string;
  origin_elevator?: boolean;
  destination_address?: string;           // free-text address string
  destination_floor?: string;
  destination_elevator?: boolean;
  services?: Services;                    // defaults to all false
  notes?: string;
  scheduled_date?: string;                // ISO 8601, e.g. "2026-03-15T09:00:00Z"
}
```

**Response** `201 Created` — `InquiryResponse` object.

**Status codes**
| Code | Meaning |
|---|---|
| 201 | Inquiry created |
| 400 | Missing customer_email or invalid input |
| 422 | Validation error |

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/inquiries \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{
    "customer_email": "max@example.com",
    "customer_name": "Max Mustermann",
    "customer_phone": "+43 660 1234567",
    "origin_address": "Musterstr. 1, 1010 Wien",
    "origin_floor": "3. OG",
    "origin_elevator": false,
    "destination_address": "Neugasse 5, 1020 Wien",
    "destination_floor": "EG",
    "destination_elevator": true,
    "services": { "packing": true, "disassembly": true, "assembly": true, "parking_ban_origin": true },
    "scheduled_date": "2026-04-01T08:00:00Z",
    "notes": "Sehr schweres Klavier im Wohnzimmer"
  }'
```

---

### GET /api/v1/inquiries

List inquiries with optional filters and pagination.

**Auth**: Bearer JWT

**Query parameters**
| Parameter | Type | Description |
|---|---|---|
| `status` | string | Filter by `InquiryStatus` value |
| `search` | string | Substring search on customer name, email, addresses, notes |
| `has_offer` | boolean | `true` = only inquiries with an active offer; `false` = only without |
| `limit` | integer | Max results (default 50, max 100) |
| `offset` | integer | Pagination offset (default 0) |

**Response** `200 OK`
```typescript
{
  inquiries: InquiryListItem[];
  total: number;
  limit: number;
  offset: number;
}
```

`InquiryListItem` is a summary projection of the full `InquiryResponse` (same top-level fields, with customer/address/offer snapshots included for display).

**Example**
```bash
curl "http://localhost:8080/api/v1/inquiries?status=pending&has_offer=false&limit=20" \
  -H "Authorization: Bearer <token>"
```

---

### GET /api/v1/inquiries/{id}

Get the full detail for a single inquiry including embedded customer, addresses, estimation, items, and active offer.

**Auth**: Bearer JWT

**Response** `200 OK` — `InquiryResponse` object.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Found |
| 404 | Inquiry not found |

**Example**
```bash
curl http://localhost:8080/api/v1/inquiries/019500000000000000000000 \
  -H "Authorization: Bearer <token>"
```

---

### PATCH /api/v1/inquiries/{id}

Partially update an inquiry. All fields are optional; only provided fields are updated. Status transitions are validated by `InquiryStatus::can_transition_to()`.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  status?: InquiryStatus;
  notes?: string;
  services?: Services;
  estimated_volume_m3?: number;
  distance_km?: number;
  scheduled_date?: string;
  origin_address_id?: string;
  destination_address_id?: string;
}
```

**Response** `200 OK` — updated `InquiryResponse` object.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Updated |
| 400 | Invalid status transition |
| 404 | Inquiry not found |

**Example**
```bash
curl -X PATCH http://localhost:8080/api/v1/inquiries/019500000000000000000000 \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"estimated_volume_m3": 18.5, "status": "estimated"}'
```

---

### DELETE /api/v1/inquiries/{id}

Soft-delete an inquiry (sets status to `cancelled`).

**Auth**: Bearer JWT

**Response** `200 OK` — updated `InquiryResponse` object with `status: "cancelled"`.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Cancelled |
| 404 | Inquiry not found |

**Example**
```bash
curl -X DELETE http://localhost:8080/api/v1/inquiries/019500000000000000000000 \
  -H "Authorization: Bearer <token>"
```

---

### GET /api/v1/inquiries/{id}/pdf

Download the latest active offer PDF for this inquiry.

**Auth**: Bearer JWT

**Response** `200 OK` with `Content-Type: application/pdf` and `Content-Disposition: attachment; filename="Angebot_<number>.pdf"`.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | PDF returned |
| 404 | Inquiry not found or no active offer with a generated PDF |

**Example**
```bash
curl http://localhost:8080/api/v1/inquiries/019500000000000000000000/pdf \
  -H "Authorization: Bearer <token>" \
  -o Angebot.pdf
```

---

### PUT /api/v1/inquiries/{id}/items

Replace the detected items list on the latest volume estimation for this inquiry and recalculate the total volume. Used by the admin UI to correct ML detection results.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  items: {
    name: string;
    volume_m3: number;
    quantity: number;
    confidence: number;
    crop_s3_key?: string;
    bbox?: number[];
    bbox_image_index?: number;
    seen_in_images?: number[];
    category?: string;
    dimensions?: object;
  }[];
}
```

**Response** `200 OK`
```typescript
{
  id: string;
  method: string;
  total_volume_m3: number;   // sum of item.volume_m3 * item.quantity
  items: ItemSnapshot[];
  source_images: string[];
  source_videos: string[];
}
```

**Business rules**
- Updates both the estimation's items and the inquiry's `volume_m3`.
- Fails with 404 if no estimation exists for this inquiry.

**Example**
```bash
curl -X PUT http://localhost:8080/api/v1/inquiries/019500000000000000000000/items \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{
    "items": [
      {"name": "Sofa", "volume_m3": 0.8, "quantity": 1, "confidence": 0.95},
      {"name": "Schreibtisch", "volume_m3": 0.5, "quantity": 1, "confidence": 0.88}
    ]
  }'
```

---

### POST /api/v1/inquiries/{id}/estimate/{method}

Trigger a volume estimation for this inquiry using the specified method.

**Auth**: Bearer JWT

**Path parameters**
| Parameter | Values | Description |
|---|---|---|
| `id` | UUID | Inquiry ID |
| `method` | `depth` or `video` | Estimation method to use |

For `depth`, images are uploaded as `multipart/form-data`:

**Request** (`multipart/form-data`)
| Field | Type | Description |
|---|---|---|
| `<any name>` | file | Image files (JPEG, PNG); one field per image |

For `video`, a single video is uploaded as `multipart/form-data`:

**Request** (`multipart/form-data`)
| Field | Type | Description |
|---|---|---|
| `video` | file | Video file (MP4, MOV, WebM, MKV) |
| `max_keyframes` | text (optional) | Override number of keyframes to extract |
| `detection_threshold` | text (optional) | Override detection confidence threshold |

**Response** `200 OK` — estimation result. For `video`, the estimation has `status: "processing"` and completes asynchronously.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Estimation triggered/completed |
| 404 | Inquiry not found |
| 422 | No files provided or invalid method |
| 500 | Vision service unavailable |

**Example**
```bash
# Depth estimation with photos
curl -X POST http://localhost:8080/api/v1/inquiries/019500000000000000000000/estimate/depth \
  -H "Authorization: Bearer <token>" \
  -F "image1=@living_room.jpg" \
  -F "image2=@bedroom.jpg"

# Video estimation
curl -X POST http://localhost:8080/api/v1/inquiries/019500000000000000000000/estimate/video \
  -H "Authorization: Bearer <token>" \
  -F "video=@walkthrough.mp4"
```

---

### POST /api/v1/inquiries/{id}/generate-offer

Generate or regenerate an offer for this inquiry. Runs the pricing engine, fills the XLSX template, converts to PDF via LibreOffice, and stores the result. Upserts into the existing active offer if one exists.

**Auth**: Bearer JWT

**Request body** (`application/json`, optional — all fields are overrides)
```typescript
{
  valid_days?: number;            // offer validity in days (default: 30)
  price_cents_netto?: number;     // override computed netto price
  persons?: number;               // override number of movers
  hours?: number;                 // override estimated hours
  rate?: number;                  // override hourly rate in euros (e.g. 35.0)
  line_items?: {                  // override line items entirely
    description: string;
    quantity: number;
    unit_price: number;
    remark?: string;
  }[];
}
```

**Response** `200 OK` — `OfferSnapshot` object.

**Business rules**
- The inquiry must have `volume_m3` set or the request fails with 400.
- Default pricing: computed from volume, distance, floor levels, and date (Saturday surcharge).
- When `price_cents_netto` is provided together with existing `persons` and `hours`, the `rate` is back-calculated as `(netto - non_labor_items) / (persons * hours)`.
- LibreOffice must be installed and `soffice` available on PATH.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Offer generated |
| 400 | Inquiry has no volume estimate |
| 404 | Inquiry or customer not found |
| 500 | PDF generation failed (LibreOffice error) |

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/inquiries/019500000000000000000000/generate-offer \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"valid_days": 14, "persons": 3, "hours": 6}'
```

---

### GET /api/v1/inquiries/{id}/emails

Get the email thread associated with this inquiry.

**Auth**: Bearer JWT

**Response** `200 OK`
```typescript
{
  thread: {
    id: string;
    subject: string;
    messages: {
      id: string;
      from: string;
      to: string;
      subject: string;
      body: string;
      direction: "inbound" | "outbound";
      status: string;
      created_at: string;
    }[];
  } | null;
}
```

**Status codes**
| Code | Meaning |
|---|---|
| 200 | OK (thread may be null if no emails exist) |
| 404 | Inquiry not found |

**Example**
```bash
curl http://localhost:8080/api/v1/inquiries/019500000000000000000000/emails \
  -H "Authorization: Bearer <token>"
```

---

### GET /api/v1/inquiries/{id}/employees/{emp_id}/travel-expenses

Generate the travel-expenses (Reisekosten) document for one employee's assignment on this inquiry.

**Auth**: Bearer JWT

**Response** `200 OK` — generated document (XLSX/PDF, depending on implementation).

---

## Inquiry Appointments

Lightweight appointments linked to an inquiry on their own (possibly non-consecutive) date — e.g. a Besichtigung before the move, or a paid Zusatztermin like a Halteverbotszone booking. Distinct from the move's own contiguous day range. Mounted at `/api/v1/inquiries/{id}/appointments`. See `AppointmentSnapshot` above for the shape returned by each endpoint.

### GET /api/v1/inquiries/{id}/appointments

List an inquiry's appointments.

**Auth**: Bearer JWT

**Response** `200 OK` — array of `AppointmentSnapshot`.

---

### POST /api/v1/inquiries/{id}/appointments

Create an appointment.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  kind?: string;                // free text, e.g. "besichtigung"; default applied server-side
  scheduled_date: string;       // required, "YYYY-MM-DD"
  start_time?: string;          // "HH:MM" or "HH:MM:SS"
  end_time?: string;
  assignee_id?: string;         // must be an active employee
  location?: string;
  description?: string;
  address_id?: string;
  notes?: string;
  employee_notes?: string;
  status?: string;              // "scheduled" | "done" | "cancelled", default "scheduled"
}
```

**Response** `201 Created` — `AppointmentSnapshot`.

**Status codes**
| Code | Meaning |
|---|---|
| 201 | Created |
| 400 | Invalid status value |
| 404 | Inquiry not found, or `assignee_id` is not an active employee |

---

### PATCH /api/v1/inquiries/{id}/appointments/{appt_id}

Partially update an appointment. Uses raw JSON so a field can be explicitly cleared (`null`) versus left unchanged (omitted) — the same fields as the create body, all optional.

**Auth**: Bearer JWT

**Response** `200 OK` — updated `AppointmentSnapshot`.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Updated |
| 400 | Invalid status, date, time, or UUID field |
| 404 | Appointment not found |

---

### DELETE /api/v1/inquiries/{id}/appointments/{appt_id}

Remove an appointment.

**Auth**: Bearer JWT

**Response** `204 No Content`

---

### GET /api/v1/inquiries/{id}/appointments/{appt_id}/employees

List the appointment's paid crew.

**Auth**: Bearer JWT

**Response** `200 OK` — array of `EmployeeAssignmentSnapshot`.

---

### POST /api/v1/inquiries/{id}/appointments/{appt_id}/employees

Assign an employee to the appointment.

**Auth**: Bearer JWT

**Request body**: `{ "employee_id": "<uuid>" }`

**Response** `201 Created` — full `AppointmentSnapshot` (with updated `employees`).

---

### PUT /api/v1/inquiries/{id}/appointments/{appt_id}/employees

Full-replace the appointment's crew list in one call.

**Auth**: Bearer JWT

**Request body**: array of
```typescript
{
  employee_id: string;
  notes?: string;
  start_time?: string;      // lenient — accepts "7:30", "07:30", "7.30"
  end_time?: string;
  clock_in?: string;
  clock_out?: string;
  break_minutes?: number;
  actual_hours?: number;
  transport_mode?: string;
  travel_costs_cents?: number;
  accommodation_cents?: number;
  misc_costs_cents?: number;
  meal_deduction?: string;
}[]
```

**Response** `200 OK` — updated `AppointmentSnapshot`.

---

### PATCH /api/v1/inquiries/{id}/appointments/{appt_id}/employees/{emp_id}

Update one crew member's hours/notes/expenses on this appointment. Body fields are the same as one entry of the `PUT` array above (all optional). Time fields use the lenient parser.

**Auth**: Bearer JWT

**Response** `200 OK` — updated `AppointmentSnapshot`.

**Errors**: `404` if the employee is not assigned yet — assign first, then update.

---

### DELETE /api/v1/inquiries/{id}/appointments/{appt_id}/employees/{emp_id}

Unassign an employee from the appointment.

**Auth**: Bearer JWT

**Response** `204 No Content`

---

## Inquiry Invoices

Invoice (Rechnung) routes, mounted under `/api/v1/inquiries/{id}/invoices`. Two modes: **full** (single invoice for the whole job) and **partial** (an Anzahlung + Restbetrag pair sharing a `partial_group_id`). Money fields are netto cents unless named `*_brutto_cents`.

### GET /api/v1/inquiries/{id}/invoices

List all invoices for an inquiry.

**Auth**: Bearer JWT

**Response** `200 OK` — array of `InvoiceResponse`:
```typescript
{
  id: string;
  inquiry_id: string;
  invoice_number: string;
  invoice_type: string;              // "full" | "partial_first" | "partial_final"
  partial_group_id: string | null;
  partial_percent: number | null;
  status: string;                    // "draft" | "ready" | "sent" | "paid"
  extra_services: { description: string; price_cents: number }[];
  is_manual: boolean;
  line_items: { description: string; quantity: number; unit_price_cents: number; remark?: string }[]; // manual invoices only
  total_netto_cents: number;
  total_brutto_cents: number;
  pdf_s3_key: string | null;
  sent_at: string | null;
  paid_at: string | null;
  created_at: string;
}
```

---

### POST /api/v1/inquiries/{id}/invoices

Create a new invoice, or an Anzahlung/Restbetrag pair. Generates the XLSX, converts to PDF, and uploads to S3. Idempotent: calling again when invoices already exist for the inquiry returns the existing ones (self-healing any missing PDF) instead of erroring.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  invoice_type: "full" | "partial";
  partial_percent?: number;          // required for "partial", 1-99
  price_cents_netto?: number;        // manual override, used only when no active offer exists
}
```

**Response** `200 OK` — array of `InvoiceResponse` (one for `full`, two for `partial`).

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Created (or existing invoices returned) |
| 400 | Inquiry status is before `accepted`, `partial_percent` missing/out of range, or offer price is 0 |
| 404 | Inquiry not found |

---

### GET /api/v1/inquiries/{id}/invoices/{inv_id}

Get a single invoice.

**Auth**: Bearer JWT

**Response** `200 OK` — `InvoiceResponse`.

---

### PATCH /api/v1/inquiries/{id}/invoices/{inv_id}

Update an invoice: mark paid, replace extra services, or switch into/out of manual line-item mode.

**Auth**: Bearer JWT

**Request body** (all optional)
```typescript
{
  status?: string;                                   // "paid" sets paid_at
  extra_services?: { description: string; price_cents: number }[]; // full / partial_final only
  line_items?: {                                      // presence switches the invoice to manual mode
    description: string;
    quantity: number;
    unit_price_cents: number;      // netto
    remark?: string;
  }[];                                                 // max 20 items
  is_manual?: false;                                  // with no line_items, reverts to offer-derived
}
```

**Response** `200 OK` — updated `InvoiceResponse`.

---

### GET /api/v1/inquiries/{id}/invoices/{inv_id}/pdf

Download the invoice PDF.

**Auth**: Bearer JWT

**Response** `200 OK` with `Content-Type: application/pdf`.

---

### POST /api/v1/inquiries/{id}/invoices/{inv_id}/send

Send the invoice by email to the customer.

**Auth**: Bearer JWT

**Request body** (optional — both fields fall back to a standard template)
```typescript
{ subject?: string; body?: string; }
```

**Response** `200 OK`.

---

### PATCH /api/v1/inquiries/{id}/invoices/{inv_id}/number

Overwrite the invoice number and regenerate its PDF. Recovery path for when the in-system counter falls out of sync with an invoice number Alex already sent manually — intentionally mutable (see the handler's doc comment on GoBD/§146 AO scope).

**Auth**: Bearer JWT

**Request body**: `{ "invoice_number": "2026-53" }`

**Response** `200 OK` — updated `InvoiceResponse`.

---

## Public Submissions

These endpoints accept multipart form data from public-facing applications (photo webapp, mobile app). They do not require authentication. Each submission creates a new inquiry, customer, and triggers the estimation pipeline automatically.

### POST /api/v1/submit/photo

Upload photos from the photo webapp for volume estimation. Creates an inquiry with `source: "photo_webapp"`.

**Auth**: None (public route)

**Request** (`multipart/form-data`)
| Field | Type | Required | Description |
|---|---|---|---|
| `email` | text | Yes | Customer email address |
| `name` | text | No | Customer name |
| `phone` | text | No | Customer phone |
| `origin_address` | text | No | Origin address (free text) |
| `destination_address` | text | No | Destination address (free text) |
| `scheduled_date` | text | No | ISO 8601 date |
| `notes` | text | No | Additional notes |
| `<any name>` | file | Yes | One or more image files (JPEG, PNG) |

**Response** `200 OK`
```typescript
{
  id: string;          // inquiry ID
  status: string;      // "estimating"
  message: string;     // confirmation message
}
```

**Business rules**
- At least one image file must be included.
- Customer is upserted by email.
- Volume estimation runs asynchronously after the response is returned.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Submission accepted |
| 400 | Missing email or no images |
| 422 | Invalid input |

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/submit/photo \
  -F "email=kunde@example.com" \
  -F "name=Max Mustermann" \
  -F "origin_address=Musterstr. 1, 1010 Wien" \
  -F "destination_address=Neugasse 5, 1020 Wien" \
  -F "image1=@living_room.jpg" \
  -F "image2=@bedroom.jpg"
```

---

### POST /api/v1/submit/mobile

Upload photos and optional depth maps from the mobile app. Creates an inquiry with `source: "mobile_app"`.

**Auth**: None (public route)

**Request** (`multipart/form-data`)
| Field | Type | Required | Description |
|---|---|---|---|
| `email` | text | Yes | Customer email address |
| `name` | text | No | Customer name |
| `phone` | text | No | Customer phone |
| `origin_address` | text | No | Origin address (free text) |
| `destination_address` | text | No | Destination address (free text) |
| `scheduled_date` | text | No | ISO 8601 date |
| `notes` | text | No | Additional notes |
| `<any name>` | file | Yes | Image files and/or depth map files |

**Response** `200 OK`
```typescript
{
  id: string;          // inquiry ID
  status: string;      // "estimating"
  message: string;     // confirmation message
}
```

**Business rules**
- At least one image file must be included.
- Depth maps (if provided) trigger the 3D ML pipeline; otherwise falls back to LLM vision.
- Customer is upserted by email.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Submission accepted |
| 400 | Missing email or no images |
| 422 | Invalid input |

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/submit/mobile \
  -F "email=kunde@example.com" \
  -F "name=Max Mustermann" \
  -F "phone=+43 660 1234567" \
  -F "origin_address=Musterstr. 1, 1010 Wien" \
  -F "destination_address=Neugasse 5, 1020 Wien" \
  -F "image1=@room1.jpg" \
  -F "depth1=@room1_depth.png" \
  -F "image2=@room2.jpg"
```

---

### POST /api/v1/submit/mobile/ar

Upload AR per-item capture data from the mobile app. Creates an inquiry with `source: "mobile_app"` and triggers the AR reconstruction pipeline on the vision service.

**Auth**: None (public route)

**Request** (`multipart/form-data`)
| Field | Type | Required | Description |
|---|---|---|---|
| `email` | text | Yes | Customer email address |
| `name` | text | No | Customer name |
| `phone` | text | No | Customer phone |
| `departure_address` | text | No | Origin address |
| `arrival_address` | text | No | Destination address |
| `services` | text | No | Comma-separated service codes (e.g. `assembly,disassembly`) |
| `scheduled_date` | text | No | ISO 8601 date |
| `message` | text | No | Additional notes |
| `item_manifest` | text | Yes | JSON array — `[{label: string, frame_count: number}, ...]` |
| `intrinsics` | text | No | JSON object — `{fx, fy, cx, cy, width, height}` from ARKit |
| `poses` | text | No | JSON array — `[[float×16], ...]` column-major 4×4 per frame |
| `images` | file(s) | Yes | RGB JPEG frames (all items combined, in order) |
| `depth_maps` | file(s) | No | 16-bit depth PNGs from LiDAR (one per image where available) |

**Response** `200 OK`
```typescript
{
  id: string;          // inquiry ID
  status: string;      // "estimating"
  message: string;     // confirmation message
}
```

**Business rules**
- Customer is upserted by email.
- Estimation method is set to `"depth_sensor"`.
- Images are stored in S3 under `estimates/{inquiry_id}/{est_id}/ar/{idx}.jpg`; depth maps under `ar/depth/{idx}.png`.
- AR metadata (`item_manifest`, `poses`, `intrinsics`) is stored in `source_data` JSONB on the estimation row.
- Background task calls `POST /estimate/ar/submit` on the Modal vision service, then polls until complete.
- On vision result: updates estimation volume, triggers auto offer generation + Telegram notification.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Submission accepted, background processing started |
| 400 | Missing email or no images |
| 422 | Invalid input |

---

## Volume Estimation

### GET /api/v1/estimates/images/{key}

Serve an image or video from storage. Used as `<img src>` or `<video src>` in the admin UI. Does not require authentication.

**Auth**: None (public route)

**Path parameter**: `key` is the full storage key returned in `source_images` / `source_videos` arrays.

**Response**: Raw binary with appropriate `Content-Type` header.

---

### POST /api/v1/estimates/vision

Analyze one or more room photos using the LLM vision model. Images are submitted as base64-encoded JSON. Stores results and updates the inquiry's volume. Triggers auto offer generation in the background.

**Auth**: Bearer JWT

**Request body** (`application/json`)
```typescript
{
  inquiry_id: string;   // UUID (inquiry ID)
  images: {
    data: string;       // base64-encoded image bytes
    mime_type: string;  // e.g. "image/jpeg", "image/png"
  }[];
}
```

**Response** `200 OK`
```typescript
{
  id: string;
  inquiry_id: string;
  method: "vision";
  status: "completed";
  source_data: {
    image_count: number;
    s3_keys: string[];
  };
  result_data: VisionAnalysisResult[];   // one per image
  total_volume_m3: number;
  confidence_score: number;
  created_at: string;
}
```

`VisionAnalysisResult` per image:
```typescript
{
  detected_items: DetectedItem[];
  total_volume_m3: number;
  confidence_score: number;
  room_type: string | null;
  analysis_notes: string | null;
}
```

`DetectedItem`:
```typescript
{
  name: string;
  volume_m3: number;
  confidence: number;
  dimensions: { length_m: number; width_m: number; height_m: number } | null;
  category: string | null;
  german_name: string | null;
  re_value: number | null;       // Raumeinheit value (1 RE = 0.1 m³)
  bbox: number[] | null;         // [x1, y1, x2, y2] normalized
  bbox_image_index: number | null;
  crop_s3_key: string | null;
}
```

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Analysis complete |
| 422 | No images provided or invalid base64 |

**Example**
```bash
IMAGE_B64=$(base64 -w 0 room.jpg)
curl -X POST http://localhost:8080/api/v1/estimates/vision \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d "{\"inquiry_id\":\"019500000000000000000000\",\"images\":[{\"data\":\"$IMAGE_B64\",\"mime_type\":\"image/jpeg\"}]}"
```

---

### POST /api/v1/estimates/depth-sensor

Upload photos for 3D ML volume estimation (depth-sensor / photogrammetry pipeline). Uses the Modal vision service when available; falls back to LLM vision analysis automatically. Accepted as a `multipart/form-data` upload.

**Auth**: Bearer JWT

**Request** (`multipart/form-data`)
| Field | Type | Description |
|---|---|---|
| `inquiry_id` | text | UUID of the inquiry |
| `<any name>` | file | Image files (JPEG, PNG, etc.); one field per image |

**Response** `200 OK` — `VolumeEstimation` object (same shape as vision estimate).

**Business rules**
- Images are stored in S3 before analysis.
- If the Modal vision service fails, the system automatically retries with the LLM.
- On completion, the inquiry's `volume_m3` is updated and an offer is generated in the background.

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/estimates/depth-sensor \
  -H "Authorization: Bearer <token>" \
  -F "inquiry_id=019500000000000000000000" \
  -F "image1=@living_room.jpg" \
  -F "image2=@bedroom.jpg"
```

---

### POST /api/v1/estimates/video

Upload a video for 3D reconstruction using MASt3R + SAM 2 on Modal (serverless GPU, L4). The video is stored in S3 immediately and processing continues in the background — the response is returned before processing finishes.

**Auth**: Bearer JWT

**Request** (`multipart/form-data`)
| Field | Type | Description |
|---|---|---|
| `inquiry_id` | text | UUID of the inquiry |
| `video` | file | Video file (MP4, MOV, WebM, MKV) — one per request |
| `max_keyframes` | text (optional) | Override number of keyframes to extract |
| `detection_threshold` | text (optional) | Override object detection confidence threshold |

**Response** `200 OK` — array of `VolumeEstimation` objects (one per video).

The returned estimation has `status: "processing"`. Poll `GET /api/v1/estimates/{id}` to check for completion.

**Business rules**
- Requires the Modal vision service to be configured (`AUST__VISION_SERVICE__ENABLED=true`). Returns 500 if not configured.
- When all videos for an inquiry finish processing, volumes are summed and offer generation is triggered.
- Default timeout: 600 seconds.

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/estimates/video \
  -H "Authorization: Bearer <token>" \
  -F "inquiry_id=019500000000000000000000" \
  -F "video=@walkthrough.mp4"
```

---

### POST /api/v1/estimates/inventory

Submit a manual inventory list. Calculates total volume by summing item volumes and quantities.

**Auth**: Bearer JWT

**Request body** (`application/json`)
```typescript
{
  inquiry_id: string;
  inventory: {
    items: {
      name: string;
      quantity: number;
      volume_m3: number;
      category?: string;
    }[];
    additional_notes?: string;
  };
}
```

**Response** `200 OK` — `VolumeEstimation` object with `method: "inventory"` and `status: "completed"`.

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/estimates/inventory \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{
    "inquiry_id": "019500000000000000000000",
    "inventory": {
      "items": [
        {"name": "Sofa", "quantity": 1, "volume_m3": 0.8, "category": "Seating"},
        {"name": "Schreibtisch", "quantity": 1, "volume_m3": 0.5}
      ],
      "additional_notes": "Sehr schweres Klavier im Wohnzimmer"
    }
  }'
```

---

### GET /api/v1/estimates/{id}

Retrieve a single volume estimation record by its ID.

**Auth**: Bearer JWT

**Response** `200 OK` — `VolumeEstimation` object.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Found |
| 404 | Estimation not found |

**Example**
```bash
curl http://localhost:8080/api/v1/estimates/019500000000000000000001 \
  -H "Authorization: Bearer <token>"
```

---

### DELETE /api/v1/estimates/{id}

Delete a volume estimation record and clean up its associated S3 objects (images, videos, crop thumbnails).

**Auth**: Bearer JWT

**Response** `204 No Content`

**Status codes**
| Code | Meaning |
|---|---|
| 204 | Deleted |
| 404 | Estimation not found |

---

## Calendar

### GET /api/v1/calendar/availability

Check whether a specific date is available for booking and get alternatives if it is full.

**Auth**: Bearer JWT

**Query parameters**
| Parameter | Type | Required | Description |
|---|---|---|---|
| `date` | `YYYY-MM-DD` | Yes | Date to check |

**Response** `200 OK`
```typescript
{
  requested_date: string;
  requested_date_available: boolean;
  requested_date_info: {
    date: string;
    available: boolean;
    capacity: number;
    booked: number;
    remaining: number;
  };
  alternatives: DateAvailability[];   // populated only when requested date is unavailable
}
```

**Example**
```bash
curl "http://localhost:8080/api/v1/calendar/availability?date=2026-04-01" \
  -H "Authorization: Bearer <token>"
```

---

### GET /api/v1/calendar/schedule

Get a day-by-day schedule showing availability and bookings for a date range. Maximum range: 90 days.

**Auth**: Bearer JWT

**Query parameters**
| Parameter | Type | Required | Description |
|---|---|---|---|
| `from` | `YYYY-MM-DD` | Yes | Start date (inclusive) |
| `to` | `YYYY-MM-DD` | Yes | End date (inclusive) |

**Response** `200 OK` — array of schedule entries:
```typescript
{
  date: string;
  availability: {
    date: string;
    available: boolean;
    capacity: number;
    booked: number;
    remaining: number;
  };
  bookings: {
    id: string;
    booking_date: string;
    inquiry_id: string | null;
    customer_name: string | null;
    customer_email: string | null;
    departure_address: string | null;
    arrival_address: string | null;
    volume_m3: number | null;
    distance_km: number | null;
    description: string | null;
    status: string;
    created_at: string;
    updated_at: string;
    offer_price_cents: number | null;   // from linked offer (enriched field)
  }[];
}[]
```

**Status codes**
| Code | Meaning |
|---|---|
| 200 | OK |
| 400 | `from` is after `to`, or range exceeds 90 days |

**Example**
```bash
curl "http://localhost:8080/api/v1/calendar/schedule?from=2026-04-01&to=2026-04-30" \
  -H "Authorization: Bearer <token>"
```

There is no separate booking-creation endpoint or `bookings` CRUD resource — a "booking" in the schedule response above is a row derived from `inquiries` (and, historically, `calendar_items`); creating or updating one means creating/updating the inquiry (see [Inquiries](#inquiries)) or a calendar item (see [Calendar Items](#calendar-items)), not posting to `/calendar`.

---

### PUT /api/v1/calendar/capacity/{date}

Override the daily booking capacity for a specific date. Use `0` to block a date entirely.

**Auth**: Bearer JWT

**Path parameter**: `date` in `YYYY-MM-DD` format.

**Request body**
```typescript
{
  capacity: number;   // integer >= 0
}
```

**Response** `200 OK`
```typescript
{
  id: string;
  override_date: string;
  capacity: number;
  created_at: string;
}
```

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Capacity set |
| 400 | `capacity` is negative |

**Example**
```bash
# Block Easter Monday
curl -X PUT http://localhost:8080/api/v1/calendar/capacity/2026-04-06 \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"capacity":0}'
```

---

## Distance

### POST /api/v1/distance/calculate

Calculate driving distance and duration for a multi-stop route. Uses OpenRouteService (OpenStreetMap-based) for geocoding and routing.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  addresses: string[];   // minimum 2; ordered list of free-text addresses
}
```

**Response** `200 OK`
```typescript
{
  addresses: string[];
  legs: {
    from_address: string;
    to_address: string;
    from_location: { latitude: number; longitude: number };
    to_location: { latitude: number; longitude: number };
    distance_km: number;
    duration_minutes: number;
    geometry: [number, number][];   // GeoJSON LineString [[lng, lat], ...]
  }[];
  total_distance_km: number;
  total_duration_minutes: number;
  price_cents: number;           // ceil(total_distance_km) * 100 (€1.00/km)
  price_per_km_cents: number;    // 100
}
```

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Route calculated |
| 422 | Fewer than 2 addresses provided |
| 500 | Geocoding or routing failed (address not found, ORS error) |

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/distance/calculate \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{
    "addresses": [
      "Musterstr. 1, 1010 Wien",
      "Neugasse 5, 1020 Wien"
    ]
  }'
```

---

## Admin

All `/api/v1/admin/` endpoints require a Bearer JWT. These routes support the admin dashboard application.

### GET /api/v1/admin/dashboard

Returns aggregate counts and recent activity for the dashboard overview.

**Auth**: Bearer JWT (admin)

**Response** `200 OK`
```typescript
{
  open_quotes: number;        // status in (pending, info_requested, estimating, estimated)
  pending_offers: number;     // status = offer_ready (not yet sent)
  todays_bookings: number;
  total_customers: number;
  recent_activity: {
    type: string;             // e.g. "offer_draft", "offer_sent"
    description: string;
    created_at: string;
    id: string | null;        // target resource UUID (inquiry, email thread, or calendar item)
    status: string | null;
  }[];
  conflict_dates: {
    date: string;              // dates in the next 30 days where bookings >= capacity
    booked: number;
    capacity: number;
  }[];
  pending_review_count: number; // overdue "Später"-deferred review requests, see GET /admin/review-reminders
}
```

**Example**
```bash
curl http://localhost:8080/api/v1/admin/dashboard \
  -H "Authorization: Bearer <token>"
```

---

### GET /api/v1/admin/customers

List customers with optional search and pagination.

**Auth**: Bearer JWT

**Query parameters**
| Parameter | Type | Description |
|---|---|---|
| `search` | string | Substring search on name and email |
| `limit` | integer | Max results (default 50, max 100) |
| `offset` | integer | Pagination offset |

**Response** `200 OK`
```typescript
{
  customers: {
    id: string;
    email: string | null;
    name: string | null;
    salutation: string | null;
    first_name: string | null;
    last_name: string | null;
    phone: string | null;
    customer_type: string | null;    // "private" | "business"
    company_name: string | null;
    created_at: string;
  }[];
  total: number;
}
```

---

### POST /api/v1/admin/customers

Create a new customer record. Email is optional — older customers without one can still be registered.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  email?: string;
  name?: string;
  salutation?: string;
  first_name?: string;
  last_name?: string;
  phone?: string;
  customer_type?: string;    // "private" | "business"
  company_name?: string;
}
```

**Response** `201 Created` — one customer item (same shape as the list row above).

**Status codes**
| Code | Meaning |
|---|---|
| 201 | Created |
| 400 | Email already in use by another customer |

---

### GET /api/v1/admin/customers/{id}

Get a single customer with their complete history: quotes (legacy field name for inquiries), offers, Termine, and address book.

**Auth**: Bearer JWT

**Response** `200 OK`
```typescript
{
  id: string;
  email: string | null;
  name: string | null;
  salutation: string | null;
  first_name: string | null;
  last_name: string | null;
  phone: string | null;
  customer_type: string | null;
  company_name: string | null;
  billing_address_id: string | null;
  billing_address: AddressSnapshot | null;
  notes: string | null;                 // internal notes (Absprachen, letzte Anpassungen)
  created_at: string;
  quotes: { id: string; status: string; service_type: string | null; estimated_volume_m3: number | null; scheduled_date: string | null; created_at: string }[];
  offers: { id: string; inquiry_id: string; price_cents: number; status: string; created_at: string; sent_at: string | null }[];
  termine: { id: string; title: string; category: string; scheduled_date: string | null; status: string }[];
  addresses: CustomerAddressItem[];      // reusable address book, most-recently-used first
}
```

`CustomerAddressItem`:
```typescript
{
  id: string;
  street: string;
  house_number: string | null;
  postal_code: string | null;
  city: string;
  country: string;
  floor: string | null;
  elevator: boolean | null;
  parking_ban: boolean;
  latitude: number | null;
  longitude: number | null;
  label: string | null;
  source: string;             // e.g. "manual" | "inquiry"
  last_used_at: string;
}
```

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Found |
| 404 | Customer not found |

---

### PATCH /api/v1/admin/customers/{id}

Partially update a customer. Only provided fields are changed (COALESCE-based).

**Auth**: Bearer JWT

**Request body** (all optional)
```typescript
{
  name?: string;
  salutation?: string;
  first_name?: string;
  last_name?: string;
  phone?: string;
  email?: string;                        // "" clears it
  customer_type?: string;                // "private" | "business"
  company_name?: string;
  notes?: string;
  billing_address?: {                    // inline — creates an address and sets billing_address_id
    street?: string; city?: string; postal_code?: string; floor?: string;
    elevator?: boolean; house_number?: string; parking_ban?: boolean;
  };
  billing_address_id?: string;           // takes priority over inline billing_address if both given
  clear_billing_address?: boolean;       // true clears the override
}
```

**Response** `200 OK` — updated customer (same shape as the list row).

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Updated |
| 404 | Customer not found |
| 422 | Email already used by another customer |

---

### POST /api/v1/admin/customers/{id}/delete

Hard-delete a customer and all linked data (cascades via FK to inquiries, offers, volume_estimations, email_threads, email_messages). S3 objects are orphaned and must be cleaned up separately. Use for GDPR erasure requests.

**Auth**: Bearer JWT (admin)

**Response** `200 OK` — `{ "ok": true }`

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Deleted |
| 403 | Authenticated but not `admin` role |
| 404 | Customer not found |

---

### GET /api/v1/admin/customers/{id}/addresses

List a customer's known-address book. Standalone version of the `addresses` array embedded in the customer detail response.

**Auth**: Bearer JWT

**Response** `200 OK` — array of `CustomerAddressItem`.

---

### POST /api/v1/admin/customers/{id}/addresses

Manually add a known address to a customer's book. Dedup-aware — re-adding a matching address refreshes it instead of creating a duplicate.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  street: string;              // required (with city)
  city: string;                // required (with street)
  house_number?: string;
  postal_code?: string;
  country?: string;
  floor?: string;
  elevator?: boolean;
  parking_ban?: boolean;
  label?: string;
}
```

**Response** `201 Created` — `CustomerAddressItem`.

**Status codes**
| Code | Meaning |
|---|---|
| 201 | Added |
| 400 | Missing street or city |
| 404 | Customer not found |

---

### POST /api/v1/admin/customers/{id}/addresses/{addr_id}/delete

Remove an address from a customer's book. Scoped to the customer, so `addr_id` cannot reach another customer's entry.

**Auth**: Bearer JWT

**Response** `200 OK` — `{ "ok": true }`

**Status codes**: `404` if not found for this customer.

---

### PATCH /api/v1/admin/addresses/{id}

Update an address record used on an inquiry (street, city, postal code, floor, elevator, parking ban).

**Auth**: Bearer JWT

**Request body**
```typescript
{
  street?: string;
  house_number?: string;
  city?: string;
  postal_code?: string;
  floor?: string;
  elevator?: boolean;
  parking_ban?: boolean;
}
```

**Response** `200 OK` — updated address (`{ id, street, house_number, city, postal_code, floor, elevator, parking_ban }`).

**Status codes**: `404` if address not found.

---

## Email Mailbox

`/api/v1/admin/emails*` manages the IMAP-synced customer mailbox: threads, messages, drafts, attachments, and read/handled/muted state. Outbound sending always goes through a `draft` row — replying or composing creates the draft, `send` transmits it via SMTP.

### GET /api/v1/admin/emails

List email threads with customer info and last-message metadata.

**Auth**: Bearer JWT

**Query parameters**: `search` (matches customer name/email, subject, and message body), `limit` (default 50, max 100), `offset`.

**Response** `200 OK`
```typescript
{
  threads: {
    id: string;
    customer_id: string | null;
    customer_email: string | null;
    customer_name: string | null;
    inquiry_id: string | null;
    subject: string | null;
    message_count: number;
    unread_count: number;
    unhandled_count: number;
    muted: boolean;
    last_message_at: string | null;
    last_direction: string | null;   // "inbound" | "outbound"
    created_at: string;
  }[];
  total: number;
}
```

---

### GET /api/v1/admin/emails/{id}

Get a thread with all its messages. Opening a thread marks it read (but not handled — reading a mail is not answering it).

**Auth**: Bearer JWT

**Response** `200 OK`
```typescript
{
  thread: {
    id: string;
    customer_id: string | null;
    muted: boolean;
    customer_email: string | null;
    customer_name: string | null;
    inquiry_id: string | null;
    subject: string | null;
    offer_pdf_filename: string | null;   // the active offer PDF `send` would attach implicitly
    created_at: string;
  };
  messages: {
    id: string;
    direction: "inbound" | "outbound";
    from_address: string;
    to_address: string;
    cc_addresses: string[];
    subject: string | null;
    body_text: string | null;
    body_html: string | null;            // sanitised (scripts/handlers/images/forms stripped)
    llm_generated: boolean;
    status: string;                      // "received" | "draft" | "sent" | "discarded"
    read_at: string | null;
    handled_at: string | null;
    attachment_keys: string[];
    attachment_names: string[];
    created_at: string;
  }[];
}
```

**Status codes**: `404` if thread not found.

---

### PATCH /api/v1/admin/emails/{id}/inquiry

Attach a thread to an inquiry.

**Auth**: Bearer JWT

**Request body**: `{ "inquiry_id": "<uuid>" }`

**Response** `200 OK` — `{ "inquiry_id": "<uuid>" }`

---

### PATCH /api/v1/admin/emails/{id}/mute

Silence a thread's unanswered-mail reminder without marking it as handled.

**Auth**: Bearer JWT

**Request body**: `{ "muted": true }`

**Response** `200 OK` — `{ "muted": boolean }`

---

### GET /api/v1/admin/emails/unread

Badge counts for the mailbox nav.

**Auth**: Bearer JWT

**Response** `200 OK`
```json
{ "unread_messages": 3, "unread_threads": 2, "unhandled": 1 }
```

---

### PATCH /api/v1/admin/emails/messages/{id}/handled

Tick an inbound message off — this is what actually silences the assistant's unanswered-mail nag (`handled_at`), distinct from read state.

**Auth**: Bearer JWT

**Request body**: `{ "handled": true }` (toggle back with `false`)

**Response** `200 OK` — `{ "handled": boolean }`

---

### PATCH /api/v1/admin/emails/messages/{id}

Edit a draft's subject, body, or recipients before sending. Only works on messages in `draft` status.

**Auth**: Bearer JWT

**Request body** (all optional)
```typescript
{ subject?: string; body_text?: string; to_address?: string; cc?: string[]; bcc?: string[]; }
```

**Response** `200 OK` — `{ "ok": true }`

**Status codes**: `404` if the draft doesn't exist or is already sent/discarded.

---

### POST /api/v1/admin/emails/messages/{id}/send

Send a draft via SMTP. The recipient is always corrected to the customer's real email regardless of what the draft stored, and the thread's active offer PDF is attached automatically if one exists. On success, closes the thread's unanswered-mail nag and, if the draft carried an offer, marks the offer/inquiry as sent.

**Auth**: Bearer JWT

**Response** `200 OK` — `{ "message": "E-Mail an <email> gesendet" }`

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Sent |
| 400 | No usable recipient address on the draft |
| 404 | Draft not found or already sent |
| 500 | SMTP failure |

---

### POST /api/v1/admin/emails/messages/{id}/discard

Discard a draft without sending (soft — sets `status = 'discarded'`).

**Auth**: Bearer JWT

**Response** `200 OK` — `{ "ok": true }`

---

### POST /api/v1/admin/emails/{id}/reply

Create a new draft reply in an existing thread (does not send).

**Auth**: Bearer JWT

**Request body**
```typescript
{ subject?: string; body_text: string; cc?: string[]; bcc?: string[]; }
```

**Response** `201 Created` — `{ "id": "<uuid>", "status": "draft" }`

---

### POST /api/v1/admin/emails/compose

Create a new thread and a draft message to an address not necessarily already in the system (upserts the customer by email).

**Auth**: Bearer JWT

**Request body**
```typescript
{ customer_email: string; subject: string; body_text: string; cc?: string[]; bcc?: string[]; }
```

**Response** `201 Created` — `{ "thread_id": "<uuid>", "message_id": "<uuid>" }`

---

### POST /api/v1/admin/emails/messages/{id}/attachments

Attach an uploaded file to a draft (`multipart/form-data`, any field name, max 20 MB per file).

**Auth**: Bearer JWT

**Response** `200 OK` — `{ "attachments": string[] }` (the stored filenames)

---

### GET /api/v1/admin/emails/{id}/documents

List a thread's ready-to-attach documents — the customer's/inquiry's generated KVAs and Rechnungen that have a PDF.

**Auth**: Bearer JWT

**Query parameters**: `message` (optional draft id — flags entries already attached to it)

**Response** `200 OK` — array of:
```typescript
{ kind: "offer" | "invoice"; id: string; label: string; filename: string; created_at: string; attached: boolean }
```

---

### POST /api/v1/admin/emails/messages/{id}/attachments/document

Attach a listed KVA or Rechnung to a draft by reference (no re-upload).

**Auth**: Bearer JWT

**Request body**: `{ "kind": "offer" | "invoice", "id": "<uuid>" }`

**Response** `200 OK` — `{ "attachment": "<filename>" }`

**Status codes**: `400` if already attached; `404` if the draft or document doesn't exist.

---

### GET /api/v1/admin/emails/messages/{id}/attachments/{idx}

Download one attachment of a message by its zero-based index into `attachment_keys`.

**Auth**: Bearer JWT

**Response**: binary with `Content-Disposition: attachment`, or `404`.

---

### GET /api/v1/admin/users

List all admin users.

**Auth**: Bearer JWT (admin)

**Response** `200 OK` — array of user objects (id, email, name, role, created_at).

---

### POST /api/v1/admin/users/{id}/delete

Delete an admin user.

**Auth**: Bearer JWT (admin)

**Response** `200 OK`

---

### GET /api/v1/admin/orders

List completed/confirmed orders (inquiries with status `completed`, `invoiced`, or `paid`, or confirmed bookings).

**Auth**: Bearer JWT

**Response** `200 OK` — list of order summary objects.

---

## Employees

### GET /api/v1/admin/employees

List employees with optional search, active filter, and monthly actual-hours aggregation.

**Auth**: Bearer JWT

**Query parameters**:
| Param | Type | Description |
|---|---|---|
| `search` | string | ILIKE filter on first_name, last_name, email |
| `active` | bool | Filter by active status |
| `month` | string | `YYYY-MM` — when present, includes `actual_hours_month` |
| `limit` | int | Max results (default 50, max 100) |
| `offset` | int | Pagination offset |

**Response** `200 OK`
```json
{
  "employees": [
    {
      "id": "uuid",
      "salutation": "Herr",
      "first_name": "Max",
      "last_name": "Mustermann",
      "email": "max@example.com",
      "phone": "+43 123 456",
      "monthly_hours_target": 160.0,
      "active": true,
      "actual_hours_month": null,
      "created_at": "2026-03-06T..."
    }
  ],
  "total": 5
}
```
Note: there is no `planned_hours_month` field on this row.

---

### POST /api/v1/admin/employees

Create a new employee.

**Auth**: Bearer JWT (admin)

**Request body**:
```json
{
  "salutation": "Herr",
  "first_name": "Max",
  "last_name": "Mustermann",
  "email": "max@example.com",
  "phone": "+43 123 456",
  "monthly_hours_target": 160.0
}
```
`first_name`, `last_name`, `email` are required; the rest are optional (`monthly_hours_target` defaults to 160.0).

**Response** `201 Created` — the new employee object (`{ id, salutation, first_name, last_name, email, phone, monthly_hours_target, active, arbeitsvertrag_key, mitarbeiterfragebogen_key, created_at, updated_at }`).

**Status codes**: `409` if the email is already in use; `403` if not `admin` role.

---

### GET /api/v1/admin/employees/{id}

Get employee detail with recent assignments.

**Auth**: Bearer JWT

**Response** `200 OK` — employee object plus an `assignments` array of `{ inquiry_id, customer_name, origin_city, destination_city, booking_date, actual_hours, notes, status }`.

---

### PATCH /api/v1/admin/employees/{id}

Update employee fields (all optional).

**Auth**: Bearer JWT (admin)

**Request body**: any subset of `{ salutation, first_name, last_name, email, phone, monthly_hours_target, active }`. `salutation` must be `"Herr"`, `"Frau"`, or `"D"` if given.

**Response** `200 OK` — updated employee object.

---

### POST /api/v1/admin/employees/{id}/delete

Soft-delete (set `active = false`) — preserves assignment history.

**Auth**: Bearer JWT (admin)

**Response** `204 No Content`.

---

### GET /api/v1/admin/employees/{id}/hours

Hours summary for a date range, with per-assignment breakdown across moving jobs, internal calendar items, and paid Zusatztermine. Defaults to the current calendar month; supports an explicit `from`/`to` range (used by the 7-day rolling view in the UI).

**Auth**: Bearer JWT

**Query parameters**: `month` (`YYYY-MM`) **or** `from`+`to` (`YYYY-MM-DD` each). Defaults to the current month if none given.

**Response** `200 OK` — a JSON object with (at least): `from`, `to`, `target_hours`, `actual_hours` (paid total, applying any payroll override), `worked_total` (recorded total before override), `assignments` (inquiry-job rows), `calendar_items` (internal-event rows), `appointments` (paid Zusatztermin rows). Each row carries its own `booking_date`/`scheduled_date`, `clock_in`/`clock_out`, `break_minutes`, `actual_hours`/`worked_hours`/`paid_hours`, and (for inquiry/calendar-item rows) `deactivated`/`paid_clock_in`/`paid_clock_out`/`paid_break_minutes` from the payroll-override layer below. This is a dynamically-built JSON object, not a fixed struct — treat unlisted fields as present-but-undocumented rather than assuming absence.

---

### PUT /api/v1/admin/employees/{id}/hours/adjustments

Save payroll edit-mode overrides (deactivations and paid-time corrections) for one month, without touching the recorded worked hours. Replaces the whole month's override set.

**Auth**: Bearer JWT

**Query parameters**: `month` (`YYYY-MM`, defaults to current month)

**Request body**: array of per-day adjustment objects (`entry_type: "inquiry" | "calendar_item"`, source id, `job_date`, `deactivated?`, `paid_clock_in?`, `paid_clock_out?`, `paid_break_minutes?`) — see `employee_repo::HoursAdjustmentInput`. Every `job_date` must fall inside the given month.

**Response** `200 OK` — `{ "ok": true }`

---

### POST /api/v1/admin/employees/{id}/hours/cleanup

**Destructive and irreversible.** "Stundenkonto säubern": bakes the month's payroll overrides into the recorded data — deactivated days drop the assignment, adjusted days overwrite the recorded clock times with the paid values — then discards the override layer.

**Auth**: Bearer JWT

**Query parameters**: `month` (`YYYY-MM`, defaults to current month)

**Response** `200 OK` — `{ "ok": true }`

---

### GET /api/v1/admin/employees/{id}/hours/export

Download the monthly Stundenzettel (timesheet), applying any payroll overrides.

**Auth**: Bearer JWT

**Query parameters**: `month` (`YYYY-MM`, defaults to current month), `format=pdf` (defaults to XLSX)

**Response** `200 OK` with `Content-Disposition: attachment` and XLSX or PDF bytes.

---

### POST /api/v1/admin/employees/{id}/documents/{doc_type}

Upload an employee document. `doc_type` is `"arbeitsvertrag"` or `"mitarbeiterfragebogen"`.

**Auth**: Bearer JWT (admin) — these are sensitive personnel documents.

**Request**: `multipart/form-data` with a single `file` field.

**Response** `200 OK` — updated employee object (with the new `arbeitsvertrag_key`/`mitarbeiterfragebogen_key`).

---

### GET /api/v1/admin/employees/{id}/documents/{doc_type}

Download an employee document.

**Auth**: Bearer JWT (admin)

**Response**: binary with `Content-Disposition: attachment`, or `404` if none uploaded.

---

### DELETE /api/v1/admin/employees/{id}/documents/{doc_type}

Remove an employee document (S3 object + DB key).

**Auth**: Bearer JWT (admin)

**Response** `200 OK` — updated employee object.

---

### Inquiry Employee Assignments

The crew for the move itself (as opposed to a linked appointment's crew — see [Inquiry Appointments](#inquiry-appointments)).

#### GET /api/v1/inquiries/{id}/employees

List employees assigned to this inquiry.

**Auth**: Bearer JWT

**Response** `200 OK` — array of:
```typescript
{
  employee_id: string;
  first_name: string;
  last_name: string;
  job_date: string;               // the day this assignment covers (multi-day inquiries have one row per day)
  notes: string | null;
  start_time: string | null;
  end_time: string | null;
  clock_in: string | null;
  clock_out: string | null;
  break_minutes: number;
  actual_hours: number | null;
  transport_mode: string | null;
  travel_costs_cents: number | null;
  accommodation_cents: number | null;
  misc_costs_cents: number | null;
  meal_deduction: string | null;
}[]
```
Note: there is no `planned_hours` field — hours are derived from `clock_in`/`clock_out`/`break_minutes` or set directly as `actual_hours`.

---

#### POST /api/v1/inquiries/{id}/employees

Assign an employee to this inquiry.

**Auth**: Bearer JWT

**Request body**: `{ "employee_id": "<uuid>", "notes"?: string }`

**Response** `201 Created`.

**Status codes**
| Code | Meaning |
|---|---|
| 201 | Assigned |
| 400 | Employee exists but is inactive |
| 404 | Inquiry or employee not found |
| 409 | Employee already assigned |

---

#### PUT /api/v1/inquiries/{id}/employees

Full-replace the inquiry's crew list in one call. Same body shape as invoked per-entry in the `PATCH` below (array of `{ employee_id, notes?, clock_in?, clock_out?, start_time?, end_time?, break_minutes?, actual_hours?, transport_mode?, travel_costs_cents?, accommodation_cents?, misc_costs_cents?, meal_deduction? }`).

**Auth**: Bearer JWT

**Response** `200 OK`.

---

#### PATCH /api/v1/inquiries/{id}/employees/{emp_id}

Update one assignment's clock times, hours, or expenses.

**Auth**: Bearer JWT

**Request body** (all optional)
```typescript
{
  clock_in?: string;             // lenient — accepts "7:30", "07:30", "7.30"
  clock_out?: string;
  start_time?: string;
  end_time?: string;
  break_minutes?: number;
  actual_hours?: number;         // manual override; else derived from clock_in/clock_out - break
  notes?: string;
  day_date?: string;             // scopes the update to one day of a multi-day inquiry
  transport_mode?: string;
  travel_costs_cents?: number;
  accommodation_cents?: number;
  meal_deduction?: string;
}
```

**Response** `200 OK` — updated assignment.

---

#### DELETE /api/v1/inquiries/{id}/employees/{emp_id}

Remove employee from inquiry.

**Auth**: Bearer JWT

**Response** `204 No Content`.

---

## Notes (Notepad)

Freeform sticky-note widget on the admin dashboard, unrelated to inquiry notes.

### GET /api/v1/admin/notes

**Auth**: Bearer JWT

**Response** `200 OK` — `{ "notes": [{ id, title, content, color, pinned, created_at, updated_at }] }`, pinned first then most recent.

---

### POST /api/v1/admin/notes

**Auth**: Bearer JWT

**Request body**: `{ title?: string; content?: string; color?: string; pinned?: boolean; }` — all optional, empty defaults applied.

**Response** `201 Created` — the new note.

---

### PATCH /api/v1/admin/notes/{id}

**Auth**: Bearer JWT

**Request body**: any subset of `{ title, content, color, pinned }`.

**Response** `200 OK` — updated note, or `404` if not found.

---

### DELETE /api/v1/admin/notes/{id}

**Auth**: Bearer JWT

**Response** `204 No Content`, or `404` if not found.

---

## Feedback (Bug Reports & Feature Requests)

Admin-facing bug/feature tracker, also used as the Telegram-assistant/agent work queue. Full detail — including the `FeedbackReport` shape, status values, and a worked agent workflow — is documented separately in [`AGENT_API.md`](../AGENT_API.md). Summary:

| Endpoint | Auth |
|---|---|
| `GET /api/v1/admin/feedback` | Bearer JWT (admin) |
| `POST /api/v1/admin/feedback` (multipart: `type`, `title`, `priority?`, `description?`, `location?`, `attachments?`) | Bearer JWT (any role) |
| `GET /api/v1/admin/feedback/{id}` | Bearer JWT (admin) |
| `PATCH /api/v1/admin/feedback/{id}` (`status?`, `agent_notes?`) | Bearer JWT (admin) |
| `GET /api/v1/admin/feedback/{id}/attachments/{idx}` | Bearer JWT (admin) |

---

## Invoice Reminders (Dunning)

### GET /api/v1/admin/invoice-reminders

List invoices sent but unpaid past their reminder interval.

**Auth**: Bearer JWT

**Response** `200 OK` — array of due-reminder rows (`billing_reminder_service::DueInvoiceReminder`): invoice id, customer, amount, current dunning level, and the date it became due.

---

### POST /api/v1/admin/invoice-reminders/{id}/action

Act on one due reminder.

**Auth**: Bearer JWT (admin)

**Request body**
```typescript
{
  action: "send" | "later" | "paid";
  snooze_days?: number;   // only for "later", default 7
}
```
- `"send"` — sends the appropriate dunning email (Zahlungserinnerung / 1. Mahnung / 2. Mahnung) and advances the level, or closes the reminder after level 3.
- `"later"` — postpones by `snooze_days`.
- `"paid"` — marks the underlying invoice paid and closes the reminder.

**Response** `200 OK` — `{ "status": "sent", "level": number }` | `{ "status": "snoozed", "remind_after": string }` | `{ "status": "paid", "review_prompt": boolean, "inquiry_id": string | null }`

---

## Review Requests

### POST /api/v1/admin/inquiries/{id}/review-request

Send or schedule a Google-review request email for a completed inquiry.

**Auth**: Bearer JWT (admin)

**Request body**
```typescript
{ action: "now" | "later" | "skip"; remind_after_days?: number; } // remind_after_days required only for "later"
```

**Response** `200 OK` — `{ "status": "sent" | "pending" | "skipped", "remind_after": string | null }`

---

### GET /api/v1/admin/review-reminders

List overdue "Später" review-request reminders (also surfaced as `pending_review_count` on the dashboard).

**Auth**: Bearer JWT (admin)

**Response** `200 OK` — array of `{ inquiry_id, remind_after, customer_name, customer_email }`.

---

## Morning Workflow

### GET /api/v1/admin/morning-workflow

Jobs whose last working day has passed (within 14 days) but aren't fully closed — powers the dashboard's "Guten Morgen" checklist.

**Auth**: Bearer JWT

**Response** `200 OK`
```typescript
{
  inquiries: {
    id: string; customer_name: string | null; customer_email: string | null;
    last_day: string | null; status: string;
    invoice_status: string | null; invoice_id: string | null; invoice_type: string | null;
    has_review_request: boolean; offer_price_cents: number | null;
  }[];
  calendar_items: { id: string; title: string; last_day: string | null; status: string }[];
}
```

---

## Rechnungsausgangsbuch (Invoice Register)

The legal invoice ledger — merges core-invoice rows and storage-invoice rows (they share one invoice-number sequence) into one flat, number-ordered list.

### GET /api/v1/admin/rechnungsausgangsbuch

**Auth**: Bearer JWT

**Response** `200 OK` — array of:
```typescript
{
  id: string;
  kind: "umzug" | "lagerung";
  inquiry_id: string | null;         // null for "lagerung"
  invoice_number: string;
  customer_name: string | null;
  scheduled_date: string | null;     // start of the Leistungszeitraum
  end_date: string | null;           // end of the Leistungszeitraum
  netto_cents: number | null;
  mwst_cents: number | null;
  brutto_cents: number | null;       // null means "amount unknown", not zero
  sent_at: string | null;
  created_at: string;
  due_date: string | null;
  paid_at: string | null;
  offene_zahlungen_cents: number | null;
  is_settled: boolean;
  paid_amount_cents: number | null;  // recorded Teilzahlung, if any
  payment_method: string | null;
  notes: string | null;
  invoice_type: string;              // "full" | "partial_first" | "partial_final" | "lagerung"
  partial_percent: number | null;
  status: string;                    // "draft" | "ready" | "sent" | "paid"
  is_gutschrift: boolean;
  is_legacy: boolean;                // imported historical row, no PDF/job to open
  pdf_s3_key: string | null;
}
```

---

### PATCH /api/v1/admin/rechnungsausgangsbuch/{id}/payment-method

**Auth**: Bearer JWT (admin)

**Request body**: `{ "payment_method": string | null }`

**Response** `200 OK` — `{ "ok": true }`. `id` may be a core-invoice or storage-invoice id; both tables are tried.

---

### PATCH /api/v1/admin/rechnungsausgangsbuch/{id}/notes

Set the Bemerkung cell (the column Alex actually writes in — reconciliation notes, part-payment context).

**Auth**: Bearer JWT (admin)

**Request body**: `{ "notes": string | null }`

**Response** `200 OK` — `{ "ok": true }`, or `404` if `id` matches neither register table.

---

### PATCH /api/v1/admin/rechnungsausgangsbuch/{id}/paid-amount

Record a Teilzahlung (partial payment) without marking the invoice settled — it stays in the Offen column and the dunning list until fully paid or explicitly marked paid.

**Auth**: Bearer JWT (admin)

**Request body**: `{ "paid_amount_cents": number | null }` (`null` clears it)

**Response** `200 OK` — `{ "ok": true }`

**Status codes**: `400` if negative; `404` if `id` matches neither table.

---

### POST /api/v1/admin/rechnungsausgangsbuch/{id}/paid

Mark a register row as fully paid. For a core invoice this also closes any open dunning step and, once the inquiry's last invoice is paid, settles the inquiry and may prompt a review request.

**Auth**: Bearer JWT (admin)

**Request body**: `{ "paid_on"?: string }` (`YYYY-MM-DD`, defaults to today)

**Response** `200 OK` — `billing_reminder_service::PaidOutcome`: `{ review_prompt: boolean, inquiry_id: string | null, ... }`

---

### GET /api/v1/admin/rechnungsausgangsbuch/export

Download one calendar year of the register as XLSX, in Alex's own column order.

**Auth**: Bearer JWT (admin)

**Query parameters**: `year` (defaults to current year)

**Response** `200 OK` with `Content-Disposition: attachment` and XLSX bytes.

---

## KVA-Buch (Quote Register)

Counterpart to the Rechnungsausgangsbuch, for Kostenvoranschläge (KVAs/Angebote). Win/loss (`lage`) is derived from the linked inquiry's status, not from `offers.status` (which is largely unmaintained in production).

### GET /api/v1/admin/kva-buch

**Auth**: Bearer JWT

**Response** `200 OK` — array of:
```typescript
{
  id: string;
  inquiry_id: string;
  offer_number: string | null;
  customer_name: string | null;
  scheduled_date: string | null;
  netto_cents: number;
  mwst_cents: number;
  brutto_cents: number;
  status: string;                 // raw offers.status — kept for completeness, not authoritative
  valid_until: string | null;
  sent_at: string | null;
  created_at: string;
  invoice_number: string | null;  // once the KVA turned into a job
  pdf_s3_key: string | null;
  lage: "gewonnen" | "verloren" | "offen" | "unbekannt";
  age_days: number;               // days since created_at
  needs_followup: boolean;        // open, overdue, move date still ahead — what the Telegram nag fires on
  followup_date_missing: boolean; // open, overdue, but no move date to check liveness against
  move_date_passed: boolean;      // open but the move date already passed
  followup_muted: boolean;
  followup_last_pinged_on: string | null;
}
```

---

### PATCH /api/v1/admin/kva-buch/{offer_id}/followup-mute

Silence one KVA's follow-up nag without moving the threshold for everything else.

**Auth**: Bearer JWT

**Request body**: `{ "muted": boolean }`

**Response** `204 No Content`, or `404` if the offer doesn't exist.

---

### PUT /api/v1/admin/kva-buch/followup-days

Set the global follow-up threshold (days since KVA creation before it's flagged).

**Auth**: Bearer JWT

**Request body**: `{ "days": number }` (1–365)

**Response** `204 No Content`

---

### GET /api/v1/admin/kva-buch/{offer_id}/pdf

Download one specific KVA document — unlike `GET /api/v1/inquiries/{id}/pdf` (which only serves the currently-active offer), this serves any listed KVA, including rejected/expired ones.

**Auth**: Bearer JWT

**Response** `200 OK` with the PDF (or, for pre-PDF-pipeline rows, an XLSX), or `404` if no file was generated.

---

### GET /api/v1/admin/kva-buch/export

Download one calendar year of the KVA register as XLSX.

**Auth**: Bearer JWT (admin)

**Query parameters**: `year` (defaults to current year)

**Response** `200 OK` with `Content-Disposition: attachment` and XLSX bytes.

---

## Settings

### GET /api/v1/admin/settings

**Auth**: Bearer JWT (admin)

**Response** `200 OK`
```typescript
{
  pricing: {
    rate_per_person_hour_cents: number;
    saturday_surcharge_cents: number;
    fahrt_rate_per_km: number;
    assembly_price: number;
    parking_ban_price: number;
    packing_price: number;
    transporter_price: number;
  };
  next_invoice_number: number;
  next_offer_number: number;
}
```

---

### PUT /api/v1/admin/settings/pricing

Persist the standard pricing values (see shape above).

**Auth**: Bearer JWT (admin)

**Response** `200 OK` — `{ "ok": true }`

---

### PUT /api/v1/admin/settings/numbers

Set the next value the invoice and/or KVA sequences will hand out. Only the provided fields change.

**Auth**: Bearer JWT (admin)

**Request body**: `{ next_invoice_number?: number; next_offer_number?: number; }` (each must be ≥ 1)

**Response** `200 OK` — `{ "ok": true }`

---

## Flash Contact

Ultra-quick "call me back" form embedded on the public site — pings Alex on Telegram immediately.

### POST /api/v1/flash-contact

**Auth**: None (public, rate-limited to 10 req/60s per IP — its own limiter bucket, separate from the auth rate limit)

**Request body**: `{ name: string; phone: string; time_preference: "morning" | "afternoon" | "evening" }` (exact `TimePreference` variants — check `aust_flash_contact::TimePreference` if extending)

**Response** `201 Created` — `{ "id": "<uuid>", "message": "Vielen Dank! Wir melden uns bei Ihnen." }`

**Status codes**: `400` if `name`/`phone` empty or too long (120/40 chars).

---

### GET /api/v1/admin/flash-contacts

List the most recent 200 flash-contact submissions.

**Auth**: Bearer JWT

**Response** `200 OK` — array of `{ id, name, phone, time_preference, created_at, reminder_sent_at, handled_at, next_remind_at, dismissed_at }`.

---

### POST /api/v1/admin/flash-contacts/{id}/handle

Mark a flash contact as handled.

**Auth**: Bearer JWT

**Response** `204 No Content`

---

## Storage / Lagerung

Storage-rental contracts and their auto-generated monthly invoices, mounted at `/api/v1/admin/storage`. Money is entered and returned in **brutto** cents in the request/response bodies here (Alex thinks brutto); netto is derived and stored internally.

### GET /api/v1/admin/storage/contracts

**Auth**: Bearer JWT

**Response** `200 OK` — array of:
```typescript
{
  id: string; customer_id: string; customer_name: string | null;
  billing_address_id: string | null;
  contract_start: string; contract_end: string | null;
  sqm: number;
  monthly_netto_cents: number; monthly_brutto_cents: number;
  billing_day: number;          // day-of-month billing runs, clamped to 28
  status: "active" | "ended" | "cancelled";
  note: string | null;
}
```

---

### POST /api/v1/admin/storage/contracts

**Auth**: Bearer JWT

**Request body**
```typescript
{
  customer_id: string;
  billing_address_id?: string;
  contract_start: string;         // "YYYY-MM-DD"
  contract_end?: string;
  sqm: number;                    // > 0
  monthly_brutto_cents: number;   // > 0
  status?: string;                // default "active"
  note?: string;
}
```

**Response** `200 OK` — the new contract (same shape as the list row).

---

### PATCH /api/v1/admin/storage/contracts/{id}

Full update — same body as create, all fields required by the handler (not a partial patch despite the HTTP verb).

**Auth**: Bearer JWT

**Response** `200 OK` — updated contract.

---

### DELETE /api/v1/admin/storage/contracts/{id}

**Auth**: Bearer JWT

**Response** `204 No Content`

**Status codes**: `400` if the contract already has invoices (FK RESTRICT — end the contract instead of deleting it); `404` if not found.

---

### POST /api/v1/admin/storage/contracts/{id}/generate-now

Force-generate this month's invoice for a contract immediately, instead of waiting for its billing day.

**Auth**: Bearer JWT

**Response** `200 OK` — `{ "created": boolean, "invoice_id": string | null }`

---

### GET /api/v1/admin/storage/invoices

**Auth**: Bearer JWT

**Query parameters**: `status` (optional filter)

**Response** `200 OK` — array of:
```typescript
{
  id: string; contract_id: string; invoice_number: string;
  period_year: number; period_month: number; period_label: string;  // e.g. "März 2026"
  netto_cents: number; brutto_cents: number;
  status: string; payment_method: string | null; customer_name: string | null;
  sqm: number; has_pdf: boolean; created_at: string;
}
```

---

### GET /api/v1/admin/storage/invoices/{id}/pdf

**Auth**: Bearer JWT

**Response** `200 OK` with the PDF (or XLSX for pre-PDF rows), or `404` if not yet generated.

---

### POST /api/v1/admin/storage/invoices/{id}/approve

Approve and send a generated storage invoice — same funnel as the Telegram inline approval button.

**Auth**: Bearer JWT

**Response** `204 No Content`

---

### POST /api/v1/admin/storage/invoices/{id}/reject

**Auth**: Bearer JWT

**Response** `204 No Content`

---

## Vehicles

Fleet management — vehicles and their maintenance reminders (TÜV, Ölwechsel, …), mounted at `/api/v1/admin/vehicles`. A background job pings Telegram as a reminder's due date approaches.

### GET /api/v1/admin/vehicles

**Auth**: Bearer JWT

**Response** `200 OK` — array of vehicles with their reminders.

---

### POST /api/v1/admin/vehicles

**Auth**: Bearer JWT

**Request body**: `{ "label": string, "kennzeichen": string }` (both required, non-empty, max 120/20 chars)

**Response** `201 Created` — the new vehicle.

---

### PATCH /api/v1/admin/vehicles/{id}

Rename a vehicle / change its Kennzeichen. Full replace, not a partial patch.

**Auth**: Bearer JWT

**Request body**: same as create — `{ "label": string, "kennzeichen": string }`

**Response** `200 OK` — updated vehicle.

---

### DELETE /api/v1/admin/vehicles/{id}

**Auth**: Bearer JWT

**Response** `204 No Content`

---

### POST /api/v1/admin/vehicles/{id}/reminders

Add a reminder to a vehicle.

**Auth**: Bearer JWT

**Request body**: `{ "label": string, "due_date": "YYYY-MM-DD" }`

**Response** `201 Created` — the new reminder.

---

### PATCH /api/v1/admin/vehicles/{id}/reminders/{rid}

Edit, complete, or dismiss a reminder.

**Auth**: Bearer JWT

**Request body** (all optional): `{ label?: string; due_date?: "YYYY-MM-DD"; active?: boolean; }` — `active: false` dismisses it and stops the Telegram nag; `true` reactivates it.

**Response** `200 OK` — updated reminder.

---

### DELETE /api/v1/admin/vehicles/{id}/reminders/{rid}

**Auth**: Bearer JWT

**Response** `204 No Content`

---

## Calendar Items

Internal work items (training, maintenance, vehicle inspections, team meetings) that need employee assignment and hours tracking like moving inquiries do, but aren't customer jobs. Mounted at `/api/v1/admin/calendar-items`.

### GET /api/v1/admin/calendar-items

**Auth**: Bearer JWT

**Query parameters**: `month` (`YYYY-MM`, optional — all items returned if omitted)

**Response** `200 OK` — array of calendar-item rows, ordered by `scheduled_date` ascending (nulls last).

---

### POST /api/v1/admin/calendar-items

**Auth**: Bearer JWT

**Request body**
```typescript
{
  title: string;                  // required, non-empty
  description?: string;
  category?: string;              // default "intern"
  location?: string;
  scheduled_date?: string;
  start_time: string;             // required, "HH:MM:SS"
  end_time?: string;
  duration_hours?: number;        // default 0
  customer_id?: string;
}
```

**Response** `201 Created` — the new item.

---

### GET /api/v1/admin/calendar-items/{id}

**Auth**: Bearer JWT

**Response** `200 OK` — item detail plus an `employees` array (per-employee hours/clock times, same shape family as the inquiry crew).

**Status codes**: `404` if not found.

---

### PATCH /api/v1/admin/calendar-items/{id}

Partial update. Only provided fields change.

**Auth**: Bearer JWT

**Request body** (all optional): `title, description, category, location, scheduled_date, start_time, end_time, duration_hours, status, customer_id, remove_customer, employee_notes, end_date, has_pauschale`. Moving `scheduled_date` on a multi-day item shifts `end_date` by the same delta unless `end_date` is given explicitly.

**Response** `200 OK` — updated item.

---

### DELETE /api/v1/admin/calendar-items/{id}

**Auth**: Bearer JWT

**Response** `204 No Content`

---

### GET /api/v1/admin/calendar-items/{id}/employees

**Auth**: Bearer JWT

**Response** `200 OK` — array of assigned employees.

---

### POST /api/v1/admin/calendar-items/{id}/employees

Assign an employee.

**Auth**: Bearer JWT

**Request body**: `{ "employee_id": "<uuid>" }`

**Response** `201 Created`.

---

### PUT /api/v1/admin/calendar-items/{id}/employees

Full-replace the assigned crew.

**Auth**: Bearer JWT

**Response** `200 OK`.

---

### PATCH /api/v1/admin/calendar-items/{id}/employees/{emp_id}

Update one assignment's clock times/hours/notes. Time fields (`clock_in`, `clock_out`, `start_time`, `end_time`) use the lenient parser (`"7:30"`, `"07:30"`, `"7.30"`). Same expense fields as the inquiry crew endpoint (`transport_mode`, `travel_costs_cents`, `accommodation_cents`, `misc_costs_cents`, `meal_deduction`), plus `day_date` to scope a multi-day item.

**Auth**: Bearer JWT

**Response** `200 OK`.

---

### DELETE /api/v1/admin/calendar-items/{id}/employees/{emp_id}

**Auth**: Bearer JWT

**Response** `204 No Content`

---

## Agent Activity

Read-only audit log of the Telegram assistant's tool calls (`agent_actions` table), mounted at `/api/v1/admin/agent-activity`. See [`AGENT_API.md`](../AGENT_API.md) for the assistant's own tool surface.

### GET /api/v1/admin/agent-activity

**Auth**: Bearer JWT

**Query parameters**: `tool_name`, `session_id`, `since` (RFC 3339), `only_errors` (bool), `only_confirmed` (bool), `limit` (default 100, max 500), `cursor` (a UUID v7 `id` from a previous page, for pagination — rows are ordered newest-first).

**Response** `200 OK`
```typescript
{
  items: {
    id: string; session_id: string; tool_name: string;
    args_summary: string;             // first 200 chars of the args JSON
    result_summary: string | null;    // first 200 chars of the result JSON, or the error message
    duration_ms: number | null;
    confirmed: boolean;
    ts: string;
  }[];
  next_cursor: string | null;
}
```

---

### GET /api/v1/admin/agent-activity/{id}

Full row including raw `args`/`result` JSONB, for the detail panel.

**Auth**: Bearer JWT

**Response** `200 OK` — `{ id, session_id, tool_name, args, result, error_message, duration_ms, confirmed_action_id, ts }`, or `404`.

---

### GET /api/v1/admin/agent-activity/stats

Aggregated call counts and error rate, optionally scoped to a time window.

**Auth**: Bearer JWT

**Query parameters**: `since` (RFC 3339, defaults to the last 7 days)

**Response** `200 OK` — `{ total_calls, error_count, confirmed_count, error_rate, by_tool: [{ tool_name, count, error_count, confirmed_count }] }`

---

## Customer Portal

`/api/v1/customer/*` — the customer-facing app. Uses a **session token**, not the admin JWT: obtained via OTP login and validated by `middleware::customer_auth::require_customer_auth` (DB-backed, in `customer_sessions`, 30-day expiry). Pass it the same way: `Authorization: Bearer <session_token>`.

### POST /api/v1/customer/auth/request

Request a 6-digit OTP by email. Always returns success (no information leakage about which emails exist — customers are auto-created on verify).

**Auth**: None

**Request body**: `{ "email": string }`

**Response** `200 OK` — generic confirmation message.

---

### POST /api/v1/customer/auth/verify

Verify the code, upsert the customer by email, and return a session token.

**Auth**: None

**Request body**: `{ "email": string, "code": string }`

**Response** `200 OK` — `{ token: string, customer: { id, email, name, salutation, first_name, last_name, phone } }`

---

### GET /api/v1/customer/me

**Auth**: Customer session token

**Response** `200 OK` — the customer profile (same shape as `verify`'s `customer` field).

---

### GET /api/v1/customer/inquiries

List the authenticated customer's own inquiries, with latest offer price.

**Auth**: Customer session token

**Response** `200 OK` — array of `{ id, status, scheduled_date, created_at, origin_city, destination_city, estimated_volume_m3, price_cents }`.

---

### GET /api/v1/customer/inquiries/{id}

Detail for one owned inquiry — addresses, latest estimation (with items), and offers. Financial fields are limited to price/status, not the full admin `OfferSnapshot`.

**Auth**: Customer session token — ownership is enforced (`inquiry.customer_id` must match the session).

**Response** `200 OK` — `{ id, status, estimated_volume_m3, distance_km, scheduled_date, origin_address, destination_address, estimation, offers: [{ id, price_cents, status, valid_until, persons, hours_estimated }] }`, or `404` if not found/not owned.

---

### POST /api/v1/customer/inquiries/{id}/accept

Accept the inquiry's current offer (must be in `draft` or `sent` status). Updates offer → `accepted`, inquiry → `accepted`, and notifies the admin via Telegram.

**Auth**: Customer session token

**Response** `200 OK` — `{ "message": "Angebot angenommen", "status": "accepted" }`

**Status codes**: `400` if the offer isn't in an acceptable status; `404` if no inquiry/active offer.

---

### POST /api/v1/customer/inquiries/{id}/reject

Mirror of `accept` — offer/inquiry → `rejected`, admin notified.

**Auth**: Customer session token

**Response** `200 OK` — `{ "message": "Angebot abgelehnt", "status": "rejected" }`

---

### GET /api/v1/customer/inquiries/{id}/pdf

Download the active offer's PDF.

**Auth**: Customer session token

**Response** `200 OK` with `Content-Type: application/pdf`, or `404` if no active offer/PDF.

---

## Employee Portal

`/api/v1/employee/*` — the worker (Mitarbeiter) portal. Uses a separate **employee session token** (OTP login, `employee_sessions`, 30-day expiry), validated by `middleware::employee_auth::require_employee_auth`. Financial data (prices, offers) is never exposed here — only logistics and the worker's own hours.

### POST /api/v1/employee/auth/request / POST /api/v1/employee/auth/verify

Same OTP flow as the customer portal, but only sends a code if the email matches an active employee (`request` always returns the same generic message either way, to avoid leaking which emails are registered). `verify` returns `{ token, employee: { id, email, first_name, last_name, salutation, phone } }`.

**Auth**: None

---

### GET /api/v1/employee/me

**Auth**: Employee session token

**Response** `200 OK` — the employee profile.

---

### GET /api/v1/employee/schedule

Combined, date-sorted list of the employee's own assignments for a month: moving jobs, internal calendar items, and paid Zusatztermine.

**Auth**: Employee session token

**Query parameters**: `month` (`YYYY-MM`, defaults to current month)

**Response** `200 OK` — array of:
```typescript
{
  entry_type: "job" | "item" | "appointment";
  inquiry_id: string | null; calendar_item_id: string | null; appointment_id: string | null;
  title: string | null; location: string | null; category: string | null;
  job_date: string | null; status: string;
  origin_street: string | null; origin_city: string | null; origin_postal_code: string | null;
  destination_street: string | null; destination_city: string | null; destination_postal_code: string | null;
  estimated_volume_m3: number | null;
  customer_name: string | null; customer_phone: string | null;
  actual_hours: number | null;
  colleague_names: string[];
  employee_notes: string | null;
}[]
```

---

### GET /api/v1/employee/pending-hours

Past assignments (job day already passed) the worker hasn't logged hours for yet — drives a blocking modal in the portal.

**Auth**: Employee session token

**Response** `200 OK` — array of pending-hours rows, oldest first.

---

### GET /api/v1/employee/jobs/{id}

Full logistics detail for one assigned moving job (addresses, items, customer contact phone, photo/video URLs, teammates, own clock times). No price/offer data.

**Auth**: Employee session token

**Query parameters**: `date` (`YYYY-MM-DD`, selects one day of a multi-day inquiry)

**Response** `200 OK`, or `404` if not assigned.

---

### PATCH /api/v1/employee/jobs/{id}/clock

Self-report clock-in/out for a job.

**Auth**: Employee session token

**Query parameters**: `date` (anchors bare-time input to a day; defaults to the job's scheduled date)

**Request body**: `{ employee_clock_in?: string; employee_clock_out?: string; employee_break_minutes?: number; }` — datetimes are ISO 8601, or bare `HH:MM`/`HH:MM:SS` combined with the anchor date. `null`/empty clears a field.

**Response** `204 No Content`. A complete pair (in + out) notifies the office via Telegram.

---

### GET /api/v1/employee/items/{id} / PATCH /api/v1/employee/items/{id}/clock

Same shape as the job endpoints above, for an assigned internal calendar item (Termin).

**Auth**: Employee session token

---

### GET /api/v1/employee/appointments/{id} / PATCH /api/v1/employee/appointments/{id}/clock

Same shape again, for an assigned paid Zusatztermin (e.g. a Halteverbotszone booking). Includes the linked customer contact and the entry's own address.

**Auth**: Employee session token

---

### GET /api/v1/employee/hours

Monthly hours overview — planned target vs. actual, across jobs/items/appointments. No financial data.

**Auth**: Employee session token

**Query parameters**: `month` (`YYYY-MM`, defaults to current month)

**Response** `200 OK` — `{ month, target_hours, actual_hours, assignment_count, assignments: [{ entry_type, inquiry_id, calendar_item_id, appointment_id, title, location, job_date, origin_city, destination_city, actual_hours, status }] }`

---

## Error Responses

All errors follow a consistent JSON shape:

```typescript
{
  error: string;   // human-readable message
}
```

| HTTP Status | When |
|---|---|
| 400 Bad Request | Malformed input, business rule violation |
| 401 Unauthorized | Missing or invalid/expired JWT |
| 403 Forbidden | Authenticated but insufficient role |
| 404 Not Found | Resource does not exist |
| 409 Conflict | Duplicate resource (e.g. employee already assigned) |
| 422 Unprocessable Entity | Validation error (missing required field, invalid format) |
| 500 Internal Server Error | Unexpected server-side error |

---

## Notes on Money

All monetary values are stored and returned in **cents** (`i64`). Prices in the database are netto (excluding VAT). The Austrian VAT rate is 19 %; brutto = `netto_cents * 1.19`.

The admin UI and Telegram approval workflow work with brutto prices. When the admin types a bare number (e.g. "350 Euro"), the system interprets it as brutto and back-calculates to netto for storage.
