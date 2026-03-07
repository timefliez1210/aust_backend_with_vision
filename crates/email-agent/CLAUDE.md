# crates/email-agent — Email Processing & Telegram Approval

> Pipeline map: [../../docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md)
> Recurring bugs (customer stub, Telegram conflict, IMAP sender): [../../docs/DEBUGGING.md](../../docs/DEBUGGING.md)

IMAP polling, email parsing (JSON attachments + text fallback), and human-in-the-loop approval via Telegram.

## Key Files

- `src/processor.rs` - Main orchestrator (EmailProcessor), state machine for drafts/approvals
- `src/parser.rs` - Email/form parsing: JSON attachment → `MovingInquiry`, text fallback
- `src/responder.rs` - LLM-powered response generation and revision
- `src/telegram.rs` - Telegram Bot integration (polling, inline keyboards, calendar commands)
- `src/lib.rs` - Module exports

## Processing Flow

```
1. IMAP poll → fetch unread emails
2. Parse (parser.rs):
   a. Try JSON attachment first (FormSubmission struct, from kostenloses-angebot form)
   b. Fall back to text parsing (extract fields from email body)
3. Merge parsed data into MovingInquiry
4. Fix customer email: use parsed email from form, not IMAP sender (umzug@example.com)
5. If inquiry is complete → forward to offer pipeline via channel
6. LLM enrichment: extract structured data from free text (for direct emails only)
7. Calendar check: is preferred date available?
8. Generate draft: LLM creates German follow-up/confirmation email
9. Send to Telegram: display to Alex with Approve/Edit/Deny buttons
10. Approval loop:
    - Approve → send via SMTP
    - Edit → Alex types instructions → LLM revises → re-send draft
    - Deny → discard
    - Capacity override → if date full, ask Alex to approve overbooking
11. Calendar commands: /kalender, /termine, /kapazitaet
```

## JSON Form Attachment Parsing

The kostenloses-angebot web form sends a JSON file attached to the email. The parser deserializes it into `FormSubmission` and converts to `MovingInquiry`. Key field mappings:

| JSON Field | MovingInquiry Field |
|-----------|-------------------|
| `form-name` | source detection |
| `name` | name |
| `email` | email |
| `phone` | phone |
| `wunschtermin` | preferred_date |
| `auszugsadresse` | departure_address |
| `etage-auszug` | departure_floor |
| `halteverbot-auszug` | departure_parking_ban (`"on"` = true) |
| `einzugsadresse` | arrival_address |
| `etage-einzug` | arrival_floor |
| `halteverbot-einzug` | arrival_parking_ban (`"on"` = true) |
| `umzugsvolumen-m3` | volume_m3 |
| `gegenstaende-liste` | items_list (VolumeCalculator format) |
| `zusatzleistungen` | service flags (comma-separated) |
| `nachricht` | notes |

### Services Parsing

The `zusatzleistungen` field contains comma-separated service names in German:
- "Möbeldemontage" → `service_disassembly = true`
- "Möbelmontage" → `service_assembly = true` (checked after removing "demontage" to avoid conflict)
- "Einpackservice" / "Verpackungsservice" → `service_packing = true`
- "Einlagerung" → `service_storage = true`
- "Entsorgung" → `service_disposal = true`

## Customer Email Fix

The IMAP sender for form submissions is always `umzug@example.com` (the company inbox). After `merge_inquiry()`, the processor checks if the parsed email (from JSON form data) differs from the IMAP sender and uses the parsed one. This ensures the customer record in DB has the correct email for offer delivery.

## State Management

EmailProcessor maintains per-session state:

- `inquiries: HashMap<String, MovingInquiry>` — per-customer inquiry data
- `pending_drafts: HashMap<String, PendingDraft>` — awaiting Telegram approval
- `editing_draft: Option<PendingDraft>` — current draft in edit mode
- `pending_capacity: HashMap<String, PendingCapacityRequest>` — awaiting capacity decision

## Telegram Integration

- Sends formatted draft messages with inline keyboard buttons
- Polls `/getUpdates` for button presses and text replies
- Calendar commands for schedule management (German UI)
- `ApprovalDecision` enum: Approve, Deny, AwaitingEditInstructions, EditInstructions(String), CalendarCommand, OfferApprove, OfferEdit, OfferDeny, OfferEditText, InquiryComplete

## Key Methods

| Method | Description |
|--------|-------------|
| `process_cycle()` | One poll iteration: fetch emails + check Telegram |
| `run(interval)` | Main loop with sleep between cycles |
| `process_incoming_email(email)` | Full pipeline for one email |
| `generate_response(inquiry, body, availability)` | LLM draft generation |
| `revise_draft(draft, instructions, subject)` | Edit loop via LLM |
| `extract_data_from_text(inquiry, body)` | Structured data extraction |

## External Connections

- IMAP server (polling)
- SMTP server (sending)
- Telegram Bot API (approval workflow)
- LLM provider (response generation, data extraction)
- Calendar service (availability checks, force-booking)

## Configuration

Uses `EmailConfig` (IMAP/SMTP details) + `TelegramConfig` (bot_token, admin_chat_id).

## Language Note

All customer-facing emails and Telegram messages are in German.
