//! Cross-crate glue between the `aust-assistant` driver and the existing
//! Telegram bot / offer pipeline.
//!
//! # Modules
//! - `notifier_impl` — concrete [`TelegramNotifier`] backed by reqwest.
//! - `telegram_output` — low-level Telegram posting primitives.
//! - `telegram_input` — routes incoming updates to the driver.
//! - `confirm_dispatcher` — posts inline-keyboard messages for pending actions.
//! - `media` — downloads Telegram photos/PDFs and prepares images for the model.

pub mod confirm_dispatcher;
pub mod media;
pub mod notifier_impl;
pub mod telegram_input;
pub mod telegram_output;

pub use notifier_impl::TelegramNotifierImpl;
