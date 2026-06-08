//! Reminder tick — reconciles auto-reminders and fires what's due.
//!
//! Driven on a short interval by a `tokio::spawn` loop in `src/main.rs`. Each tick:
//!
//! 1. **Reconcile** the email nag: every unhandled inbound email gets exactly one
//!    active recurring reminder; reminders whose email is no longer unhandled are
//!    deactivated (auto-cancel — works no matter where the email was handled).
//! 2. **Fire** every active reminder whose `due_at` has passed: push it to its chat
//!    via the notifier, then deactivate (one-shot) or advance `due_at` (recurring).
//!
//! Recurring reminders only fire within business hours (07:00–20:00 Europe/Berlin);
//! outside that window they are silently pushed to the next opening so Alex isn't
//! pinged overnight. One-shot reminders fire at their exact time regardless.

use chrono::{DateTime, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Europe::Berlin;
use sqlx::PgPool;
use tracing::{info, warn};

use crate::error::Result;
use crate::events::notifier::TelegramNotifier;

const OPEN_HOUR: u32 = 7;
const CLOSE_HOUR: u32 = 20;
const RECUR_HOURS: i64 = 3;

/// Run one reminder tick: reconcile the email nag, then fire due reminders.
pub async fn run_reminder_tick(pool: &PgPool, notifier: &dyn TelegramNotifier) -> Result<()> {
    if let Err(e) = reconcile_email_reminders(pool).await {
        warn!("Reminder reconcile failed: {e}");
    }
    fire_due_reminders(pool, notifier).await
}

/// Ensure there's exactly one active recurring reminder per unhandled inbound
/// email, and deactivate reminders whose email has since been handled.
async fn reconcile_email_reminders(pool: &PgPool) -> Result<()> {
    // Auto-cancel: email no longer unhandled → turn its reminder off.
    sqlx::query(
        r#"
        UPDATE agent_reminders SET active = FALSE
        WHERE source = 'email' AND active
          AND NOT EXISTS (
              SELECT 1 FROM email_messages m
              WHERE m.id = agent_reminders.source_ref
                AND m.direction = 'inbound'
                AND m.status = 'received'
          )
        "#,
    )
    .execute(pool)
    .await?;

    // Auto-create: only nag if there is an owner chat to nag.
    let owner: Option<(i64,)> =
        sqlx::query_as("SELECT chat_id FROM telegram_chat_bindings WHERE role = 'owner' LIMIT 1")
            .fetch_optional(pool)
            .await?;
    let Some((owner_chat,)) = owner else {
        return Ok(());
    };

    // The partial unique index (source, source_ref) WHERE active guards races.
    sqlx::query(
        r#"
        INSERT INTO agent_reminders (chat_id, text, due_at, recurrence, source, source_ref)
        SELECT $1,
               'Unbeantwortete E-Mail: ' || COALESCE(NULLIF(m.subject, ''), '(kein Betreff)')
                   || CASE WHEN m.from_address IS NOT NULL AND m.from_address <> ''
                           THEN ' — von ' || m.from_address ELSE '' END,
               NOW(), 'recurring', 'email', m.id
        FROM email_messages m
        WHERE m.direction = 'inbound' AND m.status = 'received'
          AND NOT EXISTS (
              SELECT 1 FROM agent_reminders r
              WHERE r.source = 'email' AND r.source_ref = m.id AND r.active
          )
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(owner_chat)
    .execute(pool)
    .await?;

    Ok(())
}

/// Fire every active reminder whose `due_at` has passed.
async fn fire_due_reminders(pool: &PgPool, notifier: &dyn TelegramNotifier) -> Result<()> {
    let due: Vec<(uuid::Uuid, i64, String, String)> = sqlx::query_as(
        "SELECT id, chat_id, text, recurrence FROM agent_reminders \
         WHERE active AND due_at <= NOW() ORDER BY due_at ASC LIMIT 50",
    )
    .fetch_all(pool)
    .await?;

    let now = Utc::now();

    for (id, chat_id, text, recurrence) in due {
        let recurring = recurrence == "recurring";

        // Recurring reminder that came due outside business hours: snap it to the
        // next opening and stay silent.
        if recurring && !in_business_hours(now) {
            let next = next_open_window(now);
            let _ = sqlx::query("UPDATE agent_reminders SET due_at = $2 WHERE id = $1")
                .bind(id)
                .bind(next)
                .execute(pool)
                .await;
            continue;
        }

        let body = format!("⏰ Erinnerung: {text}");
        if let Err(e) = notifier.post(chat_id, body).await {
            // Leave it due so the next tick retries rather than dropping the ping.
            warn!(reminder = %id, "Reminder notify failed, will retry: {e}");
            continue;
        }

        let update = if recurring {
            sqlx::query(
                "UPDATE agent_reminders \
                 SET due_at = $2, last_fired_at = NOW(), fired_count = fired_count + 1 \
                 WHERE id = $1",
            )
            .bind(id)
            .bind(next_recurring_due(now))
        } else {
            sqlx::query(
                "UPDATE agent_reminders \
                 SET active = FALSE, last_fired_at = NOW(), fired_count = fired_count + 1 \
                 WHERE id = $1",
            )
            .bind(id)
        };
        if let Err(e) = update.execute(pool).await {
            warn!(reminder = %id, "Reminder post-fire update failed: {e}");
        } else {
            info!(reminder = %id, recurring, "Reminder fired");
        }
    }

    Ok(())
}

/// True when `now` falls within the Europe/Berlin business window.
fn in_business_hours(now: DateTime<Utc>) -> bool {
    let h = now.with_timezone(&Berlin).hour();
    (OPEN_HOUR..CLOSE_HOUR).contains(&h)
}

/// Europe/Berlin 07:00 on `date`, as a UTC instant.
fn berlin_open(date: NaiveDate) -> DateTime<Utc> {
    let naive = date.and_hms_opt(OPEN_HOUR, 0, 0).expect("valid local time");
    Berlin
        .from_local_datetime(&naive)
        .single()
        .or_else(|| Berlin.from_local_datetime(&naive).earliest())
        .expect("Berlin 07:00 resolvable")
        .with_timezone(&Utc)
}

/// The next instant inside the business window at or after `after`.
fn next_open_window(after: DateTime<Utc>) -> DateTime<Utc> {
    let b = after.with_timezone(&Berlin);
    let h = b.hour();
    if h < OPEN_HOUR {
        berlin_open(b.date_naive())
    } else if h >= CLOSE_HOUR {
        berlin_open(b.date_naive() + chrono::Duration::days(1))
    } else {
        after
    }
}

/// Next fire time for a recurring reminder: ~3h later, snapped into the window.
fn next_recurring_due(now: DateTime<Utc>) -> DateTime<Utc> {
    next_open_window(now + chrono::Duration::hours(RECUR_HOURS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// A recurring reminder's next due time is always inside business hours.
    #[test]
    fn next_recurring_due_lands_in_business_hours() {
        // 19:00 Berlin → +3h would be 22:00 → must snap into the window.
        let late = Berlin
            .with_ymd_and_hms(2026, 6, 14, 19, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        assert!(in_business_hours(next_recurring_due(late)));

        // 10:00 Berlin → +3h = 13:00, still in window.
        let midday = Berlin
            .with_ymd_and_hms(2026, 6, 14, 10, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let next = next_recurring_due(midday);
        assert!(in_business_hours(next));
        assert!(next > midday);

        // 03:00 Berlin (overnight) → next_open_window snaps to 07:00 same day.
        let overnight = Berlin
            .with_ymd_and_hms(2026, 6, 14, 3, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        assert!(in_business_hours(next_open_window(overnight)));
    }
}
