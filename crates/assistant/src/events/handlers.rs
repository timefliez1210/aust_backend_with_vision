//! Per-kind domain event handlers.
//!
//! Each handler receives a `DomainEvent` plus a `TelegramNotifier` to post
//! messages to Alex. Handlers look up the Owner chat_id from `telegram_chat_bindings`
//! before posting. If no owner binding exists, the handler logs a warning and returns.
//!
//! Handlers are intentionally simple: they extract the minimum information they
//! need from the event payload, format a German-language message, and post it.

use sqlx::PgPool;
use tracing::{info, warn};

use aust_core::events::DomainEvent;

use crate::error::Result;
use super::notifier::TelegramNotifier;

// ── Helper: look up the first Owner chat_id ──────────────────────────────────

/// Fetch the Telegram chat_id of the Owner binding, if one exists.
///
/// We look for the first row with role = 'owner'. In practice there is exactly
/// one, but the query returns `None` gracefully if the table is empty.
async fn owner_chat_id(pool: &PgPool) -> Option<i64> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT chat_id FROM telegram_chat_bindings WHERE role = 'owner' LIMIT 1")
            .fetch_optional(pool)
            .await
            .ok()?;
    row.map(|(id,)| id)
}

// ── Handlers ─────────────────────────────────────────────────────────────────

pub async fn handle_inquiry_created(
    event: &DomainEvent,
    pool: &PgPool,
    notifier: &dyn TelegramNotifier,
) -> Result<()> {
    let payload = &event.payload;
    let source = payload["source"].as_str().unwrap_or("unknown");

    // Only notify for email-sourced inquiries — other sources are admin-initiated
    // and Alex is already aware of them.
    if source != "email" {
        info!(event_id = %event.id, "inquiry.created from source='{source}' — skipping notification");
        return Ok(());
    }

    let Some(chat_id) = owner_chat_id(pool).await else {
        warn!("inquiry.created: no owner chat binding found, cannot notify");
        return Ok(());
    };

    let name = payload["customer_name"].as_str().unwrap_or("Unbekannt");
    let volume = payload["volume_m3"].as_f64().unwrap_or(0.0);
    let from = payload["from_address"].as_str().unwrap_or("?");
    let to = payload["to_address"].as_str().unwrap_or("?");

    // Proactively offer a Besichtigung on every email inquiry (product decision):
    // a pre-move on-site survey is common, so Josie nudges Alex here. On "ja, am
    // <Datum>" she grounds the inquiry via her lookup tools and calls
    // create_inquiry_appointment — the visit is a separate, non-consecutive date,
    // not part of the move itself.
    let msg = format!(
        "📥 Neue Anfrage von {name}, {volume:.1} m³, {from} → {to}. Ich rechne.\n\n\
         Soll ich vorab eine Besichtigung eintragen? Sag mir einfach das Wunschdatum, dann lege ich sie an."
    );
    let _ = notifier.post(chat_id, msg).await;
    Ok(())
}

