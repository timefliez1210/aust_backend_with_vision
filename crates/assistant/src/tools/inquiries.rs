//! Inquiry tools: read, search, mutate inquiries via the service bridge.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::Result;
use crate::roles::Role;
use super::{
    parse_str, parse_uuid, pending_confirmation, Safety, Tool, ToolCtx,
};

// ── CreateInquiry ─────────────────────────────────────────────────────────────

/// Create a new inquiry for an existing customer (phone / on-site intake).
pub struct CreateInquiry;

#[async_trait]
impl Tool for CreateInquiry {
    fn name(&self) -> &'static str { "create_inquiry" }
    fn description(&self) -> &'static str {
        "Legt eine NEUE Anfrage (Umzugsauftrag) für einen bestehenden Kunden an — z.B. nach Telefonat oder Besichtigung. Den Kunden ggf. zuerst mit create_customer anlegen und dessen ID hier als 'customer_id' verwenden. Adressen optional. Status wird 'pending'. Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        let addr = json!({
            "type": "object",
            "properties": {
                "street":       { "type": "string" },
                "house_number": { "type": "string" },
                "postal_code":  { "type": "string" },
                "city":         { "type": "string" },
                "floor":        { "type": "string" },
                "elevator":     { "type": "boolean" }
            }
        });
        json!({
            "type": "object",
            "properties": {
                "customer_id":          { "type": "string", "format": "uuid" },
                "service_type":         { "type": "string", "description": "z.B. umzug, montage, entrümpelung" },
                "scheduled_date":       { "type": "string", "format": "date" },
                "estimated_volume_m3":  { "type": "number", "minimum": 0 },
                "notes":                { "type": "string" },
                "origin":               addr,
                "destination":          addr
            },
            "required": ["customer_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let customer_id = parse_uuid(args, "customer_id", self.name())?;

        let parse_addr = |v: &Value| -> Option<aust_core::services::NewAddress> {
            if !v.is_object() {
                return None;
            }
            Some(aust_core::services::NewAddress {
                street: v["street"].as_str().map(str::to_string),
                house_number: v["house_number"].as_str().map(str::to_string),
                postal_code: v["postal_code"].as_str().map(str::to_string),
                city: v["city"].as_str().map(str::to_string),
                floor: v["floor"].as_str().map(str::to_string),
                elevator: v["elevator"].as_bool(),
            })
        };

        let new = aust_core::services::NewInquiry {
            customer_id,
            service_type: args["service_type"].as_str().map(str::to_string),
            scheduled_date: args["scheduled_date"].as_str().and_then(|s| s.parse().ok()),
            estimated_volume_m3: args["estimated_volume_m3"].as_f64(),
            notes: args["notes"].as_str().map(str::to_string),
            services: None,
            origin: parse_addr(&args["origin"]),
            destination: parse_addr(&args["destination"]),
        };
        let resp = ctx.services.inquiries.create_inquiry(new).await?;
        Ok(serde_json::to_value(&resp)?)
    }
}

// ── GetInquiry ────────────────────────────────────────────────────────────────

/// Fetch a single inquiry by UUID and return its key fields as JSON.
pub struct GetInquiry;

#[async_trait]
impl Tool for GetInquiry {
    fn name(&self) -> &'static str { "get_inquiry" }
    fn description(&self) -> &'static str {
        "Ruft eine Anfrage (Umzugsauftrag) anhand ihrer ID ab. Gibt Status, Kundendaten, Adressen, Volumen, Termin und Angebotsstatus zurück."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid", "description": "UUID der Anfrage" }
            },
            "required": ["inquiry_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "inquiry_id", self.name())?;
        let resp = ctx.services.inquiries.get_inquiry(id).await?;
        Ok(serde_json::to_value(&resp)?)
    }
}

// ── ListInquiries ─────────────────────────────────────────────────────────────

pub struct ListInquiries;

#[async_trait]
impl Tool for ListInquiries {
    fn name(&self) -> &'static str { "list_inquiries" }
    fn description(&self) -> &'static str {
        "Listet Anfragen, optional nach Status gefiltert (z.B. 'pending', 'estimated', 'offer_sent')."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": { "type": "string" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
            }
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let status = args["status"].as_str();
        let limit = args["limit"].as_i64().unwrap_or(50).clamp(1, 200) as u32;
        let items = ctx.services.inquiries.list_inquiries(status, limit).await?;
        let count = items.len();
        Ok(json!({ "items": items, "count": count }))
    }
}

