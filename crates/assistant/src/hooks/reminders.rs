//! Reminder tick — reconciles auto-reminders and fires what's due.
//!
//! Driven on a short interval by a `tokio::spawn` loop in `src/main.rs`. Each tick:
//!
//! 1. **Reconcile** the auto-nags. Three sources, all the same shape: some row in
//!    the business schema is "open", and while it stays open exactly one active
//!    recurring reminder points at it. When it closes — wherever it was closed, by
//!    whoever — the reminder deactivates on the next tick. Nothing has to remember
//!    to cancel anything.
//!      * `email`   — an inbound email nobody has answered
//!      * `invoice` — a sent invoice that's due for a Zahlungserinnerung or Mahnung
//!      * `review`  — a deferred Bewertungsanfrage whose date has arrived
//! 2. **Fire** every active reminder whose `due_at` has passed: push it to its chat
//!    via the notifier, then deactivate (one-shot) or advance `due_at` (recurring).
//!
//! Recurring reminders only fire within business hours (07:00–20:00 Europe/Berlin);
//! outside that window they are silently pushed to the next opening so Alex isn't
//! pinged overnight. One-shot reminders fire at their exact time regardless.
//!
//! Cadence is per-reminder (`recur_hours`). The email nag stays at 3h — an unanswered
//! email is usually urgent. Money and reviews nag once a day: a Mahnung that pings
//! every three hours is noise, and Alex would learn to ignore it.

use chrono::{DateTime, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Europe::Berlin;
use sqlx::PgPool;
use tracing::{info, warn};

use crate::error::Result;
use crate::events::notifier::TelegramNotifier;

const OPEN_HOUR: u32 = 7;
const CLOSE_HOUR: u32 = 20;

/// Cadence for the email nag — unanswered mail is time-critical.
const EMAIL_RECUR_HOURS: i32 = 3;
/// Cadence for money and reviews. Daily; anything tighter trains Alex to ignore it.
const BILLING_RECUR_HOURS: i32 = 24;

/// Run one reminder tick: reconcile the auto-nags, then fire due reminders.
pub async fn run_reminder_tick(pool: &PgPool, notifier: &dyn TelegramNotifier) -> Result<()> {
    // Each reconcile is independent: a failure in one must not starve the others,
    // and none of them should stop reminders that are already due from firing.
    if let Err(e) = reconcile_email_reminders(pool).await {
        warn!("Email reminder reconcile failed: {e}");
    }
    if let Err(e) = reconcile_invoice_reminders(pool).await {
        warn!("Invoice reminder reconcile failed: {e}");
    }
    if let Err(e) = reconcile_review_reminders(pool).await {
        warn!("Review reminder reconcile failed: {e}");
    }
    fire_due_reminders(pool, notifier).await
}

/// The owner's chat, or `None` when no owner is bound yet — nobody to nag.
async fn owner_chat(pool: &PgPool) -> Result<Option<i64>> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT chat_id FROM telegram_chat_bindings WHERE role = 'owner' LIMIT 1")
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(c,)| c))
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
    let Some(chat) = owner_chat(pool).await? else {
        return Ok(());
    };

    // The partial unique index (source, source_ref) WHERE active guards races.
    sqlx::query(
        r#"
        INSERT INTO agent_reminders (chat_id, text, due_at, recurrence, recur_hours, source, source_ref)
        SELECT $1,
               'Unbeantwortete E-Mail: ' || COALESCE(NULLIF(m.subject, ''), '(kein Betreff)')
                   || CASE WHEN m.from_address IS NOT NULL AND m.from_address <> ''
                           THEN ' — von ' || m.from_address ELSE '' END,
               NOW(), 'recurring', $2, 'email', m.id
        FROM email_messages m
        WHERE m.direction = 'inbound' AND m.status = 'received'
          AND NOT EXISTS (
              SELECT 1 FROM agent_reminders r
              WHERE r.source = 'email' AND r.source_ref = m.id AND r.active
          )
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(chat)
    .bind(EMAIL_RECUR_HOURS)
    .execute(pool)
    .await?;

    Ok(())
}

