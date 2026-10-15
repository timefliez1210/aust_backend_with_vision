//! Which entities a bot message in Telegram was about (`telegram_message_refs`).
//!
//! Every bot post records the IDs behind it, keyed by the Telegram message ID.
//! When Alex replies to that message, the referent comes from here — exact IDs —
//! rather than from the wording, which usually names only a customer and made
//! Josie search by name (the wrong-Frederike reminder).

use serde_json::{Map, Value};
use sqlx::PgPool;
use tracing::warn;

/// Most IDs kept in a ref's `"ids"` list (a reply that cited a dozen records).
const MAX_IDS: usize = 20;

/// Record what `message_id` in `chat_id` was about. Best-effort: a failure is
/// logged, never surfaced — the message itself already went out.
pub async fn record(pool: &PgPool, chat_id: i64, message_id: i64, refs: Value) {
    if message_id == 0 || is_empty(&refs) {
        return;
    }
    let res = sqlx::query(
        "INSERT INTO telegram_message_refs (chat_id, message_id, refs) VALUES ($1, $2, $3)
         ON CONFLICT (tenant_id, chat_id, message_id) DO UPDATE SET refs = EXCLUDED.refs",
    )
    .bind(chat_id)
    .bind(message_id)
    .bind(&refs)
    .execute(pool)
    .await;
    if let Err(e) = res {
        warn!(chat_id, message_id, "Failed to record telegram message refs: {e}");
    }
}

/// The refs recorded for a bot message, if any.
pub async fn lookup(pool: &PgPool, chat_id: i64, message_id: i64) -> Option<Value> {
    sqlx::query_scalar("SELECT refs FROM telegram_message_refs WHERE chat_id = $1 AND message_id = $2")
        .bind(chat_id)
        .bind(message_id)
        .fetch_optional(pool)
        .await
        .unwrap_or_else(|e| {
            warn!(chat_id, message_id, "Failed to look up telegram message refs: {e}");
            None
        })
}

/// Refs from an event payload or tool arguments: every string `*_id` field.
pub fn from_payload(payload: &Value) -> Value {
    let mut out = Map::new();
    if let Some(obj) = payload.as_object() {
        for (k, v) in obj {
            if k == "id" || k.ends_with("_id") {
                if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
                    out.insert(k.clone(), Value::String(s.to_string()));
                }
            }
        }
    }
    Value::Object(out)
}

/// Refs listing loose IDs (e.g. every ID a reply's tool results carried).
pub fn from_ids(ids: impl IntoIterator<Item = String>) -> Value {
    let mut list: Vec<String> = Vec::new();
    for id in ids {
        if !list.contains(&id) {
            list.push(id);
        }
        if list.len() == MAX_IDS {
            break;
        }
    }
    if list.is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::json!({ "ids": list })
    }
}

/// One line for the prompt: `inquiry_id=…, customer_id=…, ids=[…]`.
pub fn render(refs: &Value) -> String {
    let Some(obj) = refs.as_object() else { return String::new() };
    let mut parts: Vec<String> = Vec::new();
    for (k, v) in obj {
        match v {
            Value::String(s) => parts.push(format!("{k}={s}")),
            Value::Array(a) => {
                let items: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
                if !items.is_empty() {
                    parts.push(format!("{k}=[{}]", items.join(", ")));
                }
            }
            _ => {}
        }
    }
    parts.join(", ")
}

fn is_empty(refs: &Value) -> bool {
    refs.as_object().is_none_or(|o| o.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payload_refs_keep_only_string_ids() {
        let refs = from_payload(&json!({
            "inquiry_id": "a", "customer_id": "b", "volume_m3": 12.5, "name": "Frederike", "offer_id": ""
        }));
        assert_eq!(refs, json!({ "inquiry_id": "a", "customer_id": "b" }));
    }

    #[test]
    fn id_refs_dedupe_and_cap() {
        let refs = from_ids((0..30).map(|i| format!("id-{}", i % 25)));
        assert_eq!(refs["ids"].as_array().unwrap().len(), MAX_IDS);
        assert_eq!(from_ids(Vec::<String>::new()), json!({}));
    }

    /// The Frederike case end to end: a notification records its inquiry, and a
    /// reply to that Telegram message gets back exactly that inquiry's ID.
    #[tokio::test]
    async fn reply_to_a_recorded_message_resolves_its_entities() {
        let Ok(url) = std::env::var("DATABASE_URL") else { return };
        let Ok(pool) = sqlx::PgPool::connect(&url).await else { return };

        let chat_id = -(chrono::Utc::now().timestamp_micros());
        let refs = json!({ "inquiry_id": "0199aaaa-0000-7000-8000-000000000001", "customer_id": "c" });
        record(&pool, chat_id, 4711, refs.clone()).await;
        assert_eq!(lookup(&pool, chat_id, 4711).await, Some(refs));
        assert_eq!(lookup(&pool, chat_id, 4712).await, None, "unknown message → no refs");

        // Message id 0 means the send failed; nothing to record.
        record(&pool, chat_id, 0, json!({ "inquiry_id": "x" })).await;
        assert_eq!(lookup(&pool, chat_id, 0).await, None);

        sqlx::query("DELETE FROM telegram_message_refs WHERE chat_id = $1")
            .bind(chat_id)
            .execute(&pool)
            .await
            .ok();
    }

    #[test]
    fn render_is_one_readable_line() {
        let line = render(&json!({ "inquiry_id": "a", "ids": ["x", "y"] }));
        assert!(line.contains("inquiry_id=a"));
        assert!(line.contains("ids=[x, y]"));
    }
}
