//! Invoice tools: list, get, reminders, status, payment, send, void.

use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::error::Result;
use crate::roles::Role;
use super::{parse_str, parse_uuid, pending_confirmation, Safety, Tool, ToolCtx};

// ── ListInvoices ──────────────────────────────────────────────────────────────

pub struct ListInvoices;

#[async_trait]
impl Tool for ListInvoices {
    fn name(&self) -> &'static str { "list_invoices" }
    fn description(&self) -> &'static str {
        "Listet Rechnungen auf, optional gefiltert nach Status ('draft', 'sent', 'paid', 'overdue'). Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({ "type": "object", "properties": { "status": { "type": "string" } } })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let status = args["status"].as_str();
        let items = ctx.services.invoices.list(status).await?;
        let count = items.len();
        Ok(json!({ "invoices": items, "count": count }))
    }
}

// ── GetInvoice ────────────────────────────────────────────────────────────────

pub struct GetInvoice;

#[async_trait]
impl Tool for GetInvoice {
    fn name(&self) -> &'static str { "get_invoice" }
    fn description(&self) -> &'static str { "Lädt eine Rechnung anhand ihrer ID." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string", "format": "uuid" } },
            "required": ["id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        let inv = ctx.services.invoices.get(id).await?;
        Ok(json!(inv))
    }
}

// ── ListInvoiceReminders ──────────────────────────────────────────────────────

pub struct ListInvoiceReminders;

#[async_trait]
impl Tool for ListInvoiceReminders {
    fn name(&self) -> &'static str { "list_invoice_reminders" }
    fn description(&self) -> &'static str { "Listet alle Mahnungen einer Rechnung." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "invoice_id": { "type": "string", "format": "uuid" } },
            "required": ["invoice_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "invoice_id", self.name())?;
        let items = ctx.services.invoices.list_reminders(id).await?;
        let count = items.len();
        Ok(json!({ "reminders": items, "count": count }))
    }
}

// ── CreateInvoice ─────────────────────────────────────────────────────────────

pub struct CreateInvoice;

#[async_trait]
impl Tool for CreateInvoice {
    fn name(&self) -> &'static str { "create_invoice" }
    fn description(&self) -> &'static str {
        "Erstellt eine Rechnung aus einer abgeschlossenen Anfrage. Nur für Inhaber. (Stub: aktuelles Schema unterstützt nur Standardpfad)"
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" },
                "line_items": { "type": "array" }
            },
            "required": ["inquiry_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let inquiry_id = parse_uuid(args, "inquiry_id", self.name())?;
        let summary = ctx.services.invoices.create_from_inquiry(inquiry_id).await?;
        Ok(serde_json::json!(summary))
    }
}

// ── UpdateInvoiceStatus ───────────────────────────────────────────────────────

pub struct UpdateInvoiceStatus;

#[async_trait]
impl Tool for UpdateInvoiceStatus {
    fn name(&self) -> &'static str { "update_invoice_status" }
    fn description(&self) -> &'static str {
        "Aktualisiert den Status einer Rechnung (paid, overdue, written_off, …). Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id":     { "type": "string", "format": "uuid" },
                "status": { "type": "string", "minLength": 1 }
            },
            "required": ["id", "status"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        let status = parse_str(args, "status", self.name())?;
        let inv = ctx.services.invoices.update_status(id, status).await?;
        Ok(json!(inv))
    }
}

// ── RecordPayment ─────────────────────────────────────────────────────────────

pub struct RecordPayment;

#[async_trait]
impl Tool for RecordPayment {
    fn name(&self) -> &'static str { "record_payment" }
    fn description(&self) -> &'static str { "Verbucht einen Zahlungseingang auf einer Rechnung." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "invoice_id":   { "type": "string", "format": "uuid" },
                "amount_cents": { "type": "integer", "minimum": 0 },
                "date":         { "type": "string", "format": "date" },
                "method":       { "type": "string", "minLength": 1 },
                "ref_text":     { "type": "string" }
            },
            "required": ["invoice_id", "amount_cents", "date", "method"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let invoice_id = parse_uuid(args, "invoice_id", self.name())?;
        let amount = args["amount_cents"].as_i64().unwrap_or(0);
        let date: NaiveDate = args["date"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| crate::error::AssistantError::ArgValidation {
                tool: self.name().to_string(),
                message: "date must be YYYY-MM-DD".to_string(),
            })?;
        let method = parse_str(args, "method", self.name())?;
        let ref_text = args["ref_text"].as_str();
        let payment_id = ctx.services.invoices.record_payment(invoice_id, amount, date, method, ref_text).await?;
        Ok(json!({ "ok": true, "payment_id": payment_id }))
    }
}

// ── SendInvoice (Confirm) ─────────────────────────────────────────────────────

