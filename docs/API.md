# AUST Backend — API Reference

Base URL: `http://localhost:8080` (development) / `https://<production-host>` (production)

All JSON request bodies must include `Content-Type: application/json`.

## Authentication

Most admin endpoints require a JWT Bearer token obtained from `POST /api/v1/auth/login`.

Pass it as:
```
Authorization: Bearer <access_token>
```

Public endpoints (health, `GET /api/v1/estimates/images/*`) require no token.

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

## Quotes

### POST /api/v1/quotes

Create a new quote record. Typically created automatically by the orchestrator from incoming emails; use directly to create quotes manually.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  customer_id: string;                   // UUID of an existing customer
  origin_address_id?: string;            // UUID of an existing address
  destination_address_id?: string;       // UUID of an existing address
  preferred_date?: string;               // ISO 8601 datetime, e.g. "2026-03-15T09:00:00Z"
  notes?: string;                        // comma-separated services / free text
}
```

**Response** `200 OK`
```typescript
{
  id: string;
  customer_id: string;
  origin_address_id: string | null;
  destination_address_id: string | null;
  stop_address_id: string | null;
  status: QuoteStatus;
  estimated_volume_m3: number | null;
  distance_km: number | null;
  preferred_date: string | null;
  notes: string | null;
  created_at: string;
  updated_at: string;
}
```

`QuoteStatus` values: `pending` | `info_requested` | `volume_estimated` | `offer_generated` | `offer_sent` | `accepted` | `rejected` | `expired` | `cancelled` | `done` | `paid`

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/quotes \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{
    "customer_id": "019500000000000000000000",
    "preferred_date": "2026-04-01T08:00:00Z",
    "notes": "Halteverbot Auszug, Verpackungsservice"
  }'
```

---

### GET /api/v1/quotes

List quotes with optional filters and pagination.

**Auth**: Bearer JWT

**Query parameters**
| Parameter | Type | Description |
|---|---|---|
| `status` | string | Filter by quote status |
| `customer_id` | UUID | Filter by customer |
| `limit` | integer | Max results (default 50, max 100) |
| `offset` | integer | Pagination offset (default 0) |

**Response** `200 OK`
```typescript
{
  quotes: Quote[];   // array of Quote objects (same shape as POST response)
  total: number;
  limit: number;
  offset: number;
}
```

**Example**
```bash
curl "http://localhost:8080/api/v1/quotes?status=pending&limit=20" \
  -H "Authorization: Bearer <token>"
```

---

### GET /api/v1/quotes/{id}

Get a single quote enriched with customer, addresses, estimation, and linked offers.

**Auth**: Bearer JWT

**Response** `200 OK`
```typescript
{
  quote: {
    id: string;
    volume_m3: number | null;
    distance_km: number;
    notes: string | null;
    status: string;
    customer_message: string | null;   // non-service portion of notes
    created_at: string;
  };
  customer: {
    id: string;
    email: string;
    name: string | null;
    phone: string | null;
  };
  origin_address: {
    id: string;
    street: string;
    city: string;
    postal_code: string | null;
    floor: string | null;
    elevator: boolean | null;
  } | null;
  destination_address: { /* same shape */ } | null;
  estimation: {
    id: string;
    method: "vision" | "inventory" | "depth_sensor" | "video" | "manual";
    total_volume_m3: number;
    items: {
      name: string;
      volume_m3: number;
      quantity: number;
      confidence: number;
      crop_url: string | null;
      source_image_url: string | null;
      bbox: number[] | null;
    }[];
    source_images: string[];   // relative URLs: /api/v1/estimates/images/<key>
    source_videos: string[];
  } | null;
  offers: {
    id: string;
    total_brutto_cents: number | null;
    status: string;
    created_at: string;
  }[];
  latest_offer: {
    offer_id: string;
    persons: number;
    hours: number;
    rate_cents: number;
    total_netto_cents: number;
    total_brutto_cents: number;
    line_items: {
      label: string;
      remark: string | null;
      quantity: number;
      unit_price_cents: number;
      total_cents: number;
      is_labor: boolean;
    }[];
  } | null;
}
```

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Found |
| 404 | Quote not found |

