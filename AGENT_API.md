# Agent API — Feedback (Bug Reports & Feature Requests)

This document covers everything an agent (Claude Code or any automated client) needs to
read open bug reports / feature requests and write back investigation notes and status
updates after fixing an issue.

---

## Base URL

```
https://api.aufraeumhelden.com/api/v1/admin
```

All endpoints require an `Authorization: Bearer <token>` header.

---

## Authentication

### Algorithm

**HS256** (HMAC-SHA256). The signing secret is `jwt_secret` from the server's `[auth]`
config section.

### Claims payload

```json
{
  "sub":   "<admin-user-uuid>",
  "email": "agent@aust-umzuege.de",
  "role":  "admin",
  "iat":   1744459200,
  "exp":   1744545600
}
```

| Field   | Type          | Notes |
|---------|---------------|-------|
| `sub`   | UUID string   | Must match an existing user row UUID (or any valid UUID for a dedicated agent identity) |
| `email` | string        | Informational only — not validated beyond presence |
| `role`  | `"admin"`     | Must be exactly `"admin"` (lowercase). `"buerokraft"` / `"operator"` are blocked by `require_admin()` |
| `iat`   | Unix seconds  | Issued-at |
| `exp`   | Unix seconds  | Expiry — validated by the JWT library; set far in the future for a long-lived agent token |

### Option A — Use the login endpoint

Pre-configured agent credentials (from the `.env` in the project root):

| Field    | Value                |
|----------|----------------------|
| `email`  | `agent@test.com`     |
| `password` | `HelloAgent123!`   |

```http
POST /api/v1/auth/login
Content-Type: application/json

{
  "email":    "agent@test.com",
  "password": "HelloAgent123!"
}
```

Response:

```json
{
  "access_token":  "eyJ...",
  "refresh_token": "eyJ...",
  "token_type":    "Bearer",
  "expires_in":    86400
}
```

Use `access_token` directly. Lifetime is controlled by `jwt_expiry_hours` in config
(default 24 h). Use `POST /api/v1/auth/refresh` with the `refresh_token` to get a new
pair without re-entering credentials.

### Option B — Mint a long-lived token manually

**Python**

```python
import jwt, time, uuid

payload = {
    "sub":   "00000000-0000-0000-0000-000000000001",   # replace with real admin UUID
    "email": "claude-agent@aust-umzuege.de",
    "role":  "admin",
    "iat":   int(time.time()),
    "exp":   int(time.time()) + 365 * 24 * 3600,       # 1 year
}

token = jwt.encode(payload, "YOUR_JWT_SECRET", algorithm="HS256")
print(token)
```

**Node.js**

```js
const jwt = require('jsonwebtoken');

const token = jwt.sign(
  {
    sub:   "00000000-0000-0000-0000-000000000001",
    email: "claude-agent@aust-umzuege.de",
    role:  "admin",
  },
  "YOUR_JWT_SECRET",
  { algorithm: "HS256", expiresIn: "365d" }
);

console.log(token);
```

### Using the token

Add it as a header on every request:

```
Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...
```

---

## The `FeedbackReport` object

All endpoints return this shape:

```json
{
  "id":              "0196f3b2-1234-7abc-8def-000000000001",
  "report_type":     "bug",
  "priority":        "high",
  "title":           "Kalender zeigt falsches Datum",
  "description":     "Beim Klick auf KW 17 springt die Ansicht auf KW 18.",
  "location":        "/admin/calendar",
  "attachment_keys": ["feedback/0196f3b2-.../0.png"],
  "status":          "open",
  "agent_notes":     null,
  "created_at":      "2026-04-12T10:00:00Z",
  "updated_at":      "2026-04-12T10:00:00Z"
}
```

| Field            | Type            | Notes |
|------------------|-----------------|-------|
| `id`             | UUID v7         | Primary key |
| `report_type`    | `"bug"` \| `"feature"` | |
| `priority`       | `"low"` \| `"medium"` \| `"high"` \| `"critical"` | |
| `title`          | string          | Short summary |
| `description`    | string \| null  | Full description |
| `location`       | string \| null  | Page or area in the app, e.g. `/admin/calendar` |
| `attachment_keys`| string[]        | S3 keys — fetch via the attachment download endpoint |
| `status`         | see below       | Default: `"open"` |
| `agent_notes`    | string \| null  | Written by Claude after investigation or fix |
| `created_at`     | ISO 8601        | |
| `updated_at`     | ISO 8601        | |

