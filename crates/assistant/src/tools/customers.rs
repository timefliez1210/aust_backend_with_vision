//! Customer tools: get, search, list inquiries, update, add note, merge.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::Result;
use crate::roles::Role;
use super::{parse_str, parse_uuid, pending_confirmation, Safety, Tool, ToolCtx};

// ── GetCustomer ───────────────────────────────────────────────────────────────

pub struct GetCustomer;

#[async_trait]
impl Tool for GetCustomer {
    fn name(&self) -> &'static str { "get_customer" }
    fn description(&self) -> &'static str {
        "Gibt Kundendaten (Name, E-Mail, Telefon) für eine bestimmte Kunden-ID zurück."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "customer_id": { "type": "string", "format": "uuid" } },
            "required": ["customer_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "customer_id", self.name())?;
        let snap = ctx.services.customers.get(id).await?;
        Ok(json!(snap))
    }
}

// ── SearchCustomers ───────────────────────────────────────────────────────────

pub struct SearchCustomers;

#[async_trait]
impl Tool for SearchCustomers {
    fn name(&self) -> &'static str { "search_customers" }
    fn description(&self) -> &'static str { "Sucht Kunden anhand von Name, E-Mail oder Vor-/Nachname." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 50 }
            },
            "required": ["query"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let q = parse_str(args, "query", self.name())?;
        let limit = args["limit"].as_i64().unwrap_or(20).clamp(1, 50) as u32;
        let items = ctx.services.customers.search(q, limit).await?;
        let count = items.len();
        Ok(json!({ "items": items, "count": count }))
    }
}

// ── ListCustomerInquiries ─────────────────────────────────────────────────────

pub struct ListCustomerInquiries;

#[async_trait]
impl Tool for ListCustomerInquiries {
    fn name(&self) -> &'static str { "list_customer_inquiries" }
    fn description(&self) -> &'static str { "Listet alle Anfragen eines Kunden (neueste zuerst)." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "customer_id": { "type": "string", "format": "uuid" } },
            "required": ["customer_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "customer_id", self.name())?;
        let items = ctx.services.customers.list_inquiries_for(id).await?;
        let count = items.len();
        Ok(json!({ "items": items, "count": count }))
    }
}

// ── CreateCustomer ────────────────────────────────────────────────────────────

pub struct CreateCustomer;

#[async_trait]
impl Tool for CreateCustomer {
    fn name(&self) -> &'static str { "create_customer" }
    fn description(&self) -> &'static str {
        "Legt einen neuen Kundendatensatz an — z. B. für telefonische oder Walk-in-Anfragen. \
         Mindestens Vor-/Nachname oder Firmenname ist nötig. customer_type ist 'private' (Standard) \
         oder 'business'. E-Mail und Telefon sind optional."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "first_name":    { "type": "string" },
                "last_name":     { "type": "string" },
                "email":         { "type": "string" },
                "phone":         { "type": "string" },
                "customer_type": { "type": "string", "enum": ["private", "business"] },
                "company_name":  { "type": "string" },
                "salutation":    { "type": "string", "enum": ["Herr", "Frau", "Divers"] }
            }
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let new: aust_core::services::NewCustomer = serde_json::from_value(args.clone())?;
        match ctx.services.customers.create(new).await {
            Ok(snap) => Ok(json!({ "ok": true, "customer": snap })),
            Err(aust_core::services::ServiceError::Validation(msg)) => {
                Ok(json!({ "ok": false, "message": msg }))
            }
            Err(e) => Err(e.into()),
        }
    }
}

// ── UpdateCustomer ────────────────────────────────────────────────────────────

pub struct UpdateCustomer;

#[async_trait]
impl Tool for UpdateCustomer {
    fn name(&self) -> &'static str { "update_customer" }
    fn description(&self) -> &'static str {
        "Aktualisiert Kontaktfelder eines Kunden (Telefon, E-Mail, Name). Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id":    { "type": "string", "format": "uuid" },
                "patch": {
                    "type": "object",
                    "properties": {
                        "phone":      { "type": "string" },
                        "email":      { "type": "string" },
                        "first_name": { "type": "string" },
                        "last_name":  { "type": "string" },
                        "salutation": { "type": "string", "enum": ["Herr", "Frau", "Divers"] }
                    }
                }
            },
            "required": ["id", "patch"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        let patch: aust_core::services::CustomerPatch =
            serde_json::from_value(args["patch"].clone())?;
        let snap = ctx.services.customers.update(id, patch).await?;
        Ok(json!(snap))
    }
}

