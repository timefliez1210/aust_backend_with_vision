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
    run_reminder_check_with_base(db, tg_config, "https://api.telegram.org").await
}

/// Inner implementation that accepts a configurable Telegram base URL for testing.
pub async fn run_reminder_check_with_base(
    db: &PgPool,
    tg_config: &TelegramConfig,
    tg_base_url: &str,
) -> anyhow::Result<()> {
    let contacts = fetch_pending_reminders(db).await?;
    let now = chrono::Utc::now();
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client builder");

    for contact in contacts {
        if let Some(remind_at) = reminder_time(&contact, now)
            && now >= remind_at {
                let message = format_reminder_message(&contact);
                let api_url = format!(
                    "{}/bot{}/sendMessage",
                    tg_base_url, tg_config.flash_contact_bot_token
                );

                let inline_keyboard = serde_json::json!({
                    "inline_keyboard": [[
                        { "text": "✅ Erreicht",          "callback_data": format!("fc_reached:{}", contact.id) },
                        { "text": "🔁 Nochmal erinnern", "callback_data": format!("fc_snooze:{}", contact.id) },
                        { "text": "🗑 Verwerfen",         "callback_data": format!("fc_dismiss:{}", contact.id) }
                    ]]
                });
                let payload = serde_json::json!({
                    "chat_id": tg_config.admin_chat_id,
                    "text": message,
                    "reply_markup": inline_keyboard,
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aust_core::config::TelegramConfig;
    use aust_flash_contact::{CreateFlashContact, TimePreference};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn test_tg_config() -> TelegramConfig {
        TelegramConfig {
            bot_token: "TEST_BOT_TOKEN".into(),
            admin_chat_id: 0,
            flash_contact_bot_token: "TEST_FLASH_BOT_TOKEN".into(),
        }
    }

    /// Spins up a tiny HTTP server that returns 200 OK for any request and
    /// counts how many times it was called.
    async fn mock_telegram_server() -> (String, Arc<AtomicUsize>) {
        use tokio::net::TcpListener;
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                    use tokio::io::AsyncWriteExt;
                    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\n\r\n{\"ok\":true}\r\n\r\n";
                    let _ = stream.write_all(response).await;
                }
            }
        });
        (format!("http://127.0.0.1:{}", addr.port()), counter)
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn reminder_check_fires_for_active_window_contact(pool: sqlx::PgPool) {
        let (mock_url, call_count) = mock_telegram_server().await;

        // Insert a contact, then force `next_remind_at` into the past so the
        // cron fires regardless of wall-clock time of day (test must be
        // deterministic — earlier version assumed it ran during Vormittag).
        let input = CreateFlashContact {
            name: "Cron Test".into(),
            phone: "01234-test".into(),
            time_preference: TimePreference::Vormittag,
        };
        let contact = aust_flash_contact::insert(&pool, &input).await.unwrap();
        sqlx::query("UPDATE flash_contacts SET next_remind_at = $1 WHERE id = $2")
            .bind(chrono::Utc::now() - chrono::Duration::minutes(5))
            .bind(contact.id)
            .execute(&pool)
            .await
            .unwrap();

        let tg = test_tg_config();
        run_reminder_check_with_base(&pool, &tg, &mock_url).await.unwrap();

        // Verify the row is now marked as reminded.
        let row = sqlx::query(
            "SELECT reminder_sent_at FROM flash_contacts WHERE id = $1",
        )
        .bind(contact.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        use sqlx::Row;
        let reminded_at: Option<chrono::DateTime<chrono::Utc>> = row.try_get("reminder_sent_at").unwrap();
        assert!(reminded_at.is_some(), "reminder_sent_at should be set after cron fires");

        // Telegram mock should have received exactly one call.
        assert_eq!(call_count.load(Ordering::SeqCst), 1);

        // Clean up.
        sqlx::query("DELETE FROM flash_contacts WHERE id = $1")
            .bind(contact.id)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn reminder_check_skips_already_handled(pool: sqlx::PgPool) {
        let (mock_url, call_count) = mock_telegram_server().await;

        let input = CreateFlashContact {
            name: "Handled Test".into(),
            phone: "01234-handled".into(),
            time_preference: TimePreference::Vormittag,
        };
        let contact = aust_flash_contact::insert(&pool, &input).await.unwrap();
        aust_flash_contact::mark_handled(&pool, contact.id).await.unwrap();

        let tg = test_tg_config();
        run_reminder_check_with_base(&pool, &tg, &mock_url).await.unwrap();

        // Telegram should NOT have been called.
        assert_eq!(call_count.load(Ordering::SeqCst), 0);

        sqlx::query("DELETE FROM flash_contacts WHERE id = $1")
            .bind(contact.id)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn reminder_check_skips_any_time(pool: sqlx::PgPool) {
        let (mock_url, call_count) = mock_telegram_server().await;

        let input = CreateFlashContact {
            name: "AnyTime Test".into(),
            phone: "01234-anytime".into(),
            time_preference: TimePreference::Gleich,
        };
        let contact = aust_flash_contact::insert(&pool, &input).await.unwrap();

        let tg = test_tg_config();
        run_reminder_check_with_base(&pool, &tg, &mock_url).await.unwrap();

        assert_eq!(call_count.load(Ordering::SeqCst), 0, "gleich contacts never get a scheduled reminder");

        sqlx::query("DELETE FROM flash_contacts WHERE id = $1")
            .bind(contact.id)
            .execute(&pool)
            .await
            .unwrap();
    }
}
