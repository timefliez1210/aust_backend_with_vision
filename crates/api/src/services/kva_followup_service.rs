//! Background follow-up (Nachfassen) service for Kostenvoranschläge.
//!
//! Runs on a 60-second tick (spawned in `src/main.rs`) and pings Alex's Telegram
//! chat about KVAs that have gone quiet while the job is still winnable. A KVA
//! qualifies only when **both** conditions hold:
//!
//!   1. it has been undecided longer than the follow-up threshold
//!      (`settings.kva_followup_days`, default 21 — the observed median time from
//!      KVA to decision), and
//!   2. the `scheduled_date` still lies in the future.
//!
//! The second condition is what keeps this useful. On production 17 of 33 open
//! KVAs have a moving date that has already passed — chasing those cannot earn
//! anything, and pinging about them would train Alex to ignore the channel.
//!
//! Cadence is deliberately slow: one ping when the KVA crosses the threshold,
//! then one every `REPING_INTERVAL_DAYS` while it still qualifies. It stops on
//! its own the moment the move date passes or the inquiry leaves `offer_sent` /
//! `offer_ready`. `followup_last_pinged_on` (Europe/Berlin calendar day) dedupes
//! the tick; `followup_muted` lets Alex silence one KVA.
//!
//! Pings only go out from 08:00 Berlin onward, matching `vehicle_reminder_service`.

use chrono::{NaiveDate, Timelike};
use chrono_tz::Europe::Berlin;
use reqwest::Client;
use sqlx::PgPool;
use tracing::{info, warn};

use aust_core::config::TelegramConfig;

use crate::repositories::{offer_repo, settings_repo};

/// Hour (Europe/Berlin) before which we stay quiet, so nags arrive in the morning.
const QUIET_BEFORE_HOUR: u32 = 8;

/// Days between repeat pings for a KVA that stays quiet and stays winnable.
const REPING_INTERVAL_DAYS: i64 = 7;

/// Decide whether a candidate should be pinged today.
///
/// Pure function so the cadence is unit-testable without a clock or a DB.
/// The candidate has already been filtered on age + future move date by
/// `fetch_followup_candidates`; this only applies the repeat-interval dedupe.
fn should_ping(last_pinged_on: Option<NaiveDate>, today: NaiveDate) -> bool {
    match last_pinged_on {
        None => true,
        Some(last) => (today - last).num_days() >= REPING_INTERVAL_DAYS,
    }
}

/// Format euro cents the German way: `1234567` → `12.345,67 €`.
fn format_eur(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.abs();
    let whole = abs / 100;
    let frac = abs % 100;
    let mut grouped = String::new();
    let digits = whole.to_string();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push('.');
        }
        grouped.push(ch);
    }
    format!("{sign}{grouped},{frac:02} €")
}

/// Build the German Telegram message for one overdue KVA.
fn format_message(
    offer_number: Option<&str>,
    customer_name: Option<&str>,
    price_cents: i64,
    kva_date: NaiveDate,
    scheduled_date: NaiveDate,
    today: NaiveDate,
) -> String {
    let nr = offer_number.unwrap_or("ohne Nummer");
    let kunde = customer_name.unwrap_or("Unbekannter Kunde");
    let quiet_days = (today - kva_date).num_days();
    let days_to_move = (scheduled_date - today).num_days();
    let umzug_when = match days_to_move {
        1 => "morgen".to_string(),
        d => format!("in {d} Tagen"),
    };
    format!(
        "📋 KVA nachfassen\n\n\
         {nr} — {kunde}\n\
         Wert: {}\n\
         KVA vom {} ({quiet_days} Tage ohne Antwort)\n\
         Umzug: {} ({umzug_when})\n\n\
         Der Termin ist noch frei — jetzt anrufen lohnt sich.",
        format_eur(price_cents),
        kva_date.format("%d.%m.%Y"),
        scheduled_date.format("%d.%m.%Y"),
    )
}

/// Run one follow-up check cycle.
pub async fn run_followup_check(db: &PgPool, tg_config: &TelegramConfig) -> anyhow::Result<()> {
    run_followup_check_with_base(db, tg_config, "https://api.telegram.org").await
}

