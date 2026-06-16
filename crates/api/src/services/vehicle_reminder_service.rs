//! Background reminder service for vehicle reminders.
//!
//! Runs on a 60-second tick (spawned in `src/main.rs`) and pings Alex's Telegram
//! chat as a reminder's due date approaches, on an escalating cadence:
//!   - 21 days before → one ping
//!   - 14 days before → one ping
//!   -  7 days before → one ping, then DAILY until the due date
//!   - on/after the due date → DAILY (ÜBERFÄLLIG) until the reminder is marked
//!     done/dismissed (`active = FALSE`)
//!
//! `last_pinged_on` (Europe/Berlin calendar day) dedupes the tick to at most one
//! ping per reminder per day. Pings only go out from 08:00 Berlin onward so Alex
//! gets a morning nudge, not a 00:01 buzz.

use chrono::{NaiveDate, Timelike};
use chrono_tz::Europe::Berlin;
use reqwest::Client;
use sqlx::PgPool;
use tracing::{info, warn};

use aust_core::config::TelegramConfig;

use crate::repositories::vehicle_repo;

/// Hour (Europe/Berlin) before which we stay quiet, so reminders arrive in the morning.
const QUIET_BEFORE_HOUR: u32 = 8;

/// Decide whether a reminder should be pinged on `today`.
///
/// Pure function so the cadence is unit-testable without a clock or DB.
/// Returns `true` at 21 / 14 / 7 days before the due date, every day inside the
/// final week (≤ 7 days), and every day on/after the due date (overdue).
fn should_ping(due_date: NaiveDate, today: NaiveDate) -> bool {
    let days_until = (due_date - today).num_days();
    days_until == 21 || days_until == 14 || days_until <= 7
}

/// Build the German Telegram message for a due reminder.
fn format_message(vehicle_label: &str, reminder_label: &str, due_date: NaiveDate, today: NaiveDate) -> String {
    let days_until = (due_date - today).num_days();
    let when = match days_until {
        d if d > 1 => format!("in {d} Tagen"),
        1 => "morgen".to_string(),
        0 => "heute".to_string(),
        -1 => "⚠️ ÜBERFÄLLIG seit gestern".to_string(),
        d => format!("⚠️ ÜBERFÄLLIG seit {} Tagen", -d),
    };
    format!(
        "🚗 Fahrzeug-Erinnerung\n\n{vehicle_label}: {reminder_label}\nFällig: {} ({when})",
        due_date.format("%d.%m.%Y"),
    )
}

/// Run one reminder check cycle.
pub async fn run_reminder_check(db: &PgPool, tg_config: &TelegramConfig) -> anyhow::Result<()> {
    run_reminder_check_with_base(db, tg_config, "https://api.telegram.org").await
}

/// Inner implementation with a configurable Telegram base URL for testing.
pub async fn run_reminder_check_with_base(
    db: &PgPool,
    tg_config: &TelegramConfig,
    tg_base_url: &str,
) -> anyhow::Result<()> {
    let now_berlin = chrono::Utc::now().with_timezone(&Berlin);
    // Stay quiet overnight — one morning ping is plenty.
    if now_berlin.hour() < QUIET_BEFORE_HOUR {
        return Ok(());
    }
    fire_due_reminders(db, tg_config, tg_base_url, now_berlin.date_naive()).await
}

