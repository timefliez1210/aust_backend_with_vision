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

/// How Josie writes a customer reply. The facts come only from the instruction
/// and the original mail — no invented dates, prices or promises.
const DRAFT_PROMPT: &str = "Du schreibst im Namen von {company} eine Antwort-E-Mail an einen Kunden. \
Deutsch, Sie-Form, freundlich und knapp, ohne Betreffzeile. Beginne mit einer passenden Anrede, \
ende mit \"Mit freundlichen Grüßen\" und darunter \"{company}\". Verwende NUR Fakten aus der \
Anweisung und der ursprünglichen E-Mail — erfinde keine Termine, Preise, Zusagen oder Namen. \
Antworte ausschließlich mit dem E-Mail-Text.";

#[async_trait]
impl Tool for DraftReply {
    fn name(&self) -> &'static str { "draft_reply" }
    fn description(&self) -> &'static str {
        "Schreibt eine Antwort auf eine E-Mail nach Alex' Anweisung und legt sie als ENTWURF im E-Mail-Verlauf ab (im Admin-Postfach sichtbar). Sendet NICHT. Zeig Alex den Text; gesendet wird erst nach seiner Freigabe mit send_email_draft."
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
        let original = ctx.services.emails.get_email(email_id).await?;
        let company = aust_core::tenant::profile(&ctx.db).await?.short_name;

        let original_body: String = original.body_text.as_deref().unwrap_or("").chars().take(4000).collect();
        let prompt = format!(
            "{}\n\n## Ursprüngliche E-Mail\nVon: {}\nBetreff: {}\n\n{}\n\n## Anweisung von Alex\n{}",
            DRAFT_PROMPT.replace("{company}", &company),
            original.from_address.as_deref().unwrap_or("?"),
            original.subject,
            original_body,
            instruction,
        );
        let body = ctx
            .llm
            .chat(crate::llm::ModelTier::Main, &[aust_llm_providers::LlmMessage::user(prompt)])
            .await?;
        let body = body.trim();
        if body.is_empty() {
            return Ok(json!({ "ok": false, "message": "Kein Entwurf erzeugt — bitte erneut versuchen." }));
        }
        let draft = ctx.services.emails.create_reply_draft(email_id, body).await?;
        Ok(json!({
            "ok": true,
            "draft_id": draft.id,
            "to": draft.to_address,
            "subject": draft.subject,
            "body": draft.body,
            "summary": format!("Entwurf an {} im Postfach gespeichert (nicht gesendet)", draft.to_address),
        }))
    }
}

// ── SendEmailDraft (Confirm) ──────────────────────────────────────────────────

/// Send a mailbox draft — a reply from `draft_reply`, a KVA cover letter, or
/// one Alex wrote in the dashboard — through the admin "Senden" path.
pub struct SendEmailDraft;

#[async_trait]
impl Tool for SendEmailDraft {
    fn name(&self) -> &'static str { "send_email_draft" }
    fn description(&self) -> &'static str {
        "Sendet einen gespeicherten E-Mail-Entwurf (z. B. aus draft_reply) im bestehenden Verlauf an den Kunden — mit Threading und ggf. KVA-Anhang, genau wie 'Senden' im Admin-Postfach. 'to' muss der Empfänger des Entwurfs sein. Erfordert Bestätigung."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "draft_id": { "type": "string", "format": "uuid" },
                "to":       { "type": "string", "minLength": 3, "description": "Empfänger laut Entwurf (zur Kontrolle)" }
            },
            "required": ["draft_id", "to"]
        })
    }
    fn safety(&self) -> Safety { Safety::Confirm }
    fn min_role(&self) -> Role { Role::Owner }
    fn summarize(&self, args: &Value) -> String {
        format!("E-Mail-Entwurf jetzt an {} senden?", args["to"].as_str().unwrap_or("?"))
    }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let draft_id = parse_uuid(args, "draft_id", self.name())?;
        let to = parse_str(args, "to", self.name())?;
        if !ctx.confirmed {
            return Ok(pending_confirmation(self.name(), args, self.summarize(args)));
        }
        // Alex confirmed a recipient: refuse if the draft would go somewhere else.
        let draft = ctx.services.emails.get_draft(draft_id).await?;
        if !draft.to_address.trim().eq_ignore_ascii_case(to.trim()) {
            return Ok(json!({
                "ok": false,
                "message": format!("Der Entwurf geht an {}, nicht an {to} — nichts gesendet.", draft.to_address),
            }));
        }
        let sent_to = ctx.services.emails.send_draft(draft_id).await?;
        Ok(json!({ "ok": true, "summary": format!("E-Mail an {sent_to} gesendet") }))
    }
}

