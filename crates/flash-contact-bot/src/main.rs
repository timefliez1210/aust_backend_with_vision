//! Flash-contact Telegram sidecar.
//!
//! Polls Telegram for callback_query events from flash-contact reminder messages
//! and handles three actions:
//!   fc_reached:<id>  → mark_handled   (Alex reached the customer)
//!   fc_snooze:<id>   → schedule_snooze (remind again at next slot)
//!   fc_dismiss:<id>  → mark_dismissed  (give up)
//!
//! Uses a dedicated bot token (`AUST__TELEGRAM__FLASH_CONTACT_BOT_TOKEN`),
//! separate from the main email-agent bot, so the two pollers don't fight
//! over `getUpdates` (Telegram allows only one long-poller per token).

mod bot;

use anyhow::Result;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "flash_contact_bot=info".parse().unwrap()),
        )
        .init();

    let db_url = std::env::var("AUST__DATABASE__URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .expect("AUST__DATABASE__URL must be set");

    let bot_token = std::env::var("AUST__TELEGRAM__FLASH_CONTACT_BOT_TOKEN")
        .expect("AUST__TELEGRAM__FLASH_CONTACT_BOT_TOKEN must be set");

    let admin_chat_id: i64 = std::env::var("AUST__TELEGRAM__ADMIN_CHAT_ID")
        .expect("AUST__TELEGRAM__ADMIN_CHAT_ID must be set")
        .parse()
        .expect("AUST__TELEGRAM__ADMIN_CHAT_ID must be an integer");

    let db = sqlx::PgPool::connect(&db_url).await?;

    info!("Flash-contact bot starting — polling Telegram");

    bot::run(db, bot_token, admin_chat_id).await
}