// ── SearchInquiries ───────────────────────────────────────────────────────────

pub struct SearchInquiries;

#[async_trait]
impl Tool for SearchInquiries {
    fn name(&self) -> &'static str { "search_inquiries" }
    fn description(&self) -> &'static str {
        "Sucht Anfragen per Volltext über Kundenname, Adressen und Notizen."
    }
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
        let query = parse_str(args, "query", self.name())?;
        let limit = args["limit"].as_i64().unwrap_or(20).clamp(1, 50) as u32;
        let items = ctx.services.inquiries.search_inquiries(query, limit).await?;
        let count = items.len();
        Ok(json!({ "items": items, "count": count }))
    }
}

// ── AddInquiryNote ────────────────────────────────────────────────────────────

pub struct AddInquiryNote;

#[async_trait]
impl Tool for AddInquiryNote {
    fn name(&self) -> &'static str { "add_inquiry_note" }
    fn description(&self) -> &'static str {
        "Fügt einer Anfrage eine interne Notiz hinzu (mit Zeitstempel und Rolle des Autors)."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" },
                "text": { "type": "string", "minLength": 1 }
            },
            "required": ["inquiry_id", "text"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "inquiry_id", self.name())?;
        let text = parse_str(args, "text", self.name())?;
        let role = ctx.role.to_string();
        ctx.services.inquiries.add_note(id, text, &role).await?;
        Ok(json!({ "ok": true, "inquiry_id": id }))
    }
}

// ── UpdateInquiryStatus ───────────────────────────────────────────────────────

pub struct UpdateInquiryStatus;

#[async_trait]
impl Tool for UpdateInquiryStatus {
    fn name(&self) -> &'static str { "update_inquiry_status" }
    fn description(&self) -> &'static str {
        "Ändert den Status einer Anfrage. Übergänge werden validiert. Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" },
                "new_status": { "type": "string", "minLength": 1 },
                "reason": { "type": "string" }
            },
            "required": ["inquiry_id", "new_status"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "inquiry_id", self.name())?;
        let new_status = parse_str(args, "new_status", self.name())?;
        let reason = args["reason"].as_str();
        let resp = ctx.services.inquiries.update_status(id, new_status, reason).await?;
        Ok(json!({ "ok": true, "status": resp.status }))
    }
}

// ── SetInquiryServices ────────────────────────────────────────────────────────

pub struct SetInquiryServices;

#[async_trait]
impl Tool for SetInquiryServices {
    fn name(&self) -> &'static str { "set_inquiry_services" }
    fn description(&self) -> &'static str {
        "Setzt die Service-Flags (Packen, Montage, Lagerung, etc.) für eine Anfrage. Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" },
                "services": {
                    "type": "object",
                    "properties": {
                        "packing": { "type": "boolean" },
                        "assembly": { "type": "boolean" },
                        "disassembly": { "type": "boolean" },
                        "storage": { "type": "boolean" },
                        "disposal": { "type": "boolean" },
                        "parking_ban_origin": { "type": "boolean" },
                        "parking_ban_destination": { "type": "boolean" },
                        "transporter": { "type": "boolean" }
                    },
                    "additionalProperties": false
                }
            },
            "required": ["inquiry_id", "services"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "inquiry_id", self.name())?;
        let services: aust_core::models::Services =
            serde_json::from_value(args["services"].clone())?;
        ctx.services.inquiries.set_services(id, services).await?;
        Ok(json!({ "ok": true, "inquiry_id": id }))
    }
}

// ── RequestInfoFromCustomer (Confirm) ─────────────────────────────────────────

pub struct RequestInfoFromCustomer;