/// Inner implementation with a configurable Telegram base URL for testing.
pub async fn run_followup_check_with_base(
    db: &PgPool,
    tg_config: &TelegramConfig,
    tg_base_url: &str,
) -> anyhow::Result<()> {
    let now_berlin = chrono::Utc::now().with_timezone(&Berlin);
    // Stay quiet overnight — one morning nudge is plenty.
    if now_berlin.hour() < QUIET_BEFORE_HOUR {
        return Ok(());
    }
    fire_due_followups(db, tg_config, tg_base_url, now_berlin.date_naive()).await
}

/// Fire all follow-ups due on `today`. Split out from the wall-clock gate so the
/// cadence + dedup behaviour can be exercised deterministically in tests.
async fn fire_due_followups(
    db: &PgPool,
    tg_config: &TelegramConfig,
    tg_base_url: &str,
    today: NaiveDate,
) -> anyhow::Result<()> {
    let threshold_days = settings_repo::get_kva_followup_days(db).await?;
    let candidates = offer_repo::fetch_followup_candidates(db, today, threshold_days).await?;
    if candidates.is_empty() {
        return Ok(());
    }

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client builder");

    for c in candidates {
        if !should_ping(c.followup_last_pinged_on, today) {
            continue;
        }

        let kva_date = c.created_at.with_timezone(&Berlin).date_naive();
        let message = format_message(
            c.offer_number.as_deref(),
            c.customer_name.as_deref(),
            c.price_cents,
            kva_date,
            c.scheduled_date,
            today,
        );
        let api_url = format!("{}/bot{}/sendMessage", tg_base_url, tg_config.bot_token);
        let payload = serde_json::json!({
            "chat_id": tg_config.admin_chat_id,
            "text": message,
        });

        match client.post(&api_url).json(&payload).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!("KVA follow-up pinged for {}", c.id);
                offer_repo::mark_followup_pinged(db, c.id, today).await?;
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                warn!("Telegram KVA follow-up failed ({status}): {body}");
            }
            Err(e) => warn!("Failed to send KVA follow-up: {e}"),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    // ── pure cadence ──────────────────────────────────────────────────────

    #[test]
    fn pings_a_candidate_that_was_never_pinged() {
        assert!(should_ping(None, d(2026, 8, 23)));
    }

    #[test]
    fn stays_quiet_until_the_repeat_interval_elapses() {
        let today = d(2026, 8, 23);
        for days_ago in 0..REPING_INTERVAL_DAYS {
            let last = today - chrono::Duration::days(days_ago);
            assert!(
                !should_ping(Some(last), today),
                "should stay quiet {days_ago} days after a ping"
            );
        }
    }

    #[test]
    fn repings_once_the_interval_elapsed() {
        let today = d(2026, 8, 23);
        let last = today - chrono::Duration::days(REPING_INTERVAL_DAYS);
        assert!(should_ping(Some(last), today));
        assert!(should_ping(Some(last - chrono::Duration::days(30)), today));
    }

    // ── message ───────────────────────────────────────────────────────────

    #[test]
    fn message_carries_number_customer_value_and_both_dates() {
        let msg = format_message(
            Some("2026-0210"),
            Some("Timo Riechers"),
            100_800,
            d(2026, 6, 25),
            d(2026, 9, 11),
            d(2026, 8, 23),
        );
        assert!(msg.contains("2026-0210"), "{msg}");
        assert!(msg.contains("Timo Riechers"), "{msg}");
        assert!(msg.contains("1.008,00 €"), "{msg}");
        assert!(msg.contains("25.06.2026"), "{msg}");
        assert!(msg.contains("11.09.2026"), "{msg}");
        // 59 days quiet, move 19 days out.
        assert!(msg.contains("59 Tage ohne Antwort"), "{msg}");
        assert!(msg.contains("in 19 Tagen"), "{msg}");
    }

    #[test]
    fn message_tolerates_a_missing_number_and_customer() {
        let msg = format_message(None, None, 50_000, d(2026, 8, 1), d(2026, 9, 1), d(2026, 8, 23));
        assert!(msg.contains("ohne Nummer"), "{msg}");
        assert!(msg.contains("Unbekannter Kunde"), "{msg}");
    }

    #[test]
    fn formats_german_money_with_thousands_separators() {
        assert_eq!(format_eur(0), "0,00 €");
        assert_eq!(format_eur(5), "0,05 €");
        assert_eq!(format_eur(100_800), "1.008,00 €");
        assert_eq!(format_eur(2_605_500), "26.055,00 €");
        assert_eq!(format_eur(-1_50), "-1,50 €");
    }

    // ── DB-backed gating ──────────────────────────────────────────────────

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
                    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n{\"ok\":true}";
                    let _ = stream.write_all(response).await;
                }
            }
        });
        (format!("http://127.0.0.1:{}", addr.port()), counter)
    }

    /// Seed one KVA. `age_days` back-dates the offer; `scheduled_date` may be NULL.
    async fn seed_kva(
        pool: &sqlx::PgPool,
        inquiry_status: &str,
        age_days: i64,
        scheduled_date: Option<NaiveDate>,
    ) -> uuid::Uuid {
        let customer_id = test_helpers::insert_test_customer(pool).await;
        let origin = test_helpers::insert_test_address(
            pool, "Musterstr. 1", "Hildesheim", "31134", None, None,
        )
        .await;
        let dest = test_helpers::insert_test_address(
            pool, "Zielstr. 5", "Hannover", "30159", None, None,
        )
        .await;
        let inquiry_id = test_helpers::insert_test_inquiry_full(
            pool, customer_id, origin, dest, inquiry_status, "termin", None,
        )
        .await;
        sqlx::query("UPDATE inquiries SET scheduled_date = $2 WHERE id = $1")
            .bind(inquiry_id)
            .bind(scheduled_date)
            .execute(pool)
            .await
            .unwrap();

        let offer_id = test_helpers::insert_test_offer(pool, inquiry_id, "draft").await;
        sqlx::query("UPDATE offers SET created_at = NOW() - ($2 || ' days')::interval WHERE id = $1")
            .bind(offer_id)
            .bind(age_days.to_string())
            .execute(pool)
            .await
            .unwrap();
        offer_id
    }

    fn today() -> NaiveDate {
        chrono::Utc::now().with_timezone(&Berlin).date_naive()
    }

    /// The headline rule: both conditions must hold. This is the whole point of the
    /// service — on production 17 of 33 open KVAs have a move date in the past, and
    /// pinging about those would be noise for money that can no longer be earned.
    #[sqlx::test(migrations = "../../migrations")]
    async fn pings_only_when_overdue_and_the_move_is_still_ahead(pool: sqlx::PgPool) {
        let (mock_url, calls) = mock_telegram_server().await;
        let today = today();
        let future = today + chrono::Duration::days(30);
        let past = today - chrono::Duration::days(5);

        // Qualifies: 30 days quiet, move still ahead.
        seed_kva(&pool, "offer_sent", 30, Some(future)).await;
        // Too fresh.
        seed_kva(&pool, "offer_sent", 3, Some(future)).await;
        // Overdue but the move already happened.
        seed_kva(&pool, "offer_sent", 30, Some(past)).await;
        // Overdue, future move — but no longer open.
        seed_kva(&pool, "paid", 30, Some(future)).await;
        seed_kva(&pool, "rejected", 30, Some(future)).await;
        // Overdue, future move, but no move date at all.
        seed_kva(&pool, "offer_sent", 30, None).await;

        fire_due_followups(&pool, &test_tg_config(), &mock_url, today)
            .await
            .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "exactly the one KVA that is overdue AND still winnable should ping"
        );
    }

    /// `offer_ready` is open too — the KVA exists, Alex just has not mailed it.
    #[sqlx::test(migrations = "../../migrations")]
    async fn offer_ready_counts_as_open(pool: sqlx::PgPool) {
        let (mock_url, calls) = mock_telegram_server().await;
        let today = today();
        seed_kva(&pool, "offer_ready", 30, Some(today + chrono::Duration::days(30))).await;

        fire_due_followups(&pool, &test_tg_config(), &mock_url, today)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A move happening *today* is no longer winnable — the boundary is strict.
    #[sqlx::test(migrations = "../../migrations")]
    async fn a_move_today_does_not_qualify(pool: sqlx::PgPool) {
        let (mock_url, calls) = mock_telegram_server().await;
        let today = today();
        seed_kva(&pool, "offer_sent", 30, Some(today)).await;

        fire_due_followups(&pool, &test_tg_config(), &mock_url, today)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// The threshold is exclusive: a KVA exactly at the boundary is not yet overdue.
    #[sqlx::test(migrations = "../../migrations")]
    async fn threshold_boundary_is_exclusive(pool: sqlx::PgPool) {
        let (mock_url, calls) = mock_telegram_server().await;
        let today = today();
        let future = today + chrono::Duration::days(30);

        let threshold = settings_repo::get_kva_followup_days(&pool).await.unwrap();
        assert_eq!(threshold, 21, "migration should seed the default threshold");

        seed_kva(&pool, "offer_sent", threshold, Some(future)).await;
        fire_due_followups(&pool, &test_tg_config(), &mock_url, today)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0, "exactly at the threshold: quiet");

        seed_kva(&pool, "offer_sent", threshold + 1, Some(future)).await;
        fire_due_followups(&pool, &test_tg_config(), &mock_url, today)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one day past: pings");
    }

    /// Lowering the threshold pulls more KVAs onto the list — the setting is live.
    #[sqlx::test(migrations = "../../migrations")]
    async fn threshold_setting_is_honoured(pool: sqlx::PgPool) {
        let (mock_url, calls) = mock_telegram_server().await;
        let today = today();
        seed_kva(&pool, "offer_sent", 10, Some(today + chrono::Duration::days(30))).await;

        fire_due_followups(&pool, &test_tg_config(), &mock_url, today)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0, "10 days is under the default 21");

        settings_repo::set_kva_followup_days(&pool, 7).await.unwrap();
        fire_due_followups(&pool, &test_tg_config(), &mock_url, today)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "threshold lowered to 7 → now overdue");
    }

    /// One ping, then silence until the repeat interval — the nag must not go daily.
    #[sqlx::test(migrations = "../../migrations")]
    async fn dedupes_within_the_repeat_interval(pool: sqlx::PgPool) {
        let (mock_url, calls) = mock_telegram_server().await;
        let today = today();
        seed_kva(&pool, "offer_sent", 30, Some(today + chrono::Duration::days(60))).await;
        let tg = test_tg_config();

        fire_due_followups(&pool, &tg, &mock_url, today).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Same day, and every day up to the interval → silent.
        fire_due_followups(&pool, &tg, &mock_url, today).await.unwrap();
        let day_before_due = today + chrono::Duration::days(REPING_INTERVAL_DAYS - 1);
        fire_due_followups(&pool, &tg, &mock_url, day_before_due).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "must not nag daily");

        let due = today + chrono::Duration::days(REPING_INTERVAL_DAYS);
        fire_due_followups(&pool, &tg, &mock_url, due).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2, "repeats after the interval");
    }

    /// Muting one KVA silences it without touching any other.
    #[sqlx::test(migrations = "../../migrations")]
    async fn muted_kva_never_pings(pool: sqlx::PgPool) {
        let (mock_url, calls) = mock_telegram_server().await;
        let today = today();
        let future = today + chrono::Duration::days(30);

        let muted = seed_kva(&pool, "offer_sent", 30, Some(future)).await;
        seed_kva(&pool, "offer_sent", 30, Some(future)).await;

        assert!(offer_repo::set_followup_muted(&pool, muted, true).await.unwrap());

        fire_due_followups(&pool, &test_tg_config(), &mock_url, today)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "only the unmuted KVA pings");
    }

    /// A superseded draft was replaced before anyone saw it — never chase one.
    #[sqlx::test(migrations = "../../migrations")]
    async fn superseded_kva_never_pings(pool: sqlx::PgPool) {
        let (mock_url, calls) = mock_telegram_server().await;
        let today = today();
        let offer = seed_kva(&pool, "offer_sent", 30, Some(today + chrono::Duration::days(30))).await;
        sqlx::query("UPDATE offers SET status = 'superseded' WHERE id = $1")
            .bind(offer)
            .execute(&pool)
            .await
            .unwrap();

        fire_due_followups(&pool, &test_tg_config(), &mock_url, today)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// A failed Telegram call must not stamp `followup_last_pinged_on`, or the KVA
    /// would silently drop off the list for a whole repeat interval.
    #[sqlx::test(migrations = "../../migrations")]
    async fn a_failed_send_does_not_consume_the_ping(pool: sqlx::PgPool) {
        let today = today();
        seed_kva(&pool, "offer_sent", 30, Some(today + chrono::Duration::days(30))).await;

        // Nothing listening on this port → send fails.
        fire_due_followups(&pool, &test_tg_config(), "http://127.0.0.1:1", today)
            .await
            .unwrap();

        let stamped: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM offers WHERE followup_last_pinged_on IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stamped, 0, "a failed send must stay retryable");
    }
}