**Example**
```bash
curl http://localhost:8080/api/v1/quotes/019500000000000000000000 \
  -H "Authorization: Bearer <token>"
```

---

### PATCH /api/v1/quotes/{id}

Partially update a quote. All fields are optional; only provided fields are updated.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  origin_address_id?: string;
  destination_address_id?: string;
  status?: QuoteStatus;
  estimated_volume_m3?: number;
  distance_km?: number;
  preferred_date?: string;
  notes?: string;
}
```

**Response** `200 OK` — updated `Quote` object.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Updated |
| 404 | Quote not found |

**Example**
```bash
curl -X PATCH http://localhost:8080/api/v1/quotes/019500000000000000000000 \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"estimated_volume_m3": 18.5, "status": "volume_estimated"}'
```

---

### DELETE /api/v1/quotes/{id}

Soft-delete a quote (sets status to `cancelled`).

**Auth**: Bearer JWT

**Response** `200 OK` — updated `Quote` object with `status: "cancelled"`.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Cancelled |
| 404 | Quote not found |

---

### PUT /api/v1/quotes/{id}/estimation-items

Replace the detected items list on the latest volume estimation for this quote and recalculate the total volume. Used by the admin UI to correct ML detection results.

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
  items: EstimationItem[];
  source_images: string[];
  source_videos: string[];
}
```

**Business rules**
- Updates both `volume_estimations.result_data` and `quotes.estimated_volume_m3`.
- Fails with 404 if no estimation exists for this quote.