#[async_trait]
impl Tool for RequestInfoFromCustomer {
    fn name(&self) -> &'static str { "request_info_from_customer" }
    fn description(&self) -> &'static str {
        "Fordert vom Kunden zusätzliche Informationen per E-Mail an. Erfordert Bestätigung."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" },
                "draft_email_de": { "type": "string", "minLength": 1 }
            },
            "required": ["inquiry_id", "draft_email_de"]
        })
    }
    fn safety(&self) -> Safety { Safety::Confirm }
    fn min_role(&self) -> Role { Role::Owner }

    fn summarize(&self, args: &Value) -> String {
        let id = args["inquiry_id"].as_str().unwrap_or("?");
        format!("Infoanforderung per E-Mail an Kunden für Anfrage {id} senden?")
    }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "inquiry_id", self.name())?;
        let draft = parse_str(args, "draft_email_de", self.name())?;
        if !ctx.confirmed {
            return Ok(pending_confirmation(self.name(), args, self.summarize(args)));
        }
        // The actual draft goes out via the email pipeline once we add
        // EmailService::send (next slice). For now we transition the inquiry
        // to info_requested and record the draft as a note so the action is
        // observable even before SMTP is wired.
        ctx.services
            .inquiries
            .add_note(id, &format!("Info-Anfrage an Kunden (Entwurf):\n{draft}"), "agent")
            .await?;
        ctx.services
            .inquiries
            .update_status(id, "info_requested", Some("Agent requested info from customer"))
            .await?;
        Ok(json!({
            "status": "info_requested",
            "inquiry_id": id,
            "note": "Status auf info_requested gesetzt; E-Mail-Versand erfolgt nach Wiring der EmailService::send Capability."
        }))
    }
}

// ── CancelInquiry (Confirm) ───────────────────────────────────────────────────

pub struct CancelInquiry;

#[async_trait]
impl Tool for CancelInquiry {
    fn name(&self) -> &'static str { "cancel_inquiry" }
    fn description(&self) -> &'static str {
        "Storniert eine Anfrage. Erfordert Bestätigung."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" },
                "reason": { "type": "string", "minLength": 1 }
            },
            "required": ["inquiry_id", "reason"]
        })
    }
    fn safety(&self) -> Safety { Safety::Confirm }
    fn min_role(&self) -> Role { Role::Owner }

    fn summarize(&self, args: &Value) -> String {
        let id = args["inquiry_id"].as_str().unwrap_or("?");
        let reason = args["reason"].as_str().unwrap_or("ohne Grund");
        format!("Anfrage {id} stornieren? Grund: {reason}")
    }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "inquiry_id", self.name())?;
        let reason = parse_str(args, "reason", self.name())?;
        if !ctx.confirmed {
            return Ok(pending_confirmation(self.name(), args, self.summarize(args)));
        }
        ctx.services.inquiries.cancel_inquiry(id, reason).await?;
        Ok(json!({ "status": "canceled", "inquiry_id": id }))
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
    async fn list_inquiries_returns_count() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = ListInquiries.execute(&ctx(services), &json!({})).await.unwrap();
        assert_eq!(result["count"], json!(0));
    }

    #[tokio::test]
    async fn search_inquiries_returns_count() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = SearchInquiries
            .execute(&ctx(services), &json!({ "query": "müller" }))
            .await
            .unwrap();
        assert_eq!(result["count"], json!(0));
    }

    #[tokio::test]
    async fn add_inquiry_note_succeeds() {
        let inquiry_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = AddInquiryNote
            .execute(&ctx(services), &json!({ "inquiry_id": inquiry_id, "text": "Kunde angerufen" }))
            .await
            .unwrap();
        assert_eq!(result["ok"], json!(true));
    }

    #[tokio::test]
    async fn update_inquiry_status_returns_status() {
        let inquiry_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = UpdateInquiryStatus
            .execute(&ctx(services), &json!({ "inquiry_id": inquiry_id, "new_status": "estimated" }))
            .await
            .unwrap();
        assert_eq!(result["ok"], json!(true));
    }

    #[tokio::test]
    async fn set_inquiry_services_returns_ok() {
        let inquiry_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = SetInquiryServices
            .execute(
                &ctx(services),
                &json!({ "inquiry_id": inquiry_id, "services": { "packing": true } }),
            )
            .await
            .unwrap();
        assert_eq!(result["ok"], json!(true));
    }

    #[tokio::test]
    async fn request_info_from_customer_is_pending() {
        let inquiry_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let args = json!({ "inquiry_id": inquiry_id, "draft_email_de": "Bitte sende Fotos." });
        let result = RequestInfoFromCustomer.execute(&ctx(services), &args).await.unwrap();
        assert_eq!(result["status"], json!("pending_confirmation"));
        assert_eq!(result["tool_name"], json!("request_info_from_customer"));
        assert!(!result["summary_de"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancel_inquiry_is_pending() {
        let inquiry_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let args = json!({ "inquiry_id": inquiry_id, "reason": "Kunde abgesagt" });
        let result = CancelInquiry.execute(&ctx(services), &args).await.unwrap();
        assert_eq!(result["status"], json!("pending_confirmation"));
    }
}