pub async fn handle_offer_drafted(
    event: &DomainEvent,
    pool: &PgPool,
    notifier: &dyn TelegramNotifier,
) -> Result<()> {
    let payload = &event.payload;

    // Read the routing decision that was stamped on the offer row at draft time (B4).
    // We do NOT re-read the live settings flag here — flipping the flag after draft
    // time must not change which path handles approval for already-drafted offers.
    //
    // Fallback behaviour:
    //   - `approval_owner = 'agent'`  → this handler posts.
    //   - `approval_owner = 'legacy'` → legacy pipeline already posted; skip.
    //   - `approval_owner IS NULL`    → pre-migration offer; treat as 'legacy'.
    //   - offer_id missing / DB error → conservative fallback: skip (don't double-post).
    let offer_id_str = payload["offer_id"].as_str().unwrap_or("");

    // Resolve routing decision AND display data in one query. The emitter
    // (offer_pipeline) only puts offer_id + inquiry_id on the payload, so the
    // customer name and brutto price MUST come from the DB here — otherwise the
    // message read "Angebot fertig für Unbekannt: 0.00 € brutto" (H3 regression).
    // offers.price_cents is the brutto total; customers.name is the display name.
    let offer_row: Option<(Option<String>, i64, Option<String>)> = if offer_id_str.is_empty() {
        None
    } else if let Ok(offer_id) = offer_id_str.parse::<uuid::Uuid>() {
        sqlx::query_as(
            "SELECT o.approval_owner, o.price_cents, c.name \
             FROM offers o \
             JOIN inquiries i ON i.id = o.inquiry_id \
             LEFT JOIN customers c ON c.id = i.customer_id \
             WHERE o.id = $1",
        )
        .bind(offer_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
    } else {
        None
    };

    let agent_owns = matches!(offer_row, Some((Some(ref s), _, _)) if s == "agent");

    if !agent_owns {
        info!(event_id = %event.id, "offer.drafted: approval_owner != 'agent' — legacy flow handles it");
        return Ok(());
    }

    let Some(chat_id) = owner_chat_id(pool).await else {
        warn!("offer.drafted: no owner chat binding found, cannot notify");
        return Ok(());
    };

    // Prefer DB-resolved values; fall back to the payload so a future enriched
    // emitter still works, and finally to safe defaults.
    let (db_brutto_cents, db_name) = match &offer_row {
        Some((_, price, name)) => (*price, name.clone()),
        None => (0, None),
    };
    let name = payload["customer_name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or(db_name)
        .unwrap_or_else(|| "Unbekannt".to_string());
    let brutto_cents = payload["brutto_cents"]
        .as_i64()
        .filter(|c| *c != 0)
        .unwrap_or(db_brutto_cents);
    let brutto = brutto_cents as f64 / 100.0;
    let offer_id = if offer_id_str.is_empty() { "?" } else { offer_id_str };

    // B4: previous text told Alex to tap "/approve <id>" / "/deny <id>" — but
    // no such command parser existed and no inline buttons were attached, so
    // tapping or typing either did nothing. Until the SMTP/S3 send path is
    // plumbed through OfferService and the inline keyboard wired, give Alex an
    // honest hand-off to the admin panel rather than a phantom command.
    let msg = format!(
        "✉️ Angebot fertig für {name}: {brutto:.2} € brutto.\n\
         Angebot-ID: {offer_id}\n\n\
         Bitte über das Admin-Panel prüfen und senden — der Agent-Sendpfad \
         wird verdrahtet, sobald OfferService::send (SMTP + PDF-Anhang) bereit ist."
    );
    let _ = notifier.post(chat_id, msg).await;
    Ok(())
}

pub async fn handle_offer_sent(
    event: &DomainEvent,
    pool: &PgPool,
    notifier: &dyn TelegramNotifier,
) -> Result<()> {
    let Some(chat_id) = owner_chat_id(pool).await else {
        warn!("offer.sent: no owner chat binding found");
        return Ok(());
    };

    let name = payload_str(&event.payload, "customer_name");
    let msg = format!("📨 Angebot an {name} verschickt.");
    let _ = notifier.post(chat_id, msg).await;
    Ok(())
}

pub async fn handle_status_changed(
    event: &DomainEvent,
    pool: &PgPool,
    notifier: &dyn TelegramNotifier,
) -> Result<()> {
    let payload = &event.payload;
    let new_status = payload["new_status"].as_str().unwrap_or("unknown");

    // Only notify for important transitions; skip noisy intermediate states.
    let important = matches!(new_status, "accepted" | "rejected" | "completed" | "paid");
    if !important {
        info!(event_id = %event.id, status = new_status, "status.changed — not important, skipping");
        return Ok(());
    }

    let Some(chat_id) = owner_chat_id(pool).await else {
        warn!("status.changed: no owner chat binding found");
        return Ok(());
    };

    let name = payload_str(payload, "customer_name");
    let old = payload_str(payload, "old_status");
    let msg = format!("🔄 {name}: {old} → {new_status}.");
    let _ = notifier.post(chat_id, msg).await;
    Ok(())
}

pub async fn handle_invoice_issued(
    event: &DomainEvent,
    pool: &PgPool,
    notifier: &dyn TelegramNotifier,
) -> Result<()> {
    let Some(chat_id) = owner_chat_id(pool).await else {
        warn!("invoice.issued: no owner chat binding found");
        return Ok(());
    };

    let p = &event.payload;
    let number = payload_str(p, "invoice_number");
    let name = payload_str(p, "customer_name");
    let brutto_cents = p["brutto_cents"].as_i64().unwrap_or(0);
    let brutto = brutto_cents as f64 / 100.0;

    let msg = format!("🧾 Rechnung {number} an {name}: {brutto:.2} €.");
    let _ = notifier.post(chat_id, msg).await;
    Ok(())
}

pub async fn handle_invoice_overdue(
    event: &DomainEvent,
    pool: &PgPool,
    notifier: &dyn TelegramNotifier,
) -> Result<()> {
    let Some(chat_id) = owner_chat_id(pool).await else {
        warn!("invoice.overdue: no owner chat binding found");
        return Ok(());
    };

    let p = &event.payload;
    let number = payload_str(p, "invoice_number");
    let name = payload_str(p, "customer_name");
    let days = p["days_overdue"].as_i64().unwrap_or(0);

    let msg = format!("⚠️ Rechnung {number} überfällig: {name}, {days} Tage.");
    let _ = notifier.post(chat_id, msg).await;
    Ok(())
}

// ── Utility ───────────────────────────────────────────────────────────────────

fn payload_str<'a>(payload: &'a serde_json::Value, key: &str) -> &'a str {
    payload[key].as_str().unwrap_or("Unbekannt")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::notifier::MockNotifier;
    use std::sync::Arc;
    use uuid::Uuid;

    fn make_event(kind: &str, payload: serde_json::Value) -> DomainEvent {
        DomainEvent {
            id: Uuid::now_v7(),
            kind: kind.to_string(),
            aggregate: "test:aggregate".to_string(),
            payload,
            created_at: chrono::Utc::now(),
        }
    }

    fn dangling_pool() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid_user:invalid@127.0.0.1:1/invalid")
            .expect("lazy connect cannot fail")
    }

    /// When approval_owner is not 'agent' (or DB is unreachable), the handler must not post.
    #[tokio::test]
    async fn offer_drafted_skips_when_approval_owner_not_agent() {
        let pool = dangling_pool();
        let notifier = Arc::new(MockNotifier::new());
        let event = make_event("offer.drafted", serde_json::json!({
            "offer_id": Uuid::new_v4().to_string(),
            "customer_name": "Max Mustermann",
            "brutto_cents": 119000,
        }));

        // Pool is dangling → offers query fails → falls back to false → no post.
        let result = handle_offer_drafted(&event, &pool, notifier.as_ref()).await;
        assert!(result.is_ok());
        assert!(
            notifier.recorded().is_empty(),
            "notifier should not be called when approval_owner != 'agent'"
        );
    }

    /// Race condition: flag=false at draft time (approval_owner='legacy'), flag=true at consume
    /// → handler must NOT post (stored decision wins over live flag).
    ///
    /// We simulate this by using a dangling pool (offer row not reachable) — the handler
    /// conservatively treats unreachable offer as 'legacy' and skips.
    #[tokio::test]
    async fn offer_drafted_race_flag_on_at_consume_does_not_double_post() {
        let pool = dangling_pool();
        let notifier = Arc::new(MockNotifier::new());
        let event = make_event("offer.drafted", serde_json::json!({
            "offer_id": Uuid::new_v4().to_string(),
            "customer_name": "Test",
            "brutto_cents": 50000,
        }));

        // approval_owner for this offer was 'legacy' at draft time (simulated by DB being
        // unreachable → conservative fallback = no post).
        handle_offer_drafted(&event, &pool, notifier.as_ref()).await.unwrap();
        assert!(notifier.recorded().is_empty(), "must not double-post on race: flag=true at consume, 'legacy' at draft");
    }

    /// handle_invoice_issued with no owner binding (dangling pool) must return Ok, not panic.
    #[tokio::test]
    async fn invoice_issued_no_owner_binding_is_ok() {
        let pool = dangling_pool();
        let notifier = Arc::new(MockNotifier::new());
        let event = make_event("invoice.issued", serde_json::json!({
            "invoice_number": "R-2026-001",
            "customer_name": "Max Mustermann",
            "brutto_cents": 59500,
        }));

        let result = handle_invoice_issued(&event, &pool, notifier.as_ref()).await;
        assert!(result.is_ok(), "should return Ok even with no DB connection");
    }

    /// Status.changed only notifies for important statuses.
    #[tokio::test]
    async fn status_changed_skips_noisy_statuses() {
        let pool = dangling_pool();
        let notifier = Arc::new(MockNotifier::new());

        for noisy in ["estimating", "estimated", "offer_ready", "offer_sent"] {
            let event = make_event("status.changed", serde_json::json!({
                "old_status": "pending",
                "new_status": noisy,
                "customer_name": "Max",
            }));
            handle_status_changed(&event, &pool, notifier.as_ref()).await.unwrap();
        }

        // None of the noisy statuses should have triggered a DB query (which would
        // fail on dangling pool and panic) or a notifier call.
        assert!(notifier.recorded().is_empty(), "noisy statuses must not notify");
    }
}
