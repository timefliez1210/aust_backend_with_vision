//! Offer tools: preview (pure, no side effects) and commit (full pipeline).

use async_trait::async_trait;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{AssistantError, Result};
use crate::roles::Role;
use super::{Safety, Tool, ToolCtx};

// ── PreviewOffer ──────────────────────────────────────────────────────────────

/// Owner-only: preview pricing for an inquiry **without** writing anything.
///
/// Returns an `OfferPreview` with computed line items, persons, hours, and totals.
/// Safe to call multiple times with different overrides to explore pricing.
pub struct PreviewOffer;

#[async_trait]
impl Tool for PreviewOffer {
    fn name(&self) -> &'static str {
        "preview_offer"
    }

    fn description(&self) -> &'static str {
        "Berechnet eine Angebotsvorschau für eine Anfrage — ohne etwas zu speichern oder zu senden. Gibt Positionen, Personenanzahl, Stunden und Gesamtpreise zurück. Nur für Inhaber verfügbar."
    }

    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": {
                    "type": "string",
                    "format": "uuid",
                    "description": "UUID der Anfrage"
                },
                "overrides": {
                    "type": "object",
                    "description": "Optionale Preisüberschreibungen",
                    "properties": {
                        "crew_size": { "type": "integer", "minimum": 1 },
                        "hours": { "type": "number", "minimum": 0.5 },
                        "rate_eur": { "type": "number", "minimum": 0 },
                        "price_netto_cents": { "type": "integer", "minimum": 0 }
                    },
                    "additionalProperties": false
                }
            },
            "required": ["inquiry_id"]
        })
    }

    fn safety(&self) -> Safety {
        Safety::Read
    }

    fn min_role(&self) -> Role {
        Role::Owner
    }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let inquiry_id: Uuid = args["inquiry_id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| AssistantError::ArgValidation {
                tool: self.name().to_string(),
                message: "inquiry_id must be a valid UUID".to_string(),
            })?;

        let overrides = parse_overrides(&args["overrides"]);

        let preview = ctx.services.offers.preview_offer(inquiry_id, overrides).await?;
        Ok(serde_json::to_value(&preview)?)
    }
}

// ── CommitOfferDraft ──────────────────────────────────────────────────────────

/// Owner-only: run the full offer pipeline and store a draft.
///
/// Renders XLSX, converts to PDF, uploads to S3, inserts the offer DB record.
/// Does NOT send the offer to the customer.
pub struct CommitOfferDraft;

#[async_trait]
impl Tool for CommitOfferDraft {
    fn name(&self) -> &'static str {
        "commit_offer_draft"
    }

    fn description(&self) -> &'static str {
        "Erstellt einen Angebotsentwurf für eine Anfrage (PDF, S3, Datenbank) — wird NICHT an den Kunden gesendet. Nur für Inhaber verfügbar."
    }

    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": {
                    "type": "string",
                    "format": "uuid",
                    "description": "UUID der Anfrage, für die ein Angebot erstellt werden soll"
                },
                "overrides": {
                    "type": "object",
                    "description": "Optionale Preisüberschreibungen",
                    "properties": {
                        "crew_size": { "type": "integer", "minimum": 1 },
                        "hours": { "type": "number", "minimum": 0.5 },
                        "rate_eur": { "type": "number", "minimum": 0 },
                        "price_netto_cents": { "type": "integer", "minimum": 0 }
                    },
                    "additionalProperties": false
                }
            },
            "required": ["inquiry_id"]
        })
    }

    fn safety(&self) -> Safety {
        Safety::Write
    }

    fn min_role(&self) -> Role {
        Role::Owner
    }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let inquiry_id: Uuid = args["inquiry_id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| AssistantError::ArgValidation {
                tool: self.name().to_string(),
                message: "inquiry_id must be a valid UUID".to_string(),
            })?;

        let overrides = parse_overrides(&args["overrides"]);

        let draft = ctx.services.offers.commit_offer_draft(inquiry_id, overrides).await?;
        Ok(serde_json::to_value(&draft)?)
    }
}

// ── DraftOffer (deprecated stub) ──────────────────────────────────────────────

/// Deprecated: use `CommitOfferDraft` instead.
///
/// Kept so that any external caller referencing the old tool name continues to
/// compile. The registry still registers `CommitOfferDraft`; this struct is
/// retained only for backward-compat test expectations that may reference the type.
#[deprecated(note = "use CommitOfferDraft")]
pub struct DraftOffer;

