# crates/assistant — In-Telegram Chief-of-Staff Agent

Working subsystem: soul loader, three-layer memory, tool registry, driver
loop, event consumer, retention sweeper, confirmation queue, role-gated
tool dispatch. Wired end-to-end through `crates/api::services::assistant_bridge`
into the `aust-email-agent` Telegram poller.

## Module Map

| Module | Purpose |
|--------|---------|
| `soul.rs` | Loads + validates SOUL.md at startup; exposes parsed sections |
| `llm.rs` | Two-tier LLM routing (Main: `kimi-k2.6` / Cheap: `deepseek-v4-flash`), both **hardcoded** in `model_name()` — see Key Constraints |
| `roles.rs` | `Role { Owner, Operator }` + satisfaction helpers |
| `bindings.rs` | Telegram chat_id → (user_id, role) repo |
| `session.rs` | Per-chat rolling turn history with LLM summarisation |
| `audit.rs` | Immutable `agent_actions` log writer |
| `confirmation.rs` | `pending_actions` queue: enqueue, resolve, expire |
| `retention.rs` | GC sweepers for assistant-owned tables (`agent_actions`, sessions, episodes, …), independent per table, driven every 6h from `src/main.rs`; returns `(deleted, summarized)` counts |
| `voice.rs` | `VoiceTranscriber` trait + `NoopTranscriber` (Phase 6) |
| `driver.rs` | Main processing loop: input → LLM → tool calls → reply. Also home of the grounding guard that blocks any reply UUID not backed by a real tool result (prevents fabricated IDs) |
| `events/consumer.rs` | `AssistantEventConsumer` — polls `domain_events` (via `aust_core::events`) and dispatches by kind |
| `events/handlers.rs` | Per-event-kind handlers, each given a `TelegramNotifier` to post to Alex without a dependency on `crates/api` |
| `events/notifier.rs` | `TelegramNotifier` trait + `MockNotifier` |
| `memory/durable.rs` | Append-only `agent_memory` CRUD with supersession |
| `memory/episodic.rs` | `agent_episodes` with 768-dim embeddings + similarity retrieval |
| `memory/retrieval.rs` | `assemble_bundle` — pulls all three layers, caps by token budget |
| `memory/proposals.rs` | `pending_memory_proposals` — low-confidence memory proposals (reflection hook <0.7, nightly consolidation <0.8) queued for Alex's batch approval via Telegram; append-only, `approve` inserts into `agent_memory` |
| `memory/session_mem.rs` | Session-scoped memory helpers backing `session.rs` |
| `tools/mod.rs` | `Tool` trait, `Safety` enum, `ToolRegistry`, schema validation |
| `tools/inquiries.rs`, `offers.rs`, `calendar.rs`, `customers.rs`, `emails.rs`, `invoices.rs`, `employees.rs`, `estimates.rs`, `addresses.rs`, `reminders.rs`, `reviews.rs`, `settings.rs`, `meta.rs` | ~88 tools total (see Tools below); one file per business domain, matching the `ServiceBundle` traits in `aust-core` |
| `tools/testing.rs` | `cfg(test)`-only mock implementation of every `aust_core::services` trait + `mock_bundle()` factory, used across the tool unit tests |
| `hooks/post_action.rs` | Reflection hook: parses MemoryProposal, auto-stores if confidence ≥ 0.7 |
| `hooks/consolidate.rs` | Nightly job: clusters episodes by tag, calls LLM, stores high-confidence patterns |
| `hooks/briefing.rs` | Daily briefing assembler + scheduler (`run_briefing_tick`): auto-posts to the owner chat at 07:00 + 15:00 Europe/Berlin, once per slot/day via the `agent_briefing_log` claim. Driven by a 60s loop in `src/main.rs`. |
| `hooks/reminders.rs` | Reminder tick: reconciles auto-nags (email unanswered, invoice dunning, review requests — each "open row ⇒ exactly one active recurring reminder" until it closes) and fires due ones. Short interval, `tokio::spawn` loop in `src/main.rs` |
| `learning/features.rs` | `OfferFeatures` struct + extractor |
| `learning/observations.rs` | Records offer adjustments to `offer_observations` |
| `learning/predict.rs` | `OfferAdjustmentPredictor` trait + `NullPredictor` + `LinfaPredictor` stub |

## Tools

88 tools across 13 domain files (counted via `impl Tool for`): `calendar.rs` (13),
`inquiries.rs`/`offers.rs`/`invoices.rs` (10 each), `meta.rs` (8), `customers.rs`/`emails.rs`/`reviews.rs`
(7 each), `employees.rs` (5), `reminders.rs`/`estimates.rs`/`settings.rs` (3 each), `addresses.rs` (2).
Every tool declares a `Safety` (Read/Write) and minimum `Role`; the `ToolRegistry` validates
arguments against each tool's JSON Schema before dispatch. Tools never call `offer_builder`
directly (would create a circular dependency); `DraftOffer`/`CommitOfferDraft` go through the
`OfferService` trait instead and return a marker JSON.