/// Fire all reminders due on `today`. Split out from the wall-clock gate so the
/// cadence + dedup behaviour can be exercised deterministically in tests.
async fn fire_due_reminders(
    db: &PgPool,
    tg_config: &TelegramConfig,
    tg_base_url: &str,
    today: NaiveDate,
) -> anyhow::Result<()> {
    let reminders = vehicle_repo::fetch_active_reminders(db).await?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client builder");

    for r in reminders {
        // Dedupe: at most one ping per reminder per calendar day.
        if r.last_pinged_on == Some(today) {
            continue;
        }
        if !should_ping(r.due_date, today) {
            continue;
        }

        let message = format_message(&r.vehicle_label, &r.reminder_label, r.due_date, today);
        let api_url = format!("{}/bot{}/sendMessage", tg_base_url, tg_config.bot_token);
        let payload = serde_json::json!({
            "chat_id": tg_config.admin_chat_id,
            "text": message,
        });

        match client.post(&api_url).json(&payload).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!("Vehicle reminder pinged for {}", r.id);
                vehicle_repo::mark_pinged(db, r.id, today).await?;
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                warn!("Telegram vehicle reminder failed ({status}): {body}");
            }
            Err(e) => warn!("Failed to send vehicle reminder: {e}"),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn pings_at_milestones() {
        let today = d(2026, 6, 14);
        // 21, 14, 7 days out → ping
        assert!(should_ping(today + chrono::Duration::days(21), today));
        assert!(should_ping(today + chrono::Duration::days(14), today));
        assert!(should_ping(today + chrono::Duration::days(7), today));
    }

    #[test]
    fn quiet_between_milestones() {
        let today = d(2026, 6, 14);
        // 20, 15, 10, 8 days out → no ping (between milestones)
        assert!(!should_ping(today + chrono::Duration::days(20), today));
        assert!(!should_ping(today + chrono::Duration::days(15), today));
        assert!(!should_ping(today + chrono::Duration::days(10), today));
        assert!(!should_ping(today + chrono::Duration::days(8), today));
    }

    #[test]
    fn daily_in_final_week() {
        let today = d(2026, 6, 14);
        for days in 0..=6 {
            assert!(
                should_ping(today + chrono::Duration::days(days), today),
                "expected ping at {days} days out"
            );
        }
    }

    #[test]
    fn daily_when_overdue() {
        let today = d(2026, 6, 14);
        assert!(should_ping(today - chrono::Duration::days(1), today));
        assert!(should_ping(today - chrono::Duration::days(30), today));
    }

    #[test]
    fn message_marks_overdue() {
        let today = d(2026, 6, 14);
        let msg = format_message("Sprinter", "TÜV", today - chrono::Duration::days(3), today);
        assert!(msg.contains("ÜBERFÄLLIG"));
        assert!(msg.contains("Sprinter"));
        assert!(msg.contains("TÜV"));
    }

    // ── DB-backed: exercises the real repo SQL (insert / fetch / mark / dismiss) ──

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn test_tg_config() -> TelegramConfig {
        TelegramConfig {
            bot_token: "TEST_BOT_TOKEN".into(),
            admin_chat_id: 0,
            flash_contact_bot_token: "TEST_FLASH_BOT_TOKEN".into(),
        }
    }

    /// Tiny HTTP server that 200s every request and counts the hits.
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
                    let response =
                        b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n{\"ok\":true}";
                    let _ = stream.write_all(response).await;
                }
            }
        });
        (format!("http://127.0.0.1:{}", addr.port()), counter)
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn fires_once_per_day_then_dedupes(pool: sqlx::PgPool) {
        let (mock_url, calls) = mock_telegram_server().await;
        let today = chrono::Utc::now().with_timezone(&Berlin).date_naive();

        let vehicle = vehicle_repo::insert_vehicle(&pool, "Sprinter", "HI-AB 123").await.unwrap();
        // Due today → in the daily window.
        vehicle_repo::insert_reminder(&pool, vehicle.id, "TÜV", today)
            .await
            .unwrap();

        let tg = test_tg_config();

        // First run fires and stamps last_pinged_on.
        fire_due_reminders(&pool, &tg, &mock_url, today).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Second run the same day must NOT re-ping (dedup on last_pinged_on).
        fire_due_reminders(&pool, &tg, &mock_url, today).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "should dedupe within a day");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn dismissed_reminder_is_silent(pool: sqlx::PgPool) {
        let (mock_url, calls) = mock_telegram_server().await;
        let today = chrono::Utc::now().with_timezone(&Berlin).date_naive();

        let vehicle = vehicle_repo::insert_vehicle(&pool, "LKW", "HI-CD 456").await.unwrap();
        let reminder = vehicle_repo::insert_reminder(&pool, vehicle.id, "Ölwechsel", today)
            .await
            .unwrap();

        // Mark done → active = false, stops the nag.
        let updated =
            vehicle_repo::update_reminder(&pool, vehicle.id, reminder.id, None, None, Some(false))
                .await
                .unwrap();
        assert!(!updated.active);
        assert!(updated.completed_at.is_some());

        let tg = test_tg_config();
        fire_due_reminders(&pool, &tg, &mock_url, today).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0, "dismissed reminders never ping");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn deleting_vehicle_cascades_reminders(pool: sqlx::PgPool) {
        let today = chrono::Utc::now().with_timezone(&Berlin).date_naive();
        let vehicle = vehicle_repo::insert_vehicle(&pool, "Transporter", "HI-EF 789").await.unwrap();
        vehicle_repo::insert_reminder(&pool, vehicle.id, "TÜV", today)
            .await
            .unwrap();

        let rows = vehicle_repo::delete_vehicle(&pool, vehicle.id).await.unwrap();
        assert_eq!(rows, 1);

        let remaining = vehicle_repo::fetch_active_reminders(&pool).await.unwrap();
        assert!(remaining.is_empty(), "reminders should cascade-delete with the vehicle");
    }
}