**Example**
```bash
curl -X PUT http://localhost:8080/api/v1/quotes/019500000000000000000000/estimation-items \
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

## Volume Estimation

### GET /api/v1/estimates/images/{key}

Serve an image or video from storage. Used as `<img src>` or `<video src>` in the admin UI. Does not require authentication.

**Auth**: None (public route)

**Path parameter**: `key` is the full storage key returned in `source_images` / `source_videos` arrays.

**Response**: Raw binary with appropriate `Content-Type` header.

---

### POST /api/v1/estimates/vision

Analyze one or more room photos using the LLM vision model. Images are submitted as base64-encoded JSON. Stores results and updates the quote's volume. Triggers auto offer generation in the background.

**Auth**: Bearer JWT

**Request body** (`application/json`)
```typescript
{
  quote_id: string;   // UUID
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
  quote_id: string;
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
  -d "{\"quote_id\":\"019500000000000000000000\",\"images\":[{\"data\":\"$IMAGE_B64\",\"mime_type\":\"image/jpeg\"}]}"
```

---

### POST /api/v1/estimates/depth-sensor

Upload photos for 3D ML volume estimation (depth-sensor / photogrammetry pipeline). Uses the Modal vision service when available; falls back to LLM vision analysis automatically. Accepted as a `multipart/form-data` upload.

**Auth**: Bearer JWT

**Request** (`multipart/form-data`)
| Field | Type | Description |
|---|---|---|
| `quote_id` | text | UUID of the quote |
| `<any name>` | file | Image files (JPEG, PNG, etc.); one field per image |

**Response** `200 OK` — `VolumeEstimation` object (same shape as vision estimate).

**Business rules**
- Images are stored in S3 before analysis.
- If the Modal vision service fails, the system automatically retries with the LLM.
- On completion, `quotes.estimated_volume_m3` is updated and an offer is generated in the background.

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/estimates/depth-sensor \
  -H "Authorization: Bearer <token>" \
  -F "quote_id=019500000000000000000000" \
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
| `quote_id` | text | UUID of the quote |
| `video` | file | Video file (MP4, MOV, WebM, MKV) — one per request |
| `max_keyframes` | text (optional) | Override number of keyframes to extract |
| `detection_threshold` | text (optional) | Override object detection confidence threshold |

**Response** `200 OK` — array of `VolumeEstimation` objects (one per video).

The returned estimation has `status: "processing"`. Poll `GET /api/v1/estimates/{id}` to check for completion.

**Business rules**
- Requires the Modal vision service to be configured (`AUST__VISION_SERVICE__ENABLED=true`). Returns 500 if not configured.
- Quote status is set to `processing` immediately.
- When all videos for a quote finish processing, volumes are summed and offer generation is triggered.
- Default timeout: 600 seconds.

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/estimates/video \
  -H "Authorization: Bearer <token>" \
  -F "quote_id=019500000000000000000000" \
  -F "video=@walkthrough.mp4"
```

---

### POST /api/v1/estimates/inventory

Submit a manual inventory list. Calculates total volume by summing item volumes and quantities.

**Auth**: Bearer JWT

**Request body** (`application/json`)
```typescript
{
  quote_id: string;
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
    "quote_id": "019500000000000000000000",
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

## Offers

### POST /api/v1/offers/generate

Generate a PDF offer from a quote. Runs the pricing engine, fills the XLSX template, converts to PDF via LibreOffice, and stores the result in S3. The offer is stored in the database with `status: "draft"`.

**Auth**: Bearer JWT

**Request body** (`application/json`)
```typescript
{
  quote_id: string;               // UUID — quote must have an estimated_volume_m3
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

**Response** `200 OK`
```typescript
{
  id: string;
  quote_id: string;
  price_cents: number;                // netto price in cents
  currency: string;                   // "EUR"
  valid_until: string | null;         // date string "YYYY-MM-DD"
  pdf_storage_key: string | null;
  status: "draft" | "sent" | "viewed" | "accepted" | "rejected" | "expired";
  created_at: string;
  sent_at: string | null;
  offer_number: string | null;
  persons: number | null;
  hours_estimated: number | null;
  rate_per_hour_cents: number | null;
  line_items_json: object[] | null;
}
```

**Business rules**
- The quote must have `estimated_volume_m3` set or the request fails with 400.
- Default pricing: computed from volume, distance, floor levels, and date (Saturday surcharge).
- When `price_cents_netto` is provided together with existing `persons` and `hours`, the `rate` is back-calculated as `(netto - non_labor_items) / (persons * hours)`.
- LibreOffice must be installed and `soffice` available on PATH.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Offer generated |
| 400 | Quote has no volume estimate |
| 404 | Quote or customer not found |
| 500 | PDF generation failed (LibreOffice error) |

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/offers/generate \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"quote_id":"019500000000000000000000","valid_days":14}'
```

---

### GET /api/v1/offers/{id}

Retrieve an offer by ID.

**Auth**: Bearer JWT

**Response** `200 OK` — `Offer` object (same shape as generate response).

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Found |
| 404 | Offer not found |

**Example**
```bash
curl http://localhost:8080/api/v1/offers/019500000000000000000002 \
  -H "Authorization: Bearer <token>"
```

---

### GET /api/v1/offers/{id}/pdf

Download the offer PDF as an octet-stream.

**Auth**: Bearer JWT

**Response** `200 OK` with `Content-Type: application/pdf` and `Content-Disposition: attachment; filename="Angebot_<number>.pdf"`.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | PDF returned |
| 404 | Offer not found or PDF not yet generated |

**Example**
```bash
curl http://localhost:8080/api/v1/offers/019500000000000000000002/pdf \
  -H "Authorization: Bearer <token>" \
  -o Angebot.pdf
```

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
    quote_id: string | null;
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

---

### POST /api/v1/calendar/bookings

Create a new calendar booking. Fails if the date is already at capacity.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  booking_date: string;             // "YYYY-MM-DD"
  quote_id?: string;                // UUID — link to an existing quote
  customer_name?: string;
  customer_email?: string;
  departure_address?: string;
  arrival_address?: string;
  volume_m3?: number;
  distance_km?: number;
  description?: string;
  status?: string;                  // default: "confirmed"
}
```

**Response** `200 OK` — `Booking` object.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Booking created |
| 400 | Date is fully booked |

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/calendar/bookings \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{
    "booking_date": "2026-04-15",
    "quote_id": "019500000000000000000000",
    "customer_name": "Max Mustermann",
    "departure_address": "Musterstr. 1, 1010 Wien",
    "arrival_address": "Neugasse 5, 1020 Wien"
  }'
```

---

### GET /api/v1/calendar/bookings/{id}

Get a single booking by ID.

**Auth**: Bearer JWT

**Response** `200 OK` — `Booking` object.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Found |
| 404 | Booking not found |

---

### PATCH /api/v1/calendar/bookings/{id}

Update booking status. Accepted values are `confirmed` and `cancelled`. When a booking linked to a quote is confirmed, the quote status is also updated to `accepted`. When cancelled, the quote status reverts to `offer_sent`.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  status: "confirmed" | "cancelled";
}
```

**Response** `200 OK` — updated `Booking` object.

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Updated |
| 400 | Invalid status value |
| 404 | Booking not found |

**Example**
```bash
curl -X PATCH http://localhost:8080/api/v1/calendar/bookings/019500000000000000000003 \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"status":"confirmed"}'
```

---

### DELETE /api/v1/calendar/bookings/{id}

Delete a booking record.

**Auth**: Bearer JWT

**Response** `200 OK`
```json
{ "ok": true }
```

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Deleted |
| 404 | Booking not found |

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
  open_quotes: number;      // status in (pending, info_requested, volume_estimated)
  pending_offers: number;   // status = draft
  todays_bookings: number;
  total_customers: number;
  recent_activity: {
    type: string;           // e.g. "offer_draft", "offer_sent"
    description: string;
    created_at: string;
  }[];
  conflict_dates: {
    date: string;           // dates in next 30 days where bookings >= capacity
    booked: number;
    capacity: number;
  }[];
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
| `limit` | integer | Max results (default 50) |
| `offset` | integer | Pagination offset |

**Response** `200 OK`
```typescript
{
  customers: {
    id: string;
    email: string;
    name: string | null;
    phone: string | null;
    created_at: string;
    quote_count: number;
  }[];
  total: number;
}
```

---

### POST /api/v1/admin/customers

Create a new customer record.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  email: string;   // valid email address
  name?: string;
  phone?: string;  // minimum 5 characters
}
```

**Response** `200 OK` — `Customer` object.

---

### GET /api/v1/admin/customers/{id}

Get a single customer with their complete quote history.

**Auth**: Bearer JWT

**Response** `200 OK` — customer object with nested quotes.

---

### PATCH /api/v1/admin/customers/{id}

Update customer name or phone.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  name?: string;
  phone?: string;
}
```

