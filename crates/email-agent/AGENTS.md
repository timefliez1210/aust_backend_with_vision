# crates/email-agent — IMAP Email Polling + Telegram Approval

Background service: polls IMAP inbox for customer emails, parses them into inquiries, forwards offers to Telegram for Alex's approval.

> **Currently not deployed in production** — customer inquiries enter via web form or admin dashboard.

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

## JSON Form Attachment Parsing

The kostenloses-angebot web form sends JSON attached to the email. Key field mappings:

| JSON Field | MovingInquiry Field |
|-----------|-------------------|
| `name` | name |
| `email` | email (NOT IMAP sender — that's the company inbox) |
| `phone` | phone |
| `wunschtermin` | preferred_date |
| `auszugsadresse`, `etage-auszug`, `halteverbot-auszug` | departure address/floor/parking ban |
| `einzugsadresse`, `etage-einzug`, `halteverbot-einzug` | arrival address/floor/parking ban |
| `umzugsvolumen-m3` | volume_m3 |
| `gegenstaende-liste` | items_list (VolumeCalculator format) |
| `zusatzleistungen` | services (comma-separated German names) |
| `nachricht` | notes |

## Customer Email Fix

IMAP sender for form submissions is always the company inbox (`umzug@example.com`). After parsing, the processor uses the email from the JSON form data instead — ensures correct customer record.

## State Management

- `inquiries: HashMap<String, MovingInquiry>` — per-customer inquiry data
- `pending_drafts: HashMap<String, PendingDraft>` — awaiting Telegram approval
- `editing_draft: Option<PendingDraft>` — current draft in edit mode

## External Connections

IMAP (polling), SMTP (sending), Telegram Bot API, LLM provider, Calendar service.