**Valid statuses**: `"open"` · `"in_progress"` · `"resolved"` · `"needs_clarification"`

---

## Endpoints

### `GET /feedback` — List reports

```
GET /api/v1/admin/feedback
GET /api/v1/admin/feedback?type=bug&status=open
```

**Query parameters** (all optional):

| Param    | Values                                                         |
|----------|----------------------------------------------------------------|
| `status` | `open` \| `in_progress` \| `resolved` \| `needs_clarification` |
| `type`   | `bug` \| `feature`                                             |

**Response**: `200 OK` — JSON array of `FeedbackReport`, newest first.

---

### `GET /feedback/{id}` — Get single report

```
GET /api/v1/admin/feedback/0196f3b2-1234-7abc-8def-000000000001
```

**Response**: `200 OK` — single `FeedbackReport`, or `404` if not found.

---

### `PATCH /feedback/{id}` — Update status / write agent notes

This is the primary endpoint for agent use. After investigating or fixing an issue,
write a summary here so humans can track what happened.

```
PATCH /api/v1/admin/feedback/0196f3b2-1234-7abc-8def-000000000001
Content-Type: application/json
```

**Body** — at least one field required:

```json
{
  "status":      "resolved",
  "agent_notes": "Fixed in commit f454466 — lane assignment rewritten in +page.svelte."
}
```

| Field         | Type   | Notes |
|---------------|--------|-------|
| `status`      | string | Omit to leave unchanged |
| `agent_notes` | string | Omit to leave unchanged. This field **overwrites** — pass the full new value |

**Example bodies**:

```json
{ "status": "in_progress", "agent_notes": "Reproducing locally." }
{ "status": "resolved",    "agent_notes": "Fixed in commit abc123 — moved query to repo layer." }
{ "status": "needs_clarification", "agent_notes": "Which floor behaviour is expected for EG — should it count as 0 or 1?" }
{ "agent_notes": "Root cause identified: race condition in offer_builder. Working on fix." }
```

**Response**: `200 OK` — updated `FeedbackReport`.

**Errors**:

| Status | Reason |
|--------|--------|
| `400`  | Neither `status` nor `agent_notes` provided, or invalid status string |
| `401`  | Missing or invalid JWT |
| `403`  | Token role is not `"admin"` |
| `404`  | Report not found |

---

### `GET /feedback/{id}/attachments/{idx}` — Download attachment

```
GET /api/v1/admin/feedback/0196f3b2-.../attachments/0
```

Returns the file as a binary response with `Content-Disposition: attachment` and the
correct `Content-Type`. Attachment index is zero-based (matches position in
`attachment_keys` array).

**Response**: `200 OK` binary, or `404` if the index is out of range.

---

### `POST /feedback` — Create report

Only needed if an agent wants to file a report programmatically. Accepts
`multipart/form-data`.

**Form fields**:

| Field         | Required | Description |
|---------------|----------|-------------|
| `type`        | Yes      | `"bug"` or `"feature"` |
| `title`       | Yes      | Short summary — non-empty |
| `priority`    | No       | `"low"` / `"medium"` (default) / `"high"` / `"critical"` |
| `description` | No       | Full description |
| `location`    | No       | Page or area, e.g. `/admin/calendar` |
| `attachments` | No       | File(s) — uploaded to S3 automatically |

**Response**: `201 Created` — the created `FeedbackReport`.

---

## Typical agent workflow

```
1.  GET  /feedback?type=bug&status=open          # fetch all open bugs

2.  GET  /feedback/{id}                          # inspect a specific report
                                                 # (includes description, location,
                                                 #  attachment_keys)

3.  GET  /feedback/{id}/attachments/0            # download screenshot if present

4.  ... investigate, fix, commit ...

5.  PATCH /feedback/{id}                         # write fix summary and close
    {
      "status":      "resolved",
      "agent_notes": "Fixed in commit <sha> — <one-line description of what changed>."
    }
```

If a bug needs more context from the human admin before it can be fixed:

```json
{
  "status":      "needs_clarification",
  "agent_notes": "The report mentions 'wrong floor', but I need to know: does EG count as floor 0 or floor 1 in the pricing formula?"
}
```

---

## Role access summary

| Endpoint | Required role |
|----------|---------------|
| `GET /feedback` | `admin` |
| `GET /feedback/{id}` | `admin` |
| `PATCH /feedback/{id}` | `admin` |
| `GET /feedback/{id}/attachments/{idx}` | `admin` |
| `POST /feedback` | any authenticated user |
