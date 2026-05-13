//! Telegram long-polling loop and callback handler.

use anyhow::Result;
use aust_flash_contact::{mark_dismissed, mark_handled, next_snooze, schedule_snooze, TimePreference};
use reqwest::Client;
use serde::Deserialize;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

const POLL_TIMEOUT_SECS: u64 = 30;

// ── Telegram API types ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct TgResponse<T> {
    ok: bool,
    result: Option<T>,
}

#[derive(Debug, Deserialize)]
struct Update {
    update_id: i64,
    callback_query: Option<CallbackQuery>,
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    id: String,
    data: Option<String>,
    from: TgUser,
    message: Option<TgMessage>,
}

#[derive(Debug, Deserialize)]
struct TgUser {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct TgMessage {
    chat: TgChat,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
}

// ── Main loop ─────────────────────────────────────────────────────────────────

pub async fn run(db: PgPool, bot_token: String, admin_chat_id: i64) -> Result<()> {
    let client = Client::new();
    let base = format!("https://api.telegram.org/bot{}", bot_token);
    let mut offset: i64 = 0;
    let mut backoff_secs: u64 = 5;

    loop {
        let updates = match poll(&client, &base, offset).await {
            Ok(u) => {
                backoff_secs = 5;
                u
            }
            Err(e) => {
                warn!("Telegram poll error: {e}; retrying in {backoff_secs}s");
                tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(60);
                continue;
            }
        };

        for update in updates {
            offset = offset.max(update.update_id + 1);

            let Some(cb) = update.callback_query else { continue };
            let Some(data) = cb.data else { continue };

            // Accept clicks from the admin user (private chat) OR from the admin chat (group chat).
            let from_admin = cb.from.id == admin_chat_id
                || cb.message.as_ref().map(|m| m.chat.id) == Some(admin_chat_id);
            if !from_admin {
                answer_callback(&client, &base, &cb.id, Some("Nicht autorisiert.")).await;
                continue;
            }

            if let Some(id_str) = data.strip_prefix("fc_reached:") {
                handle_reached(&client, &base, &db, &cb.id, id_str).await;
            } else if let Some(id_str) = data.strip_prefix("fc_snooze:") {
                handle_snooze(&client, &base, &db, &cb.id, id_str).await;
            } else if let Some(id_str) = data.strip_prefix("fc_dismiss:") {
                handle_dismiss(&client, &base, &db, &cb.id, id_str).await;
            }
        }
    }
}

// ── Handlers ─────────────────────────────────────────────────────────────────

async fn handle_reached(client: &Client, base: &str, db: &PgPool, cb_id: &str, id_str: &str) {
    let Ok(id) = id_str.parse::<Uuid>() else {
        answer_callback(client, base, cb_id, Some("Ungültige ID.")).await;
        return;
    };
    match mark_handled(db, id).await {
        Ok(()) => {
            info!("Flash contact {id} marked as reached");
            answer_callback(client, base, cb_id, Some("✅ Als erreicht markiert.")).await;
        }
        Err(e) => {
            warn!("mark_handled failed for {id}: {e}");
            answer_callback(client, base, cb_id, Some("Fehler beim Speichern.")).await;
        }
    }
}

async fn handle_snooze(client: &Client, base: &str, db: &PgPool, cb_id: &str, id_str: &str) {
    let Ok(id) = id_str.parse::<Uuid>() else {
        answer_callback(client, base, cb_id, Some("Ungültige ID.")).await;
        return;
    };

    // Look up the contact's time_preference to compute the next slot.
    let pref = match fetch_preference(db, id).await {
        Some(p) => p,
        None => {
            answer_callback(client, base, cb_id, Some("Kontakt nicht gefunden.")).await;
            return;
        }
    };

    let now = chrono::Utc::now();
    let Some(next) = next_snooze(pref, now) else {
        answer_callback(client, base, cb_id, Some("Kein weiterer Slot verfügbar.")).await;
        return;
    };

    match schedule_snooze(db, id, next).await {
        Ok(()) => {
            let berlin = next.with_timezone(&chrono_tz::Europe::Berlin);
            let label = berlin.format("%d.%m. %H:%M Uhr").to_string();
            info!("Flash contact {id} snoozed until {label}");
            answer_callback(
                client,
                base,
                cb_id,
                Some(&format!("🔁 Erinnerung um {label}")),
            )
            .await;
        }
        Err(e) => {
            warn!("schedule_snooze failed for {id}: {e}");
            answer_callback(client, base, cb_id, Some("Fehler beim Speichern.")).await;
        }
    }
}

async fn handle_dismiss(client: &Client, base: &str, db: &PgPool, cb_id: &str, id_str: &str) {
    let Ok(id) = id_str.parse::<Uuid>() else {
        answer_callback(client, base, cb_id, Some("Ungültige ID.")).await;
        return;
    };
    match mark_dismissed(db, id).await {
        Ok(()) => {
            info!("Flash contact {id} dismissed");
            answer_callback(client, base, cb_id, Some("🗑 Verworfen.")).await;
        }
        Err(e) => {
            warn!("mark_dismissed failed for {id}: {e}");
            answer_callback(client, base, cb_id, Some("Fehler beim Speichern.")).await;
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn poll(client: &Client, base: &str, offset: i64) -> Result<Vec<Update>> {
    let resp: TgResponse<Vec<Update>> = client
        .post(format!("{base}/getUpdates"))
        .json(&serde_json::json!({
            "offset": offset,
            "timeout": POLL_TIMEOUT_SECS,
            "allowed_updates": ["callback_query"],
        }))
        .timeout(std::time::Duration::from_secs(POLL_TIMEOUT_SECS + 5))
        .send()
        .await?
        .json()
        .await?;

    if !resp.ok {
        anyhow::bail!("Telegram getUpdates returned ok=false");
    }
    Ok(resp.result.unwrap_or_default())
}

async fn answer_callback(client: &Client, base: &str, callback_query_id: &str, text: Option<&str>) {
    let mut payload = serde_json::json!({ "callback_query_id": callback_query_id });
    if let Some(t) = text {
        payload["text"] = serde_json::Value::String(t.to_string());
        payload["show_alert"] = serde_json::Value::Bool(false);
    }
    if let Err(e) = client
        .post(format!("{base}/answerCallbackQuery"))
        .json(&payload)
        .send()
        .await
    {
        warn!("answerCallbackQuery failed: {e}");
    }
}

async fn fetch_preference(db: &PgPool, id: Uuid) -> Option<TimePreference> {
    use sqlx::Row;
    let row = sqlx::query("SELECT time_preference FROM flash_contacts WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
        .ok()??;
    let s: String = row.try_get("time_preference").ok()?;
    s.parse().ok()
}
