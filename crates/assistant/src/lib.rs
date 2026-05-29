//! `aust-assistant` — in-Telegram chief-of-staff agent for Aust Umzüge.
//!
//! # Architecture
//! - [`soul`] — SOUL.md persona loader (validated at startup).
//! - [`llm`] — two-tier LLM routing (main: kimi-k2.6 / cheap: deepseek-v4-flash).
//! - [`roles`] — Owner / Operator role definitions.
//! - [`bindings`] — Telegram chat → user/role mapping.
//! - [`session`] — per-chat rolling turn history with summarisation.
//! - [`memory`] — three-layer memory: session, durable (facts), episodic (events + embeddings).
//! - [`tools`] — Tool trait, registry, schema validation.
//! - [`hooks`] — post-action reflection, nightly consolidation, morning briefing.
//! - [`learning`] — offer adjustment predictor (Phase 5 training stub).
//! - [`driver`] — main processing loop: receive input → LLM → tools → reply.
//! - [`audit`] — immutable audit log for every tool call.
//! - [`confirmation`] — pending-action queue for write-safety confirmation.
//! - [`voice`] — VoiceTranscriber trait + NoopTranscriber stub (Phase 6).

pub mod audit;
pub mod bindings;
pub mod confirmation;
pub mod retention;
pub mod events;
pub mod driver;
pub mod error;
pub mod hooks;
pub mod learning;
pub mod llm;
pub mod memory;
pub mod roles;
pub mod session;
pub mod soul;
pub mod tools;
pub mod voice;

pub use error::{AssistantError, Result};
pub use events::notifier::{MockNotifier, TelegramNotifier};
pub use llm::{AssistantLlmProvider, ModelTier, OllamaAssistantLlm};
pub use roles::Role;
pub use soul::Soul;
pub use tools::ToolRegistry;
pub use voice::{NoopTranscriber, VoiceTranscriber};
