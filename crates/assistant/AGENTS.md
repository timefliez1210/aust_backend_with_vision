# crates/assistant — In-Telegram Chief-of-Staff Agent

Phase 0 foundation: soul loader, three-layer memory, tool registry, driver loop, learning skeleton.
All subsystems are present as working stubs so later phases can fill in tools/learners without architectural changes.

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
| `hooks/briefing.rs` | Morning briefing assembler (calendar, invoices, offers, emails) |
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

## Phases

| Phase | Status | Description |
|-------|--------|-------------|
| 0 | Done | Foundation — all stubs wired, 36 tests green |
| 1 | TODO | Wire Telegram bot update handler to `driver::process_turn` |
| 2 | TODO | Real offer drafting via injected service trait |
| 3 | TODO | Telegram confirmation keyboards, nightly consolidation scheduler |
| 4 | TODO | Embedding-based episode clustering |
| 5 | TODO | Train LinfaPredictor on offer_observations (min 50 rows) |
| 6 | TODO | WhisperTranscriber — real voice input |
