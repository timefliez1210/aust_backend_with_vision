# crates/email-agent — Email Processing & Telegram Approval

IMAP polling, LLM-powered email parsing and response generation, and human-in-the-loop approval via Telegram.

## Key Files

- `src/processor.rs` - Main orchestrator (EmailProcessor), state machine for drafts/approvals
- `src/responder.rs` - LLM-powered response generation and revision
- `src/telegram.rs` - Telegram Bot integration (polling, inline keyboards, calendar commands)
- `src/lib.rs` - Module exports

## Processing Flow

```
1. IMAP poll → fetch unread emails
2. Parse: extract name, phone, addresses, dates, volume hints
3. LLM enrichment: extract structured data from free text
4. Calendar check: is preferred date available?
5. Generate draft: LLM creates German follow-up/confirmation email
6. Send to Telegram: display to Alex with Approve/Edit/Deny buttons
7. Approval loop:
   - Approve → send via SMTP
   - Edit → Alex types instructions → LLM revises → re-send draft
   - Deny → discard
   - Capacity override → if date full, ask Alex to approve overbooking
8. Calendar commands: /kalender, /termine, /kapazitaet
```

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
- `ApprovalDecision` enum: Approve, Deny, AwaitingEditInstructions, EditInstructions(String), CalendarCommand, etc.

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
