//! Reminder tools: set, list, cancel.
//!
//! Reminders are pushed back to the chat by the background reminder tick
//! (`crate::hooks::reminders`). One-shot reminders fire once at `due_at`;
//! recurring ones re-fire every ~3h within business hours until cancelled.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Europe::Berlin;
use serde_json::{json, Value};

use crate::error::Result;
use crate::roles::Role;
use super::{parse_str, parse_uuid, Safety, Tool, ToolCtx};

/// Parse a `due_at` argument into a UTC instant. Accepts an RFC 3339 timestamp
/// (with offset) or a naive `YYYY-MM-DDTHH:MM[:SS]` / `YYYY-MM-DD HH:MM[:SS]`
/// which is interpreted as Europe/Berlin local time.
/// Format a UTC instant as Europe/Berlin local wall-clock time, e.g.
/// `09.06.2026 12:00 Uhr`. The stored `due_at` is always UTC; surfacing the local
/// time here stops the model (and Alex) misreading `…10:00Z` as "wrong time" when
/// it is in fact the correct 12:00 Berlin.
fn berlin_local(dt: DateTime<Utc>) -> String {
    dt.with_timezone(&Berlin).format("%d.%m.%Y %H:%M Uhr").to_string()
}

fn parse_due_at(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    let s = s.trim();
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return Berlin
                .from_local_datetime(&naive)
                .single()
                .or_else(|| Berlin.from_local_datetime(&naive).earliest())
                .map(|dt| dt.with_timezone(&Utc));
        }
    }
    None
}

// ── SetReminder ───────────────────────────────────────────────────────────────

pub struct SetReminder;

#[async_trait]
impl Tool for SetReminder {
    fn name(&self) -> &'static str { "set_reminder" }
    fn description(&self) -> &'static str {
        "Legt eine Erinnerung an, die dir per Telegram zugestellt wird. due_at ist der Zeitpunkt \
         (ISO 8601, z. B. '2026-06-14T09:00' = Europe/Berlin). Setze repeat=true für eine \
         dauerhafte Erinnerung, die alle ~3 Stunden während der Geschäftszeiten (07–20 Uhr) \
         wiederholt wird, bis sie abgeschaltet wird; sonst feuert sie einmalig. Nutze das \
         Datum aus dem Systemkontext, um relative Angaben ('morgen', 'in 2 Stunden') selbst \
         in einen konkreten due_at-Zeitpunkt umzurechnen."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text":   { "type": "string", "minLength": 1 },
                "due_at": { "type": "string", "minLength": 1 },
                "repeat": { "type": "boolean" }
            },
            "required": ["text", "due_at"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let text = parse_str(args, "text", self.name())?;
        let due_raw = parse_str(args, "due_at", self.name())?;
        let repeat = args["repeat"].as_bool().unwrap_or(false);

        let due_at = match parse_due_at(due_raw) {
            Some(dt) => dt,
            None => {
                return Ok(json!({
                    "ok": false,
                    "message": format!("Konnte den Zeitpunkt '{due_raw}' nicht verstehen. Bitte als ISO 8601 angeben, z. B. 2026-06-14T09:00.")
                }))
            }
        };

        match ctx.services.reminders.create(ctx.chat_id, text, due_at, repeat).await {
            Ok(rec) => Ok(json!({
                "ok": true,
                "id": rec.id,
                "text": rec.text,
                "due_at": rec.due_at,
                "due_at_local": berlin_local(rec.due_at),
                "recurrence": rec.recurrence
            })),
            Err(aust_core::services::ServiceError::Validation(msg)) => {
                Ok(json!({ "ok": false, "message": msg }))
            }
            Err(e) => Err(e.into()),
        }
    }
}

// ── ListReminders ─────────────────────────────────────────────────────────────

pub struct ListReminders;

#[async_trait]
impl Tool for ListReminders {
    fn name(&self) -> &'static str { "list_reminders" }
    fn description(&self) -> &'static str {
        "Listet die Erinnerungen dieses Chats (standardmäßig nur aktive)."
    }
    fn params_schema(&self) -> Value {
        json!({ "type": "object", "properties": { "include_inactive": { "type": "boolean" } } })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let include_inactive = args["include_inactive"].as_bool().unwrap_or(false);
        let items = ctx.services.reminders.list(ctx.chat_id, !include_inactive).await?;
        let count = items.len();
        // Enrich each row with the Berlin-local due time so the model reports the
        // wall-clock time Alex expects rather than the raw UTC instant.
        let reminders: Vec<Value> = items
            .into_iter()
            .map(|r| {
                let due_local = berlin_local(r.due_at);
                let mut v = serde_json::to_value(&r).unwrap_or_else(|_| json!({}));
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("due_at_local".to_string(), json!(due_local));
                }
                v
            })
            .collect();
        Ok(json!({ "reminders": reminders, "count": count }))
    }
}

// ── CancelReminder ────────────────────────────────────────────────────────────

pub struct CancelReminder;

#[async_trait]
impl Tool for CancelReminder {
    fn name(&self) -> &'static str { "cancel_reminder" }
    fn description(&self) -> &'static str {
        "Schaltet eine Erinnerung ab (auch dauerhafte E-Mail-Erinnerungen). Benötigt die Reminder-ID."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string", "format": "uuid" } },
            "required": ["id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        match ctx.services.reminders.cancel(id).await {
            Ok(rec) => Ok(json!({ "ok": true, "id": rec.id, "text": rec.text })),
            Err(aust_core::services::ServiceError::NotFound(msg)) => {
                Ok(json!({ "ok": false, "message": msg }))
            }
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testing;
    use std::sync::Arc;

    fn dangling_pool() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid_user:invalid@127.0.0.1:1/invalid")
            .expect("lazy connect cannot fail")
    }

    fn ctx(services: aust_core::services::ServiceBundle) -> ToolCtx {
        ToolCtx {
            db: dangling_pool(),
            llm: Arc::new(crate::llm::MockAssistantLlm::always("ok")),
            services,
            role: Role::Owner,
            user_id: uuid::Uuid::nil(),
            chat_id: 42,
            session_id: uuid::Uuid::nil(),
            confirmed: false,
        }
    }

    #[test]
    fn parse_due_at_accepts_iso_and_naive() {
        assert!(parse_due_at("2026-06-14T09:00").is_some());
        assert!(parse_due_at("2026-06-14 09:00:00").is_some());
        assert!(parse_due_at("2026-06-14T09:00:00+02:00").is_some());
        assert!(parse_due_at("morgen früh").is_none());
    }

    #[tokio::test]
    async fn set_reminder_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = SetReminder
            .execute(&ctx(services), &json!({ "text": "Kunde Fabig anrufen", "due_at": "2026-06-14T09:00", "repeat": true }))
            .await
            .unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["recurrence"], json!("recurring"));
    }

    #[tokio::test]
    async fn set_reminder_rejects_bad_time() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = SetReminder
            .execute(&ctx(services), &json!({ "text": "x", "due_at": "irgendwann" }))
            .await
            .unwrap();
        assert_eq!(r["ok"], json!(false));
    }

    #[tokio::test]
    async fn list_reminders_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = ListReminders.execute(&ctx(services), &json!({})).await.unwrap();
        assert_eq!(r["count"], json!(1));
    }
}
