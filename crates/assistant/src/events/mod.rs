//! Assistant event consumer — polls domain events and dispatches to handlers.

pub mod consumer;
pub mod handlers;
pub mod notifier;

pub use consumer::AssistantEventConsumer;
pub use notifier::{MockNotifier, TelegramNotifier};
