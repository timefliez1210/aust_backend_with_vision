# crates/assistant — In-Telegram Chief-of-Staff Agent

Working subsystem: soul loader, three-layer memory, tool registry, driver
loop, event consumer, retention sweeper, confirmation queue, role-gated
tool dispatch. Wired end-to-end through `crates/api::services::assistant_bridge`
into the `aust-email-agent` Telegram poller.

## Module Map

| Module | Purpose |
|--------|---------|
| `soul.rs` | Loads + validates SOUL.md at startup; exposes parsed sections |
| `llm.rs` | Two-tier LLM routing (Main: kimi-k2.6 / Cheap: deepseek-v4-flash) + MockAssistantLlm |
| `roles.rs` | `Role { Owner, Operator }` + satisfaction helpers |
| `bindings.rs` | Telegram chat_id → (user_id, role) repo |
| `session.rs` | Per-chat rolling turn history with LLM summarisation |
| `audit.rs` | Immutable `agent_actions` log writer |
| `confirmation.rs` | `pending_actions` queue: enqueue, resolve, expire |
| `voice.rs` | `VoiceTranscriber` trait + `NoopTranscriber` (Phase 6) |
| `driver.rs` | Main processing loop: input → LLM → tool calls → reply |
| `memory/durable.rs` | Append-only `agent_memory` CRUD with supersession |
| `memory/episodic.rs` | `agent_episodes` with 768-dim embeddings + similarity retrieval |
| `memory/retrieval.rs` | `assemble_bundle` — pulls all three layers, caps by token budget |
| `tools/mod.rs` | `Tool` trait, `Safety` enum, `ToolRegistry`, schema validation |
| `tools/inquiries.rs` | `GetInquiry` (Read, Operator+) |
| `tools/offers.rs` | `DraftOffer` (Write, Owner) |
| `tools/calendar.rs` | `GetCalendar` (Read, Operator+) |
| `tools/customers.rs` | `GetCustomer` (Read, Operator+) |
| `tools/emails.rs` | `ListInbox` (Read, Owner) |
| `tools/invoices.rs` | `ListInvoices` (Read, Owner) |
| `tools/meta.rs` | `Remember` (Write, Owner) — stores durable memories |
| `hooks/post_action.rs` | Reflection hook: parses MemoryProposal, auto-stores if confidence ≥ 0.7 |
| `hooks/consolidate.rs` | Nightly job: clusters episodes by tag, calls LLM, stores high-confidence patterns |
| `hooks/briefing.rs` | Daily briefing assembler + scheduler (`run_briefing_tick`): auto-posts to the owner chat at 07:00 + 15:00 Europe/Berlin, once per slot/day via the `agent_briefing_log` claim. Driven by a 60s loop in `src/main.rs`. |
| `learning/features.rs` | `OfferFeatures` struct + extractor |
| `learning/observations.rs` | Records offer adjustments to `offer_observations` |
| `learning/predict.rs` | `OfferAdjustmentPredictor` trait + `NullPredictor` + `LinfaPredictor` stub |

## Prompts (`prompts/`)

| File | Purpose |
|------|---------|
| `SOUL.md` | Persona, Hard Rules, Domain Primer, Tone, Escalation (stub; TODO(alex)) |
| `tools_preamble.md` | Tool-calling etiquette injected into every turn |
| `reflection_post_action.md` | Prompt template for post-action hook |
| `consolidation_nightly.md` | Prompt template for nightly consolidation |
| `offer_drafting.md` | Context for offer drafting tool |

## Key Constraints

- `driver.rs` is named `driver` not `loop` — `loop` is a reserved Rust keyword.
- No `unwrap()` in non-test code.
- German for all user-facing strings (tool descriptions, Telegram replies).
- `LinfaPredictor::train` and `predict` are `unimplemented!("Phase 5")`.
- `NoopTranscriber::transcribe` returns `Err(VoiceUnsupported)` — Phase 6 wires real ASR.
- Tools never call `offer_builder` directly (would create circular dep); `DraftOffer` returns a marker JSON.

## Status

| Phase | Status | Description |
|-------|--------|-------------|
| 0 | Done | Foundation — soul, memory, registry, driver |
| 1 | Done | Telegram → `driver::process_turn` via `assistant_bridge` |
| 2 | Done | Real offer drafting + 60+ tools wired through `ServiceBundle` |
| 3 | Done | Confirmation keyboards (`Tool::summarize` + `ctx.confirmed`), event consumer, retention sweepers |
| 4 | Deferred | Embedding-based episode clustering (Ollama Cloud has no embedding model) |
| 5 | TODO | Train LinfaPredictor on offer_observations (min 50 rows) |
| 6 | TODO | WhisperTranscriber — real voice input |

## Known partial wires

- `SendInvoice`, `SendPaymentReminder`, `SendOfferToCustomer`, `SendEmail`,
  `UpdatePricing` return `AssistantError::NotWired` on confirm — they need
  SMTP/S3 plumbed through the bridge to become real actions.
- `apply_nl_override` is rule-based (LLM variant deferred).
- `post_action::reflect` and `hooks::consolidate` are not scheduled — and
  would need to route through `pending_memory_proposals` before being safe
  to schedule (auto-store at confidence ≥ 0.7 currently bypasses B6's
  Confirm gate on `remember`).
- `agent_owns_approval=true` notifies "/approve …" but no `/approve` parser
  and no inline Senden button exist — keep the flag false until B4 ships.
