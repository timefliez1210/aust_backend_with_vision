# AUST Backend — API Reference

Base URL: `http://localhost:8080` (development) / `https://<production-host>` (production)

All JSON request bodies must include `Content-Type: application/json`.

## Authentication

Most admin endpoints require a JWT Bearer token obtained from `POST /api/v1/auth/login`.

Pass it as:
```
Authorization: Bearer <access_token>
```

Public endpoints (health, `GET /api/v1/estimates/images/*`, `POST /api/v1/submit/*`) require no token.

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

Status transitions are validated server-side by `InquiryStatus::can_transition_to()`.

### InquiryResponse type

```typescript
{
  id: string;
  status: InquiryStatus;
  source: string;       // "direct_email" | "admin_dashboard" | "photo_webapp" | "mobile_app"
  services: Services;   // { packing, assembly, disassembly, storage, disposal, parking_ban_origin, parking_ban_destination }
  volume_m3: number | null;
  distance_km: number | null;
  preferred_date: string | null;
  notes: string | null;
  customer_message: string | null;
  created_at: string;
  updated_at: string;
  offer_sent_at: string | null;
  accepted_at: string | null;
  customer: CustomerSnapshot | null;
  origin_address: AddressSnapshot | null;
  destination_address: AddressSnapshot | null;
  stop_address: AddressSnapshot | null;
  estimation: EstimationSnapshot | null;
  items: ItemSnapshot[];
  offer: OfferSnapshot | null;
  employees: EmployeeAssignmentSnapshot[];  // empty array if none assigned
}
```

`EmployeeAssignmentSnapshot`:
```typescript
{
  employee_id: string;
  first_name: string;
  last_name: string;
  planned_hours: number;
  actual_hours: number | null;
  notes: string | null;
}
```

`CustomerSnapshot`:
```typescript
{
  id: string;
  email: string;
  name: string | null;
  phone: string | null;
}
```

`AddressSnapshot`:
```typescript
{
  id: string;
  street: string;
  city: string;
  postal_code: string | null;
  floor: string | null;
  elevator: boolean | null;
}
```

`EstimationSnapshot`:
```typescript
{
  id: string;
  method: "vision" | "inventory" | "depth_sensor" | "video" | "manual";
  total_volume_m3: number;
  source_images: string[];
  source_videos: string[];
}
```

`ItemSnapshot`:
```typescript
{
  name: string;
  volume_m3: number;
  quantity: number;
  confidence: number;
  crop_url: string | null;
  source_image_url: string | null;
  bbox: number[] | null;
}
```

`OfferSnapshot`:
```typescript
{
  id: string;
  offer_number: string | null;
  persons: number | null;
  hours_estimated: number | null;
  rate_per_hour_cents: number | null;
  total_netto_cents: number;
  total_brutto_cents: number;
  status: string;
  valid_until: string | null;
  pdf_storage_key: string | null;
  line_items: {
    label: string;
    remark: string | null;
    quantity: number;
    unit_price_cents: number;
    total_cents: number;
    is_labor: boolean;
  }[];
  created_at: string;
}
```

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
  preferred_date?: string;                // ISO 8601, e.g. "2026-03-15T09:00:00Z"
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
    "preferred_date": "2026-04-01T08:00:00Z",
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
  preferred_date?: string;
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
| `preferred_date` | text | No | ISO 8601 date |
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
| `preferred_date` | text | No | ISO 8601 date |
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

---

### POST /api/v1/calendar/bookings

Create a new calendar booking. Fails if the date is already at capacity.

**Auth**: Bearer JWT