## Prompts (`prompts/`)

| File | Purpose |
|------|---------|
| `SOUL.md` | Persona (Josie), Hard Rules (no unconfirmed writes, no deletes, no fabricated data/IDs — must call the real tool, `create_feedback` on real defects), Domain Primer, Tone, Escalation. Has real content; three `TODO(alex)` markers remain for additional business rules, typical order sizes/seasonality, and specific escalation paths |
| `tools_preamble.md` | Tool-calling etiquette injected into every turn |
| `reflection_post_action.md` | Prompt template for post-action hook |
| `consolidation_nightly.md` | Prompt template for nightly consolidation |
| `offer_drafting.md` | Context for offer drafting tool |

## Key Constraints

- `driver.rs` is named `driver` not `loop` — `loop` is a reserved Rust keyword.
- No `unwrap()` in non-test code.
- German for all user-facing strings (tool descriptions, Telegram replies).
- **The chat model is hardcoded, not config-driven.** `llm.rs::OllamaAssistantLlm::model_name()`
  returns a fixed `"kimi-k2.6"`/`"deepseek-v4-flash"` per `ModelTier`. `CompanyConfig`'s sibling
  `LlmConfig::ollama.model` field (env var `AUST__LLM__OLLAMA__MODEL`) looks like it should
  control this — it does not; that field feeds a *different* generic Ollama provider elsewhere
  in the codebase (e.g. vision), not Josie. Changing Josie's model means editing `model_name()`.
- `LinfaPredictor::train` and `predict` are `unimplemented!("Phase 5")`.
- `NoopTranscriber::transcribe` returns `Err(VoiceUnsupported)` — Phase 6 wires real ASR.
- Tools never call `offer_builder` directly (would create circular dep); `DraftOffer`/`CommitOfferDraft` return a marker JSON via the `OfferService` trait instead.

## Testing

No dedicated `TEST_DATABASE_URL` convention in this crate. Two patterns coexist:

- `#[sqlx::test(migrations = "../../migrations")]` (used throughout `retention.rs`) — sqlx
  spins up and migrates a scratch DB per test automatically; just needs `DATABASE_URL` set
  to a reachable Postgres server for sqlx's test harness to provision against.
- Manual `try_pool()`-style helpers (`confirmation.rs`, `events/consumer.rs`, `hooks/reminders.rs`)
  read `DATABASE_URL` directly and **silently skip the test** (`return`/`None`) if it's unset —
  so these pass trivially in an environment with no DB configured, which can mask a real
  regression. Don't assume a green run here means the DB path was exercised; check for `DATABASE_URL`.
- Tool-level unit tests use `tools::testing::mock_bundle()` (mocks every `ServiceBundle` trait) and need no DB at all.

## Status

| Phase | Status | Description |
|-------|--------|-------------|
| 0 | Done | Foundation — soul, memory, registry, driver |
| 1 | Done | Telegram → `driver::process_turn` via `assistant_bridge` |
| 2 | Done | Real offer drafting + 88 tools wired through `ServiceBundle` |
| 3 | Done | Confirmation keyboards (`Tool::summarize` + `ctx.confirmed`), event consumer, retention sweepers |
| 4 | Deferred | Embedding-based episode clustering (Ollama Cloud has no embedding model) |
| 5 | TODO | Train LinfaPredictor on offer_observations (min 50 rows) |
| 6 | TODO | WhisperTranscriber — real voice input |

## Known partial wires

- `SendInvoice`, `SendOfferToCustomer`, `UpdatePricing` return `AssistantError::NotWired`
  on confirm — the PDF-send pipeline (S3 fetch + SMTP attach) is plumbed through the legacy
  route handler, not yet exposed via `InvoiceService`/`OfferService`, and `SettingsService::update_pricing`
  has no implementation yet. `SendEmail` and `SendPaymentReminder` are now fully wired
  (`EmailService::send`, `InvoiceService::send_dunning`) — they were NotWired stubs when this
  file was last written and have since shipped.
- `apply_nl_override` is rule-based (LLM variant deferred).
- `post_action::reflect` and `hooks::consolidate` are not scheduled — and
  would need to route through `pending_memory_proposals` before being safe
  to schedule (auto-store at confidence ≥ 0.7 currently bypasses B6's
  Confirm gate on `remember`).
- `agent_owns_approval=true` (`events/handlers.rs::handle_offer_ready`) used to tell
  Alex to tap "/approve <id>"/"/deny <id>" even though no such command parser or
  inline button existed — **fixed (B4)**: the notification now honestly hands off
  to the admin panel instead of a phantom command. The underlying send itself is
  still not wired (see `SendOfferToCustomer` above) — only the message text was fixed.