pub struct SendInvoice;

#[async_trait]
impl Tool for SendInvoice {
    fn name(&self) -> &'static str { "send_invoice" }
    fn description(&self) -> &'static str { "Sendet eine Rechnung per E-Mail. Erfordert Bestätigung." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "invoice_id": { "type": "string", "format": "uuid" } },
            "required": ["invoice_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Confirm }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, _ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "invoice_id", self.name())?;
        Ok(pending_confirmation(self.name(), args, format!("Rechnung {id} senden?")))
    }
}

// ── SendPaymentReminder (Confirm) ─────────────────────────────────────────────

pub struct SendPaymentReminder;

#[async_trait]
impl Tool for SendPaymentReminder {
    fn name(&self) -> &'static str { "send_payment_reminder" }
    fn description(&self) -> &'static str {
        "Sendet eine Zahlungserinnerung (Stufe 1/2/3 in steigender Schärfe). Erfordert Bestätigung."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "invoice_id": { "type": "string", "format": "uuid" },
                "level":      { "type": "integer", "minimum": 1, "maximum": 3 }
            },
            "required": ["invoice_id", "level"]
        })
    }
    fn safety(&self) -> Safety { Safety::Confirm }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, _ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "invoice_id", self.name())?;
        let level = args["level"].as_i64().unwrap_or(1);
        Ok(pending_confirmation(
            self.name(),
            args,
            format!("Mahnung Stufe {level} für Rechnung {id} senden?"),
        ))
    }
}

// ── VoidInvoice (Confirm) ─────────────────────────────────────────────────────

pub struct VoidInvoice;

#[async_trait]
impl Tool for VoidInvoice {
    fn name(&self) -> &'static str { "void_invoice" }
    fn description(&self) -> &'static str { "Storniert eine Rechnung. Erfordert Bestätigung." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id":     { "type": "string", "format": "uuid" },
                "reason": { "type": "string", "minLength": 1 }
            },
            "required": ["id", "reason"]
        })
    }
    fn safety(&self) -> Safety { Safety::Confirm }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, _ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        Ok(pending_confirmation(self.name(), args, format!("Rechnung {id} stornieren?")))
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
        }
    }

    #[tokio::test]
    async fn get_invoice_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = GetInvoice.execute(&ctx(services), &json!({ "id": uuid::Uuid::new_v4() })).await.unwrap();
        assert!(r["invoice_number"].is_string());
    }

    #[tokio::test]
    async fn list_reminders_returns_count() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = ListInvoiceReminders
            .execute(&ctx(services), &json!({ "invoice_id": uuid::Uuid::new_v4() }))
            .await
            .unwrap();
        assert_eq!(r["count"], json!(0));
    }

    #[tokio::test]
    async fn create_invoice_returns_summary() {
        let inquiry_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = CreateInvoice.execute(&ctx(services), &json!({ "inquiry_id": inquiry_id })).await.unwrap();
        // mock returns "draft" — real impl returns "ready"; just check it's a string
        assert!(r["status"].is_string());
        assert!(r["invoice_number"].is_string());
    }

    #[tokio::test]
    async fn update_invoice_status_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = UpdateInvoiceStatus
            .execute(&ctx(services), &json!({ "id": uuid::Uuid::new_v4(), "status": "paid" }))
            .await
            .unwrap();
        assert_eq!(r["status"], json!("paid"));
    }

    #[tokio::test]
    async fn record_payment_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = RecordPayment
            .execute(
                &ctx(services),
                &json!({
                    "invoice_id": uuid::Uuid::new_v4(),
                    "amount_cents": 1000,
                    "date": "2026-06-01",
                    "method": "bank"
                }),
            )
            .await
            .unwrap();
        assert_eq!(r["ok"], json!(true));
        assert!(r["payment_id"].is_string(), "payment_id uuid should be returned");
    }

    #[tokio::test]
    async fn send_invoice_pending() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = SendInvoice.execute(&ctx(services), &json!({ "invoice_id": uuid::Uuid::new_v4() })).await.unwrap();
        assert_eq!(r["status"], json!("pending_confirmation"));
    }

    #[tokio::test]
    async fn send_payment_reminder_pending() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = SendPaymentReminder
            .execute(&ctx(services), &json!({ "invoice_id": uuid::Uuid::new_v4(), "level": 1 }))
            .await
            .unwrap();
        assert_eq!(r["status"], json!("pending_confirmation"));
    }

    #[tokio::test]
    async fn void_invoice_pending() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = VoidInvoice
            .execute(&ctx(services), &json!({ "id": uuid::Uuid::new_v4(), "reason": "Fehler" }))
            .await
            .unwrap();
        assert_eq!(r["status"], json!("pending_confirmation"));
    }
}