/// One active nag per dunning step that has come due, auto-cancelled on payment.
///
/// `source_ref` is the `invoice_reminders.id`, not the invoice — so when a step is
/// sent and the ladder advances to the next level, the *same* row stays the anchor
/// and the nag simply falls quiet until that next level comes due. Marking the
/// invoice paid closes the row and the nag dies with it.
async fn reconcile_invoice_reminders(pool: &PgPool) -> Result<()> {
    // Auto-cancel: the step was sent, snoozed, closed, or the invoice got paid.
    sqlx::query(
        r#"
        UPDATE agent_reminders SET active = FALSE
        WHERE source = 'invoice' AND active
          AND NOT EXISTS (
              SELECT 1 FROM invoice_reminders ir
              JOIN invoices i ON i.id = ir.invoice_id
              WHERE ir.id = agent_reminders.source_ref
                AND ir.status = 'pending'
                AND ir.remind_after <= CURRENT_DATE
                AND i.status = 'sent'
          )
        "#,
    )
    .execute(pool)
    .await?;

    let Some(chat) = owner_chat(pool).await? else {
        return Ok(());
    };

    // Mirrors billing_reminder_service::dunning_label — kept in SQL because this
    // runs as a set-based reconcile, not a row-by-row fetch.
    sqlx::query(
        r#"
        INSERT INTO agent_reminders (chat_id, text, due_at, recurrence, recur_hours, source, source_ref)
        SELECT $1,
               CASE ir.level WHEN 1 THEN 'Zahlungserinnerung'
                             WHEN 2 THEN '1. Mahnung'
                             ELSE '2. Mahnung' END
                   || ' fällig: Rechnung ' || i.invoice_number
                   || COALESCE(' — ' || c.name, '')
                   || ' (offen seit ' || to_char(ir.remind_after, 'DD.MM.') || ')',
               NOW(), 'recurring', $2, 'invoice', ir.id
        FROM invoice_reminders ir
        JOIN invoices   i   ON i.id  = ir.invoice_id
        JOIN inquiries  inq ON inq.id = i.inquiry_id
        LEFT JOIN customers c ON c.id = inq.customer_id
        WHERE ir.status = 'pending'
          AND ir.remind_after <= CURRENT_DATE
          AND i.status = 'sent'
          AND NOT EXISTS (
              SELECT 1 FROM agent_reminders r
              WHERE r.source = 'invoice' AND r.source_ref = ir.id AND r.active
          )
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(chat)
    .bind(BILLING_RECUR_HOURS)
    .execute(pool)
    .await?;

    Ok(())
}

/// One active nag per review request whose deferral has run out.
///
/// `source_ref` is the inquiry: `review_requests` is one row per inquiry, and the
/// nag dies as soon as its status leaves `pending` (sent or skipped).
async fn reconcile_review_reminders(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE agent_reminders SET active = FALSE
        WHERE source = 'review' AND active
          AND NOT EXISTS (
              SELECT 1 FROM review_requests rr
              WHERE rr.inquiry_id = agent_reminders.source_ref
                AND rr.status = 'pending'
                AND rr.remind_after IS NOT NULL
                AND rr.remind_after <= CURRENT_DATE
          )
        "#,
    )
    .execute(pool)
    .await?;

    let Some(chat) = owner_chat(pool).await? else {
        return Ok(());
    };

    sqlx::query(
        r#"
        INSERT INTO agent_reminders (chat_id, text, due_at, recurrence, recur_hours, source, source_ref)
        SELECT $1,
               'Bewertungsanfrage fällig' || COALESCE(': ' || c.name, '')
                   || ' — Google-Rezension anfragen?',
               NOW(), 'recurring', $2, 'review', rr.inquiry_id
        FROM review_requests rr
        JOIN inquiries i ON i.id = rr.inquiry_id
        LEFT JOIN customers c ON c.id = i.customer_id
        WHERE rr.status = 'pending'
          AND rr.remind_after IS NOT NULL
          AND rr.remind_after <= CURRENT_DATE
          AND NOT EXISTS (
              SELECT 1 FROM agent_reminders r
              WHERE r.source = 'review' AND r.source_ref = rr.inquiry_id AND r.active
          )
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(chat)
    .bind(BILLING_RECUR_HOURS)
    .execute(pool)
    .await?;

    Ok(())
}

/// Fire every active reminder whose `due_at` has passed.
async fn fire_due_reminders(pool: &PgPool, notifier: &dyn TelegramNotifier) -> Result<()> {
    let due: Vec<(uuid::Uuid, i64, String, String, i32)> = sqlx::query_as(
        "SELECT id, chat_id, text, recurrence, recur_hours FROM agent_reminders \
         WHERE active AND due_at <= NOW() ORDER BY due_at ASC LIMIT 50",
    )
    .fetch_all(pool)
    .await?;

    let now = Utc::now();

    for (id, chat_id, text, recurrence, recur_hours) in due {
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
            .bind(next_recurring_due(now, recur_hours))
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

/// Next fire time for a recurring reminder: `recur_hours` later, snapped into the
/// business window. A 24h cadence therefore lands at the same hour the next day,
/// or at 07:00 if that hour falls outside the window.
fn next_recurring_due(now: DateTime<Utc>, recur_hours: i32) -> DateTime<Utc> {
    next_open_window(now + chrono::Duration::hours(recur_hours.max(1) as i64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn berlin(h: u32) -> DateTime<Utc> {
        Berlin
            .with_ymd_and_hms(2026, 6, 14, h, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc)
    }

    /// A recurring reminder's next due time is always inside business hours.
    #[test]
    fn next_recurring_due_lands_in_business_hours() {
        // 19:00 Berlin → +3h would be 22:00 → must snap into the window.
        assert!(in_business_hours(next_recurring_due(berlin(19), EMAIL_RECUR_HOURS)));

        // 10:00 Berlin → +3h = 13:00, still in window.
        let midday = berlin(10);
        let next = next_recurring_due(midday, EMAIL_RECUR_HOURS);
        assert!(in_business_hours(next));
        assert!(next > midday);

        // 03:00 Berlin (overnight) → next_open_window snaps to 07:00 same day.
        assert!(in_business_hours(next_open_window(berlin(3))));
    }

    /// The daily cadence used for money and reviews nags roughly once a day, and
    /// still never lands outside business hours.
    #[test]
    fn billing_cadence_is_daily_and_in_window() {
        let midday = berlin(10);
        let next = next_recurring_due(midday, BILLING_RECUR_HOURS);

        assert!(in_business_hours(next));
        // ~24h out, not the 3h the email nag uses.
        let delta = (next - midday).num_hours();
        assert!((23..=25).contains(&delta), "expected ~24h, got {delta}h");
    }

    /// 19:00 + 24h = 19:00 next day, which is outside the window and must snap
    /// forward to the next opening rather than pinging Alex at night.
    #[test]
    fn billing_cadence_snaps_evening_nag_to_next_morning() {
        let evening = berlin(19);
        let next = next_recurring_due(evening, BILLING_RECUR_HOURS);
        assert!(in_business_hours(next));
        assert!(next > evening);
    }

    /// A zero/negative cadence must not produce a reminder that fires forever in
    /// a tight loop; the floor of 1h keeps it moving forward.
    #[test]
    fn degenerate_cadence_still_advances() {
        let midday = berlin(10);
        assert!(next_recurring_due(midday, 0) > midday);
    }

    async fn try_pool() -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        sqlx::PgPool::connect(&url).await.ok()
    }

    /// The whole point of the invoice nag: it appears on its own when a dunning step
    /// comes due, and it disappears on its own once the invoice is paid — no matter
    /// where the payment was booked (register, dashboard, or Josie herself).
    ///
    /// If the auto-cancel half regresses, Alex gets nagged daily about an invoice he
    /// already collected, which is worse than not nagging at all.
    #[tokio::test]
    async fn invoice_nag_appears_when_due_and_dies_when_paid() {
        let Some(pool) = try_pool().await else { return };

        let chat_id: i64 = -999_100 - (Utc::now().timestamp() % 1000);
        sqlx::query(
            "INSERT INTO telegram_chat_bindings (chat_id, user_id, role)
             VALUES ($1, $2, 'owner') ON CONFLICT DO NOTHING",
        )
        .bind(chat_id)
        .bind(uuid::Uuid::now_v7())
        .execute(&pool)
        .await
        .expect("bind owner chat");

        // customer → inquiry → sent invoice → overdue dunning step
        let customer_id = uuid::Uuid::now_v7();
        sqlx::query("INSERT INTO customers (id, name, email) VALUES ($1, 'Nag Test', $2)")
            .bind(customer_id)
            .bind(format!("nag-{customer_id}@example.de"))
            .execute(&pool)
            .await
            .expect("insert customer");

        let inquiry_id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO inquiries (id, customer_id, status) VALUES ($1, $2, 'invoiced')",
        )
        .bind(inquiry_id)
        .bind(customer_id)
        .execute(&pool)
        .await
        .expect("insert inquiry");

        let invoice_id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO invoices (id, inquiry_id, invoice_number, invoice_type, status, sent_at)
             VALUES ($1, $2, $3, 'full', 'sent', NOW())",
        )
        .bind(invoice_id)
        .bind(inquiry_id)
        .bind(format!("NAG-{}", uuid::Uuid::new_v4()))
        .execute(&pool)
        .await
        .expect("insert invoice");

        let (reminder_id,): (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO invoice_reminders (invoice_id, level, status, remind_after)
             VALUES ($1, 2, 'pending', CURRENT_DATE - 2) RETURNING id",
        )
        .bind(invoice_id)
        .fetch_one(&pool)
        .await
        .expect("insert dunning step");

        // Reconcile: the nag should now exist, daily, addressed to the owner.
        reconcile_invoice_reminders(&pool)
            .await
            .expect("reconcile creates nag");

        let nag: Option<(String, i32, bool)> = sqlx::query_as(
            "SELECT text, recur_hours, active FROM agent_reminders \
             WHERE source = 'invoice' AND source_ref = $1",
        )
        .bind(reminder_id)
        .fetch_optional(&pool)
        .await
        .expect("load nag");

        let (text, recur_hours, active) = nag.expect("an overdue dunning step must raise a nag");
        assert!(active);
        assert_eq!(recur_hours, BILLING_RECUR_HOURS, "money nags daily, not every 3h");
        assert!(
            text.contains("1. Mahnung"),
            "the nag must name the level actually owed, got: {text}"
        );
        assert!(text.contains("Nag Test"), "the nag must name the customer, got: {text}");

        // Reconciling again must not pile up a second nag for the same step.
        reconcile_invoice_reminders(&pool)
            .await
            .expect("second reconcile");
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM agent_reminders WHERE source = 'invoice' AND source_ref = $1 AND active",
        )
        .bind(reminder_id)
        .fetch_one(&pool)
        .await
        .expect("count nags");
        assert_eq!(count, 1, "reconcile must be idempotent");

        // Pay it — the way the register does — and the nag must go quiet by itself.
        sqlx::query("UPDATE invoices SET status = 'paid', paid_at = NOW() WHERE id = $1")
            .bind(invoice_id)
            .execute(&pool)
            .await
            .expect("mark paid");
        sqlx::query("UPDATE invoice_reminders SET status = 'closed' WHERE id = $1")
            .bind(reminder_id)
            .execute(&pool)
            .await
            .expect("close dunning step");

        reconcile_invoice_reminders(&pool)
            .await
            .expect("reconcile after payment");

        let (still_active,): (bool,) =
            sqlx::query_as("SELECT active FROM agent_reminders WHERE source_ref = $1")
                .bind(reminder_id)
                .fetch_one(&pool)
                .await
                .expect("reload nag");
        assert!(
            !still_active,
            "a paid invoice must silence its nag — otherwise Alex is chased for money he already has"
        );

        // Cleanup by source_ref, not chat_id: a parallel test may hold the other
        // owner binding, and the reconcile's LIMIT 1 could have addressed the nag
        // to that chat instead of ours.
        sqlx::query("DELETE FROM agent_reminders WHERE source_ref = $1")
            .bind(reminder_id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM telegram_chat_bindings WHERE chat_id = $1")
            .bind(chat_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// A deferred Bewertungsanfrage raises a nag when its date arrives, and the nag
    /// dies as soon as the request is sent or skipped.
    #[tokio::test]
    async fn review_nag_appears_when_due_and_dies_when_handled() {
        let Some(pool) = try_pool().await else { return };

        let chat_id: i64 = -999_200 - (Utc::now().timestamp() % 1000);
        sqlx::query(
            "INSERT INTO telegram_chat_bindings (chat_id, user_id, role)
             VALUES ($1, $2, 'owner') ON CONFLICT DO NOTHING",
        )
        .bind(chat_id)
        .bind(uuid::Uuid::now_v7())
        .execute(&pool)
        .await
        .expect("bind owner chat");

        let customer_id = uuid::Uuid::now_v7();
        sqlx::query("INSERT INTO customers (id, name, email) VALUES ($1, 'Rezension Test', $2)")
            .bind(customer_id)
            .bind(format!("rev-{customer_id}@example.de"))
            .execute(&pool)
            .await
            .expect("insert customer");

        let inquiry_id = uuid::Uuid::now_v7();
        sqlx::query("INSERT INTO inquiries (id, customer_id, status) VALUES ($1, $2, 'paid')")
            .bind(inquiry_id)
            .bind(customer_id)
            .execute(&pool)
            .await
            .expect("insert inquiry");

        // Alex said "später", and that date has now passed.
        sqlx::query(
            "INSERT INTO review_requests (inquiry_id, status, remind_after)
             VALUES ($1, 'pending', CURRENT_DATE - 1)",
        )
        .bind(inquiry_id)
        .execute(&pool)
        .await
        .expect("insert deferred review request");

        reconcile_review_reminders(&pool).await.expect("reconcile");

        let nag: Option<(String, bool)> = sqlx::query_as(
            "SELECT text, active FROM agent_reminders WHERE source = 'review' AND source_ref = $1",
        )
        .bind(inquiry_id)
        .fetch_optional(&pool)
        .await
        .expect("load nag");

        let (text, active) = nag.expect("an overdue review request must raise a nag");
        assert!(active);
        assert!(text.contains("Rezension Test"), "must name the customer, got: {text}");

        // Sending it (status leaves 'pending') must silence the nag.
        sqlx::query("UPDATE review_requests SET status = 'sent' WHERE inquiry_id = $1")
            .bind(inquiry_id)
            .execute(&pool)
            .await
            .expect("mark sent");

        reconcile_review_reminders(&pool)
            .await
            .expect("reconcile after send");

        let (still_active,): (bool,) = sqlx::query_as(
            "SELECT active FROM agent_reminders WHERE source = 'review' AND source_ref = $1",
        )
        .bind(inquiry_id)
        .fetch_one(&pool)
        .await
        .expect("reload nag");
        assert!(!still_active, "a handled review request must silence its nag");

        sqlx::query("DELETE FROM agent_reminders WHERE source_ref = $1")
            .bind(inquiry_id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM telegram_chat_bindings WHERE chat_id = $1")
            .bind(chat_id)
            .execute(&pool)
            .await
            .ok();
    }
}