**Request body**
```typescript
{
  booking_date: string;             // "YYYY-MM-DD"
  inquiry_id?: string;                // UUID — link to an existing inquiry
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
    "inquiry_id": "019500000000000000000000",
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

Update booking status. Accepted values are `confirmed` and `cancelled`. When a booking linked to an inquiry is confirmed, the inquiry status is also updated to `scheduled`. When cancelled, the inquiry status reverts to `offer_sent`.

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
  open_inquiries: number;     // status in (pending, info_requested, estimating, estimated)
  pending_offers: number;     // status = offer_ready (not yet sent)
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
    inquiry_count: number;
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

Get a single customer with their complete inquiry history.

**Auth**: Bearer JWT

**Response** `200 OK` — customer object with nested inquiries.

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

### POST /api/v1/admin/customers/{id}/delete

Delete a customer and all linked data.

**Auth**: Bearer JWT

**Response** `200 OK`

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

### GET /api/v1/admin/orders

List completed/confirmed orders (inquiries with status `completed`, `invoiced`, or `paid`, or confirmed bookings).

**Auth**: Bearer JWT

**Response** `200 OK` — list of order summary objects.

---

## Employees

### GET /api/v1/admin/employees

List employees with optional search, active filter, and monthly hours aggregation.

**Auth**: Bearer JWT

**Query parameters**:
| Param | Type | Description |
|---|---|---|
| `search` | string | ILIKE filter on first_name, last_name, email |
| `active` | bool | Filter by active status |
| `month` | string | `YYYY-MM` — includes `planned_hours_month` and `actual_hours_month` |
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
      "planned_hours_month": 32.0,
      "actual_hours_month": null,
      "created_at": "2026-03-06T..."
    }
  ],
  "total": 5
}
```

---

### POST /api/v1/admin/employees

Create a new employee.

**Auth**: Bearer JWT

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

**Response** `201 Created` — the new employee object.

---

### GET /api/v1/admin/employees/{id}

Get employee detail with recent assignments.

**Auth**: Bearer JWT

**Response** `200 OK` — employee object with `assignments` array.

---

### PATCH /api/v1/admin/employees/{id}

Update employee fields (all optional).

**Auth**: Bearer JWT

**Request body**: any subset of `{ salutation, first_name, last_name, email, phone, monthly_hours_target, active }`.

**Response** `200 OK` — updated employee object.

---

### POST /api/v1/admin/employees/{id}/delete

Soft-delete (set `active=false`).

**Auth**: Bearer JWT

**Response** `204 No Content`.

---

### GET /api/v1/admin/employees/{id}/hours

Monthly hours summary with per-assignment breakdown.

**Auth**: Bearer JWT

**Query parameters**:
| Param | Type | Description |
|---|---|---|
| `month` | string | `YYYY-MM` (defaults to current month) |

**Response** `200 OK`
```json
{
  "month": "2026-03",
  "target_hours": 160.0,
  "planned_hours": 32.0,
  "actual_hours": 0.0,
  "assignment_count": 4,
  "assignments": [
    {
      "inquiry_id": "uuid",
      "customer_name": "...",
      "origin_city": "Wien",
      "destination_city": "Graz",
      "booking_date": "2026-03-15",
      "planned_hours": 8.0,
      "actual_hours": null,
      "status": "scheduled"
    }
  ]
}
```

---

### Inquiry Employee Assignments

#### GET /api/v1/inquiries/{id}/employees

List employees assigned to this inquiry.

**Auth**: Bearer JWT

**Response** `200 OK`
```json
{
  "assignments": [
    {
      "employee_id": "uuid",
      "first_name": "Max",
      "last_name": "Mustermann",
      "email": "max@example.com",
      "planned_hours": 8.0,
      "actual_hours": null,
      "notes": null
    }
  ]
}
```

---

#### POST /api/v1/inquiries/{id}/employees

Assign an employee to this inquiry.

**Auth**: Bearer JWT

**Request body**:
```json
{
  "employee_id": "uuid",
  "planned_hours": 8.0,
  "notes": "Teamleiter"
}
```

**Response** `201 Created`.

---

#### PATCH /api/v1/inquiries/{id}/employees/{emp_id}

Update assignment hours/notes.

**Auth**: Bearer JWT

**Request body**: any subset of `{ planned_hours, actual_hours, notes }`.

**Response** `200 OK` — updated assignment.

---

#### DELETE /api/v1/inquiries/{id}/employees/{emp_id}

Remove employee from inquiry.

**Auth**: Bearer JWT

**Response** `204 No Content`.

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