**Response** `200 OK` — updated `Customer` object.

---

### GET /api/v1/admin/quotes

List all quotes with full customer and address data. Supports filtering and pagination.

**Auth**: Bearer JWT

**Query parameters**: `status`, `customer_id`, `limit`, `offset` (same as `GET /api/v1/quotes`).

**Response** `200 OK` — paginated list with enriched quote objects (includes customer name/email).

---

### POST /api/v1/admin/quotes

Create a quote (admin path, same functionality as `POST /api/v1/quotes`).

---

### GET /api/v1/admin/quotes/{id}

Get a quote with full detail including all linked offers, estimation, and addresses.

---

### GET /api/v1/admin/offers

List all offers, ordered by creation date (newest first).

**Auth**: Bearer JWT

**Query parameters**: `limit`, `offset`

**Response** `200 OK` — array of offer detail objects.

---

### GET /api/v1/admin/offers/{id}

Get a full offer detail record including line items and linked quote/customer data.

**Auth**: Bearer JWT

**Response** `200 OK` — enriched offer object with:
- Quote + customer information
- Full line item breakdown
- PDF download URL

---

### PATCH /api/v1/admin/offers/{id}

Update an offer's metadata (e.g. subject or body of the draft email).

**Auth**: Bearer JWT

**Request body** — partial update of editable offer fields.

**Response** `200 OK` — updated offer.

---

### POST /api/v1/admin/offers/{id}/regenerate

Regenerate the offer PDF with new pricing overrides. Updates the existing offer record in-place (preserves offer number and creation date).

**Auth**: Bearer JWT

**Request body**
```typescript
{
  price_cents_netto?: number;
  persons?: number;
  hours?: number;
  rate?: number;
  line_items?: {
    description: string;
    quantity: number;
    unit_price: number;
    remark?: string;
  }[];
}
```

**Response** `200 OK` — updated `Offer` object.

**Example**
```bash
curl -X POST http://localhost:8080/api/v1/admin/offers/019500000000000000000002/regenerate \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"persons": 3, "hours": 6}'
```