#[async_trait]
#[allow(deprecated)]
impl Tool for DraftOffer {
    fn name(&self) -> &'static str {
        "draft_offer"
    }

    fn description(&self) -> &'static str {
        "Veraltet — verwende commit_offer_draft."
    }

    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" }
            },
            "required": ["inquiry_id"]
        })
    }

    fn safety(&self) -> Safety {
        Safety::Write
    }

    fn min_role(&self) -> Role {
        Role::Owner
    }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let inquiry_id: Uuid = args["inquiry_id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| AssistantError::ArgValidation {
                tool: self.name().to_string(),
                message: "inquiry_id must be a valid UUID".to_string(),
            })?;
        let draft = ctx.services.offers.commit_offer_draft(inquiry_id, None).await?;
        Ok(serde_json::to_value(&draft)?)
    }
}

// ── RecomputeOffer ────────────────────────────────────────────────────────────

pub struct RecomputeOffer;

#[async_trait]
impl Tool for RecomputeOffer {
    fn name(&self) -> &'static str { "recompute_offer" }
    fn description(&self) -> &'static str {
        "Berechnet ein Angebot mit angepassten Überschreibungen neu und speichert es als Entwurf. Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" },
                "overrides": {
                    "type": "object",
                    "properties": {
                        "crew_size": { "type": "integer", "minimum": 1 },
                        "hours": { "type": "number", "minimum": 0.5 },
                        "rate_eur": { "type": "number", "minimum": 0 },
                        "price_netto_cents": { "type": "integer", "minimum": 0 }
                    },
                    "additionalProperties": false
                }
            },
            "required": ["inquiry_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let inquiry_id = super::parse_uuid(args, "inquiry_id", self.name())?;
        let overrides = parse_overrides(&args["overrides"]);
        let draft = ctx.services.offers.commit_offer_draft(inquiry_id, overrides).await?;
        Ok(serde_json::to_value(&draft)?)
    }
}

// ── ApplyNaturalLanguageOverride ──────────────────────────────────────────────

pub struct ApplyNaturalLanguageOverride;

#[async_trait]
impl Tool for ApplyNaturalLanguageOverride {
    fn name(&self) -> &'static str { "apply_nl_override" }
    fn description(&self) -> &'static str {
        "Wendet eine Klartext-Anweisung auf das Angebot an (z.B. '5 % Rabatt'). Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" },
                "instruction_de": { "type": "string", "minLength": 1 }
            },
            "required": ["inquiry_id", "instruction_de"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let inquiry_id = super::parse_uuid(args, "inquiry_id", self.name())?;
        let instruction = args["instruction_de"]
            .as_str()
            .ok_or_else(|| crate::error::AssistantError::ArgValidation {
                tool: self.name().to_string(),
                message: "instruction_de is required".to_string(),
            })?;
        let draft = ctx.services.offers.apply_nl_override(inquiry_id, instruction).await?;
        Ok(serde_json::to_value(&draft)?)
    }
}

// ── SendOfferToCustomer (Confirm) ─────────────────────────────────────────────

pub struct SendOfferToCustomer;

#[async_trait]
impl Tool for SendOfferToCustomer {
    fn name(&self) -> &'static str { "send_offer_to_customer" }
    fn description(&self) -> &'static str {
        "Sendet das aktive Angebot per E-Mail an den Kunden. Erfordert Bestätigung."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" },
                "email_template": { "type": "string" }
            },
            "required": ["inquiry_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Confirm }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, _ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let inquiry_id = super::parse_uuid(args, "inquiry_id", self.name())?;
        Ok(super::pending_confirmation(
            self.name(),
            args,
            format!("Angebot für Anfrage {inquiry_id} an Kunden senden?"),
        ))
    }
}

// ── CancelOffer (Confirm) ─────────────────────────────────────────────────────

pub struct CancelOffer;

#[async_trait]
impl Tool for CancelOffer {
    fn name(&self) -> &'static str { "cancel_offer" }
    fn description(&self) -> &'static str {
        "Storniert das aktive Angebot. Erfordert Bestätigung."
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

    async fn execute(&self, _ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let inquiry_id = super::parse_uuid(args, "inquiry_id", self.name())?;
        Ok(super::pending_confirmation(
            self.name(),
            args,
            format!("Angebot der Anfrage {inquiry_id} stornieren?"),
        ))
    }
}

// ── GetOfferHistory ───────────────────────────────────────────────────────────

pub struct GetOfferHistory;

#[async_trait]
impl Tool for GetOfferHistory {
    fn name(&self) -> &'static str { "get_offer_history" }
    fn description(&self) -> &'static str {
        "Listet alle Angebotsversionen einer Anfrage (neueste zuerst)."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" }
            },
            "required": ["inquiry_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let inquiry_id = super::parse_uuid(args, "inquiry_id", self.name())?;
        let versions = ctx.services.offers.list_offer_versions(inquiry_id).await?;
        let count = versions.len();
        Ok(json!({ "versions": versions, "count": count }))
    }
}