// ── SendEmail (Confirm) ───────────────────────────────────────────────────────

pub struct SendEmail;

#[async_trait]
impl Tool for SendEmail {
    fn name(&self) -> &'static str { "send_email" }
    fn description(&self) -> &'static str {
        "Sendet eine eigenständige E-Mail an einen beliebigen Empfänger — ohne dass eine \
         Anfrage oder ein bestehender E-Mail-Verlauf nötig ist (z. B. Nachfassen, \
         Reaktivierung, allgemeine Korrespondenz). Für Antworten innerhalb eines \
         bestehenden Verlaufs nutze stattdessen draft_reply. Erfordert Bestätigung. \
         Anhänge werden derzeit nicht unterstützt."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "to":      { "type": "string", "minLength": 1, "description": "E-Mail-Adresse des Empfängers" },
                "subject": { "type": "string", "minLength": 1 },
                "body":    { "type": "string", "minLength": 1 }
            },
            "required": ["to", "subject", "body"]
        })
    }
    fn safety(&self) -> Safety { Safety::Confirm }
    fn min_role(&self) -> Role { Role::Owner }

    fn summarize(&self, args: &Value) -> String {
        let to = args["to"].as_str().unwrap_or("?");
        let subject = args["subject"].as_str().unwrap_or("(ohne Betreff)");
        format!("E-Mail an {to} senden? Betreff: {subject}")
    }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let to = parse_str(args, "to", self.name())?;
        let subject = parse_str(args, "subject", self.name())?;
        let body = parse_str(args, "body", self.name())?;
        if !ctx.confirmed {
            return Ok(pending_confirmation(self.name(), args, self.summarize(args)));
        }
        match ctx.services.emails.send(to, subject, body).await {
            Ok(()) => Ok(json!({ "ok": true, "to": to, "subject": subject })),
            Err(aust_core::services::ServiceError::Validation(msg)) => {
                Ok(json!({ "ok": false, "message": msg }))
            }
            Err(e) => Err(e.into()),
        }
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

    /// The draft is signed in the company's name — for Aust exactly as before.
    #[sqlx::test(migrations = "../../migrations")]
    async fn draft_reply_returns_draft(pool: sqlx::PgPool) {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let ctx = ToolCtx { db: pool, ..ctx(services) };
        let r = DraftReply
            .execute(
                &ctx,
                &json!({ "email_id": uuid::Uuid::new_v4(), "instruction_de": "Bestätige Termin" }),
            )
            .await
            .unwrap();
        // The model's text (mock: "ok") is saved as a real draft, not sent.
        assert_eq!(r["ok"], true);
        assert_eq!(r["body"], "ok");
        assert!(r["draft_id"].is_string());
        assert!(r["summary"].as_str().unwrap().contains("nicht gesendet"));
    }

    #[tokio::test]
    async fn send_email_draft_needs_confirmation_then_checks_the_recipient() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let args = json!({ "draft_id": uuid::Uuid::new_v4(), "to": "kunde@example.com" });
        let pending = SendEmailDraft.execute(&ctx(services.clone()), &args).await.unwrap();
        assert_eq!(pending["status"], "pending_confirmation");

        let confirmed = ToolCtx { confirmed: true, ..ctx(services.clone()) };
        let sent = SendEmailDraft.execute(&confirmed, &args).await.unwrap();
        assert_eq!(sent["ok"], true);

        let wrong = json!({ "draft_id": uuid::Uuid::new_v4(), "to": "jemand@anders.de" });
        let refused = SendEmailDraft.execute(&ToolCtx { confirmed: true, ..ctx(services) }, &wrong).await.unwrap();
        assert_eq!(refused["ok"], false, "a recipient mismatch must not send");
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
