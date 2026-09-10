# crates/email-agent — IMAP Email Polling + Telegram Approval

Background service: polls IMAP inbox for customer emails, parses them into inquiries, forwards offers to Telegram for Alex's approval.

> **Runs in production, inside the `aust_backend` process** — it is not a separate
> service, so a panic here aborts the whole API (this is what caused the June 2026
> UTF-8 crash-loop). Inquiries also arrive via web form and the admin dashboard.

## Processing Flow

```
IMAP poll → parse email → extract JSON attachment or plain text
  → create/upsert customer → create inquiry → estimate volume → auto-offer
  → send to Telegram → poll for approval → dispatch email on accept
```

## Key Files

| File | Purpose |
|------|---------|
| `src/processor.rs` | Main orchestrator, state machine for drafts/approvals |
| `src/parser.rs` | Email content parsing (HTML → text, JSON attachment extraction) |
| `src/responder.rs` | LLM-powered response generation/revising |
| `src/telegram.rs` | Telegram Bot integration (inline keyboards, calendar commands) |

## Char-Boundary Safety

Every place this crate slices a `String` for a preview/log/Telegram message goes
through `text::truncate_on_char_boundary(s, max_bytes)` (13 call sites across
`processor.rs`, `responder.rs`, `telegram.rs`, `text.rs`), which snaps the cut down
to the nearest UTF-8 char boundary. A plain `&s[..n]` panics whenever `n` lands
inside a multi-byte character — common in German text (ü, ä, ö, ß) — and because
this crate runs inside the main process, that panic previously aborted the whole
backend and crash-looped it on the same unread mail. Never slice a String here
without it.

## JSON Form Attachment Parsing

Two web forms feed this path — `kostenloses-angebot` (hyphenated field names) and
`manuell-angebot` (snake_case). `ParsedInquiry`'s serde fields accept both via
`alias` (e.g. `etage-auszug` / `etage_auszug`, `halteverbot-auszug` / `halteverbot_auszug`).
Key field mappings (hyphenated form shown; see `parser.rs` for the aliases):

| JSON Field | MovingInquiry Field |
|-----------|-------------------|
| `name` | name |
| `email` | email (NOT IMAP sender — that's the company inbox) |
| `phone` | phone |
| `wunschtermin` | scheduled_date |
| `auszugsadresse`, `etage-auszug`, `halteverbot-auszug` | departure address/floor/parking ban |
| `einzugsadresse`, `etage-einzug`, `halteverbot-einzug` | arrival address/floor/parking ban |
| `umzugsvolumen-m3` | volume_m3 |
| `gegenstaende-liste` | items_list (VolumeCalculator format) |
| `zusatzleistungen` | services (comma-separated German names) |
| `nachricht` | notes |

## Customer Email Fix

IMAP sender for form submissions is always the company inbox (`<company-inbox>`). After parsing, the processor uses the email from the JSON form data instead — ensures correct customer record.

## Inbound Persistence and Threading

Every inbound mail is written to `email_messages` with `status = 'received'`, and the
IMAP `\Seen` flag is set **only after that row commits** — flagging a mail read that
failed to persist drops it out of the `UNSEEN` search permanently and it exists nowhere
else.

`find_or_create_thread` always returns a thread. Resolution order:

1. `In-Reply-To` / `References` matched against `email_messages.message_id`.
2. The customer's most recent thread inside 30 days.
3. A new thread — with `customer_id NULL` and the sender in `contact_address` when the
   mail cannot be attributed to anyone.

`handled_at IS NULL` (not `status`) is what the assistant's unanswered-email reminder
reconciles against; see `crates/assistant/src/hooks/reminders.rs`.

## State Management

- `inquiries: HashMap<String, MovingInquiry>` — per-customer inquiry data
- `pending_drafts: HashMap<String, PendingDraft>` — awaiting Telegram approval
- `editing_draft: Option<PendingDraft>` — current draft in edit mode

## External Connections

IMAP (polling), SMTP (sending), Telegram Bot API, LLM provider, Calendar service.
## ⚠️ Connected Changes

| If you change... | ...along change... |
|---|---|
| Email parser / `ParsedInquiry` | `submissions.rs` form parsing (shares field names like `halteverbot-auszug`), `inquiry_builder.rs` field mapping |
| Telegram approval flow | `telegram_service.rs` callback data, `offer_pipeline.rs` auto-offer, `orchestrator.rs` event handling |
| `MovingInquiry` struct | `inquiry_builder.rs` response builder, `inquiry_repo.rs` field names |
