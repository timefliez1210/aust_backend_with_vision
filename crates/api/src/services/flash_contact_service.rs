//! Background reminder service for flash contacts.
//!
//! Checks the database every few minutes and sends Telegram reminders
//! when the requested callback time window begins.

use aust_core::config::TelegramConfig;
use aust_flash_contact::{fetch_pending_reminders, format_reminder_message, mark_reminder_sent, reminder_time};
use reqwest::Client;
use sqlx::PgPool;
use tracing::{info, warn};

/// Run one reminder check cycle.
///
/// Queries all pending flash contacts, computes whether their requested
/// time window has started, and sends a Telegram ping to Alex if so.
pub async fn run_reminder_check(db: &PgPool, tg_config: &TelegramConfig) -> anyhow::Result<()> {
    let contacts = fetch_pending_reminders(db).await?;
    let now = chrono::Utc::now();
    let client = Client::new();

    for contact in contacts {
        if let Some(remind_at) = reminder_time(&contact, now) {
            if now >= remind_at {
                let message = format_reminder_message(&contact);
                let api_url = format!(
                    "https://api.telegram.org/bot{}/sendMessage",
                    tg_config.bot_token
                );
                let payload = serde_json::json!({
                    "chat_id": tg_config.admin_chat_id,
                    "text": message,
                });

                match client.post(&api_url).json(&payload).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        info!("Flash reminder sent for {}", contact.id);
                        mark_reminder_sent(db, contact.id).await?;
                    }
                    Ok(resp) => {
                        let status = resp.status();
                        let body = resp.text().await.unwrap_or_default();
                        warn!("Telegram reminder failed ({status}): {body}");
                    }
                    Err(e) => {
                        warn!("Failed to send Telegram reminder: {e}");
                    }
                }
            }
        }
    }

    Ok(())
}
