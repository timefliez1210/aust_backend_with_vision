//! Three-layer memory system for the assistant.
//!
//! - [`session_mem`] — rolling per-session context (in `agent_sessions.turns`)
//! - [`durable`]    — append-only structured facts (`agent_memory`)
//! - [`episodic`]   — what Josie did and how Alex reacted (`agent_episodes`), hybrid recall
//! - [`embedder`]   — optional in-process sentence embeddings for episodic recall
//! - [`retrieval`]  — bundle assembler: pulls from all three layers for prompt context

pub mod durable;
pub mod embedder;
pub mod episodic;
pub mod proposals;
pub mod retrieval;
pub mod session_mem;

pub use durable::{DurableMemory, MemoryKind};
pub use episodic::Episode;
pub use retrieval::MemoryBundle;