---

### POST /api/v1/admin/offers/{id}/send

Send the offer PDF to the customer via SMTP email. Updates offer status to `sent`.

**Auth**: Bearer JWT

**Request body** (optional)
```typescript
{
  subject?: string;   // custom email subject
  body?: string;      // custom email body (HTML or plain text)
}
```

**Response** `200 OK`
```json
{ "ok": true }
```

**Status codes**
| Code | Meaning |
|---|---|
| 200 | Email sent |
| 404 | Offer not found |
| 500 | SMTP delivery failed |

---

### POST /api/v1/admin/offers/{id}/reject

Mark an offer as rejected.

**Auth**: Bearer JWT

**Response** `200 OK` — updated `Offer` object with `status: "rejected"`.

---

### POST /api/v1/admin/offers/{id}/re-estimate

Re-run volume estimation for the offer's quote using updated data, then regenerate the offer.

**Auth**: Bearer JWT

**Response** `200 OK` — regenerated `Offer` object.

---

### PATCH /api/v1/admin/addresses/{id}

Update an address record (street, city, postal code, floor, elevator).

**Auth**: Bearer JWT

**Request body**
```typescript
{
  street?: string;
  city?: string;
  postal_code?: string;
  floor?: string;
  elevator?: boolean;
}
```

**Response** `200 OK` — updated address.

---

### GET /api/v1/admin/emails

List email threads (IMAP conversation history).

**Auth**: Bearer JWT

**Query parameters**: `limit`, `offset`

**Response** `200 OK` — paginated list of email threads.

---

### GET /api/v1/admin/emails/{id}

Get a single email thread with all messages.

**Auth**: Bearer JWT

**Response** `200 OK` — email thread object with messages array.

---

### PATCH /api/v1/admin/emails/messages/{id}

Update a draft email message (e.g. edit subject or body before sending).

**Auth**: Bearer JWT

**Request body** — partial message fields.

**Response** `200 OK` — updated message.

---

### POST /api/v1/admin/emails/messages/{id}/send

Send a draft email message.

**Auth**: Bearer JWT

**Response** `200 OK`

---

### POST /api/v1/admin/emails/messages/{id}/discard

Discard (delete) a draft message.

**Auth**: Bearer JWT

**Response** `200 OK`

---

### POST /api/v1/admin/emails/{id}/reply

Send a reply to an existing email thread.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  body: string;
  subject?: string;
}
```

**Response** `200 OK`

---

### POST /api/v1/admin/emails/compose

Compose and send a new outbound email (not a reply).

**Auth**: Bearer JWT

**Request body**
```typescript
{
  to: string;
  subject: string;
  body: string;
}
```

**Response** `200 OK`

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

### POST /api/v1/admin/offers/{id}/delete

Hard delete an offer record and its associated PDF from storage.

**Auth**: Bearer JWT

**Response** `200 OK`

---

### POST /api/v1/admin/quotes/{id}/delete

Hard delete a quote and all its linked estimations, offers, and storage objects.

**Auth**: Bearer JWT

**Response** `200 OK`

---

### POST /api/v1/admin/customers/{id}/delete

Delete a customer and all linked data.

**Auth**: Bearer JWT

**Response** `200 OK`

---

### POST /api/v1/admin/quotes/{id}/status

Manually set a quote's status.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  status: QuoteStatus;
}
```

**Response** `200 OK` — updated quote.

---

### GET /api/v1/admin/orders

List completed/confirmed orders (quotes with status `done` or `paid`, or confirmed bookings).

**Auth**: Bearer JWT

**Response** `200 OK` — list of order summary objects.

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
| 422 Unprocessable Entity | Validation error (missing required field, invalid format) |
| 500 Internal Server Error | Unexpected server-side error |

---

## Notes on Money

All monetary values are stored and returned in **cents** (`i64`). Prices in the database are netto (excluding VAT). The Austrian VAT rate is 19 %; brutto = `netto_cents * 1.19`.

The admin UI and Telegram approval workflow work with brutto prices. When the admin types a bare number (e.g. "350 Euro"), the system interprets it as brutto and back-calculates to netto for storage.