// ── AddCustomerNote ───────────────────────────────────────────────────────────

pub struct AddCustomerNote;

#[async_trait]
impl Tool for AddCustomerNote {
    fn name(&self) -> &'static str { "add_customer_note" }
    fn description(&self) -> &'static str { "Fügt einem Kunden eine interne Notiz hinzu." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id":   { "type": "string", "format": "uuid" },
                "text": { "type": "string", "minLength": 1 }
            },
            "required": ["id", "text"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        let text = parse_str(args, "text", self.name())?;
        ctx.services.customers.add_note(id, text).await?;
        Ok(json!({ "ok": true }))
    }
}

// ── MergeCustomers (Confirm) ──────────────────────────────────────────────────

pub struct MergeCustomers;

#[async_trait]
impl Tool for MergeCustomers {
    fn name(&self) -> &'static str { "merge_customers" }
    fn description(&self) -> &'static str {
        "Führt zwei Kundendatensätze zusammen (Anfragen werden umgehängt). Erfordert Bestätigung."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "keep_id":  { "type": "string", "format": "uuid" },
                "merge_id": { "type": "string", "format": "uuid" }
            },
            "required": ["keep_id", "merge_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Confirm }
    fn min_role(&self) -> Role { Role::Owner }

    fn summarize(&self, args: &Value) -> String {
        let keep = args["keep_id"].as_str().unwrap_or("?");
        let merge = args["merge_id"].as_str().unwrap_or("?");
        format!("Kunde {merge} in {keep} zusammenführen? Alle Anfragen werden umgehängt.")
    }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let keep_id = parse_uuid(args, "keep_id", self.name())?;
        let merge_id = parse_uuid(args, "merge_id", self.name())?;
        if !ctx.confirmed {
            return Ok(pending_confirmation(self.name(), args, self.summarize(args)));
        }
        let snapshot = ctx.services.customers.merge(keep_id, merge_id).await?;
        Ok(json!({
            "status": "merged",
            "keep_id": keep_id,
            "merged_id": merge_id,
            "kept_customer": snapshot,
        }))
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
    async fn search_returns_results() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = SearchCustomers.execute(&ctx(services), &json!({ "query": "müller" })).await.unwrap();
        assert_eq!(r["count"], json!(1));
    }

    #[tokio::test]
    async fn list_inquiries_returns_count() {
        let id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), id, uuid::Uuid::new_v4());
        let r = ListCustomerInquiries.execute(&ctx(services), &json!({ "customer_id": id })).await.unwrap();
        assert_eq!(r["count"], json!(0));
    }

    #[tokio::test]
    async fn create_customer_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = CreateCustomer
            .execute(&ctx(services), &json!({ "first_name": "Clemens", "last_name": "Fabig", "phone": "09286284042" }))
            .await
            .unwrap();
        assert_eq!(r["ok"], json!(true));
        assert!(r["customer"]["id"].is_string());
    }

    #[tokio::test]
    async fn update_customer_ok() {
        let id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), id, uuid::Uuid::new_v4());
        let r = UpdateCustomer
            .execute(&ctx(services), &json!({ "id": id, "patch": { "phone": "+49170111" } }))
            .await
            .unwrap();
        assert_eq!(r["id"], json!(id.to_string()));
    }

    #[tokio::test]
    async fn add_customer_note_ok() {
        let id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), id, uuid::Uuid::new_v4());
        let r = AddCustomerNote
            .execute(&ctx(services), &json!({ "id": id, "text": "Rückruf vereinbart" }))
            .await
            .unwrap();
        assert_eq!(r["ok"], json!(true));
    }

    #[tokio::test]
    async fn merge_customers_pending() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = MergeCustomers
            .execute(
                &ctx(services),
                &json!({ "keep_id": uuid::Uuid::new_v4(), "merge_id": uuid::Uuid::new_v4() }),
            )
            .await
            .unwrap();
        assert_eq!(r["status"], json!("pending_confirmation"));
    }
}