// ── MarkOfferAccepted ─────────────────────────────────────────────────────────

pub struct MarkOfferAccepted;

#[async_trait]
impl Tool for MarkOfferAccepted {
    fn name(&self) -> &'static str { "mark_offer_accepted" }
    fn description(&self) -> &'static str {
        "Markiert das Angebot als angenommen (z.B. nach telefonischer Zusage). Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" },
                "source": { "type": "string", "minLength": 1 }
            },
            "required": ["inquiry_id", "source"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let inquiry_id = super::parse_uuid(args, "inquiry_id", self.name())?;
        let source = args["source"].as_str().unwrap_or("manual");
        ctx.services.offers.mark_offer_accepted(inquiry_id, source).await?;
        Ok(json!({ "ok": true, "inquiry_id": inquiry_id }))
    }
}

// ── MarkOfferRejected ─────────────────────────────────────────────────────────

pub struct MarkOfferRejected;

#[async_trait]
impl Tool for MarkOfferRejected {
    fn name(&self) -> &'static str { "mark_offer_rejected" }
    fn description(&self) -> &'static str {
        "Markiert das Angebot als abgelehnt. Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" },
                "source": { "type": "string", "minLength": 1 },
                "reason": { "type": "string" }
            },
            "required": ["inquiry_id", "source"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let inquiry_id = super::parse_uuid(args, "inquiry_id", self.name())?;
        let source = args["source"].as_str().unwrap_or("manual");
        let reason = args["reason"].as_str();
        ctx.services.offers.mark_offer_rejected(inquiry_id, source, reason).await?;
        Ok(json!({ "ok": true, "inquiry_id": inquiry_id }))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_overrides(v: &Value) -> Option<aust_core::services::OfferOverrides> {
    if v.is_null() || !v.is_object() {
        return None;
    }
    let mut o = aust_core::services::OfferOverrides::default();
    if let Some(n) = v["crew_size"].as_u64() {
        o.crew_size = Some(n as u32);
    }
    if let Some(n) = v["hours"].as_f64() {
        o.hours = Some(n);
    }
    if let Some(n) = v["rate_eur"].as_f64() {
        o.rate_eur = Some(n);
    }
    if let Some(n) = v["price_netto_cents"].as_i64() {
        o.price_netto_cents = Some(n);
    }
    Some(o)
}

#[cfg(test)]
mod new_offer_tests {
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
    async fn recompute_offer_returns_draft() {
        let inquiry_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = RecomputeOffer
            .execute(&ctx(services), &json!({ "inquiry_id": inquiry_id }))
            .await
            .unwrap();
        assert_eq!(result["status"], json!("draft"));
    }

    #[tokio::test]
    async fn apply_nl_override_returns_draft() {
        let inquiry_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = ApplyNaturalLanguageOverride
            .execute(&ctx(services), &json!({ "inquiry_id": inquiry_id, "instruction_de": "5% Rabatt" }))
            .await
            .unwrap();
        assert_eq!(result["status"], json!("draft"));
    }

    #[tokio::test]
    async fn send_offer_to_customer_pending() {
        let inquiry_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = SendOfferToCustomer
            .execute(&ctx(services), &json!({ "inquiry_id": inquiry_id }))
            .await
            .unwrap();
        assert_eq!(result["status"], json!("pending_confirmation"));
    }

    #[tokio::test]
    async fn cancel_offer_pending() {
        let inquiry_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = CancelOffer
            .execute(&ctx(services), &json!({ "inquiry_id": inquiry_id, "reason": "x" }))
            .await
            .unwrap();
        assert_eq!(result["status"], json!("pending_confirmation"));
    }

    #[tokio::test]
    async fn get_offer_history_returns_versions() {
        let inquiry_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = GetOfferHistory
            .execute(&ctx(services), &json!({ "inquiry_id": inquiry_id }))
            .await
            .unwrap();
        assert_eq!(result["count"], json!(1));
    }

    #[tokio::test]
    async fn mark_offer_accepted_ok() {
        let inquiry_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = MarkOfferAccepted
            .execute(&ctx(services), &json!({ "inquiry_id": inquiry_id, "source": "phone" }))
            .await
            .unwrap();
        assert_eq!(result["ok"], json!(true));
    }

    #[tokio::test]
    async fn mark_offer_rejected_ok() {
        let inquiry_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = MarkOfferRejected
            .execute(&ctx(services), &json!({ "inquiry_id": inquiry_id, "source": "phone" }))
            .await
            .unwrap();
        assert_eq!(result["ok"], json!(true));
    }
}

