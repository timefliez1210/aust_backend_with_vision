//! Email tools: list inbox, get email, list thread, draft reply, send, mark, categorize.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::Result;
use crate::roles::Role;
use super::{parse_str, parse_uuid, pending_confirmation, Safety, Tool, ToolCtx};

// ── ListInbox ─────────────────────────────────────────────────────────────────

pub struct ListInbox;

#[async_trait]
impl Tool for ListInbox {
    fn name(&self) -> &'static str { "list_inbox" }
    fn description(&self) -> &'static str {
        "Listet die neuesten E-Mail-Nachrichten aus dem Posteingang auf. Nur für Inhaber verfügbar."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 50 } }
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let limit = args["limit"].as_i64().unwrap_or(10).clamp(1, 50) as u32;
        let messages = ctx.services.emails.list_inbox(limit).await?;
        let count = messages.len();
        Ok(json!({ "messages": messages, "count": count }))
    }
}

// ── GetEmail ──────────────────────────────────────────────────────────────────

pub struct GetEmail;

#[async_trait]
impl Tool for GetEmail {
    fn name(&self) -> &'static str { "get_email" }
    fn description(&self) -> &'static str { "Lädt eine E-Mail (inkl. Text) anhand der ID." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string", "format": "uuid" } },
            "required": ["id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        let email = ctx.services.emails.get_email(id).await?;
        Ok(json!(email))
    }
}

// ── ListThread ────────────────────────────────────────────────────────────────

pub struct ListThread;

#[async_trait]
impl Tool for ListThread {
    fn name(&self) -> &'static str { "list_thread" }
    fn description(&self) -> &'static str { "Listet die vollständige E-Mail-Konversation eines Kunden." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "customer_id": { "type": "string", "format": "uuid" } },
            "required": ["customer_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "customer_id", self.name())?;
        let items = ctx.services.emails.list_thread(id).await?;
        let count = items.len();
        Ok(json!({ "messages": items, "count": count }))
    }
}

// ── DraftReply ────────────────────────────────────────────────────────────────

pub struct DraftReply;

#[async_trait]
impl Tool for DraftReply {
    fn name(&self) -> &'static str { "draft_reply" }
    fn description(&self) -> &'static str {
        "Erstellt einen E-Mail-Antwortentwurf basierend auf einer Klartext-Anweisung. Sendet NICHT."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "email_id":        { "type": "string", "format": "uuid" },
                "instruction_de":  { "type": "string", "minLength": 1 }
            },
            "required": ["email_id", "instruction_de"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let email_id = parse_uuid(args, "email_id", self.name())?;
        let instruction = parse_str(args, "instruction_de", self.name())?;
        // Fetch original for context (best-effort); the LLM call is left for the
        // chat layer — here we just synthesize a placeholder draft.
        let original = ctx.services.emails.get_email(email_id).await.ok();
        let draft = format!(
            "Sehr geehrte Damen und Herren,\n\n[Entwurf basierend auf Anweisung: \"{instruction}\"]\n\nMit freundlichen Grüßen\nAust Umzüge"
        );
        Ok(json!({
            "draft": draft,
            "email_id": email_id,
            "original_subject": original.as_ref().map(|e| e.subject.clone()),
        }))
    }
}

// ── SendEmail (Confirm) ───────────────────────────────────────────────────────

pub struct SendEmail;

#[async_trait]
impl Tool for SendEmail {
    fn name(&self) -> &'static str { "send_email" }
    fn description(&self) -> &'static str {
        "Sendet eine E-Mail an einen Empfänger. Erfordert Bestätigung."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "to":          { "type": "string", "minLength": 1 },
                "subject":     { "type": "string", "minLength": 1 },
                "body":        { "type": "string", "minLength": 1 },
                "attachments": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["to", "subject", "body"]
        })
    }
    fn safety(&self) -> Safety { Safety::Confirm }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, _ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let to = parse_str(args, "to", self.name())?;
        Ok(pending_confirmation(self.name(), args, format!("E-Mail an {to} senden?")))
    }
}

// ── MarkEmailHandled ──────────────────────────────────────────────────────────

pub struct MarkEmailHandled;

#[async_trait]
impl Tool for MarkEmailHandled {
    fn name(&self) -> &'static str { "mark_email_handled" }
    fn description(&self) -> &'static str { "Markiert eine E-Mail als bearbeitet." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string", "format": "uuid" } },
            "required": ["id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        ctx.services.emails.mark_handled(id).await?;
        Ok(json!({ "ok": true }))
    }
}

// ── CategorizeEmail ───────────────────────────────────────────────────────────

pub struct CategorizeEmail;

#[async_trait]
impl Tool for CategorizeEmail {
    fn name(&self) -> &'static str { "categorize_email" }
    fn description(&self) -> &'static str { "Setzt eine Kategorie / Label auf eine E-Mail." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id":    { "type": "string", "format": "uuid" },
                "label": { "type": "string", "minLength": 1 }
            },
            "required": ["id", "label"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        let label = parse_str(args, "label", self.name())?;
        ctx.services.emails.categorize(id, label).await?;
        Ok(json!({ "ok": true }))
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
            chat_id: 0,
            session_id: uuid::Uuid::nil(),
            confirmed: false,
        }
    }

    #[tokio::test]
    async fn get_email_returns_detail() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = GetEmail.execute(&ctx(services), &json!({ "id": uuid::Uuid::new_v4() })).await.unwrap();
        assert!(r["subject"].is_string());
    }

    #[tokio::test]
    async fn list_thread_returns_count() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = ListThread
            .execute(&ctx(services), &json!({ "customer_id": uuid::Uuid::new_v4() }))
            .await
            .unwrap();
        assert_eq!(r["count"], json!(0));
    }

    #[tokio::test]
    async fn draft_reply_returns_draft() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = DraftReply
            .execute(
                &ctx(services),
                &json!({ "email_id": uuid::Uuid::new_v4(), "instruction_de": "Bestätige Termin" }),
            )
            .await
            .unwrap();
        assert!(r["draft"].as_str().unwrap().contains("Anweisung"));
    }

    #[tokio::test]
    async fn send_email_pending() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = SendEmail
            .execute(
                &ctx(services),
                &json!({ "to": "a@b.de", "subject": "Hi", "body": "Hallo" }),
            )
            .await
            .unwrap();
        assert_eq!(r["status"], json!("pending_confirmation"));
    }

    #[tokio::test]
    async fn mark_email_handled_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = MarkEmailHandled
            .execute(&ctx(services), &json!({ "id": uuid::Uuid::new_v4() }))
            .await
            .unwrap();
        assert_eq!(r["ok"], json!(true));
    }

    #[tokio::test]
    async fn categorize_email_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = CategorizeEmail
            .execute(&ctx(services), &json!({ "id": uuid::Uuid::new_v4(), "label": "spam" }))
            .await
            .unwrap();
        assert_eq!(r["ok"], json!(true));
    }
}
