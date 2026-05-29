//! Estimation tools: get, override, request-revision.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::Result;
use crate::roles::Role;
use super::{parse_uuid, Safety, Tool, ToolCtx};

// ── GetEstimation ─────────────────────────────────────────────────────────────

pub struct GetEstimation;

#[async_trait]
impl Tool for GetEstimation {
    fn name(&self) -> &'static str { "get_estimation" }
    fn description(&self) -> &'static str {
        "Lädt die aktuelle Volumenschätzung einer Anfrage (Quelle, Volumen, Konfidenz, Item-Anzahl)."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "inquiry_id": { "type": "string", "format": "uuid" } },
            "required": ["inquiry_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "inquiry_id", self.name())?;
        let est = ctx.services.estimations.get(id).await?;
        Ok(json!(est))
    }
}

// ── OverrideEstimation ────────────────────────────────────────────────────────

pub struct OverrideEstimation;

#[async_trait]
impl Tool for OverrideEstimation {
    fn name(&self) -> &'static str { "override_estimation" }
    fn description(&self) -> &'static str {
        "Setzt das Volumen einer Anfrage manuell (überschreibt die Schätzung). Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" },
                "volume_m3":  { "type": "number", "minimum": 0 },
                "notes":      { "type": "string" }
            },
            "required": ["inquiry_id", "volume_m3"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "inquiry_id", self.name())?;
        let volume = args["volume_m3"].as_f64().unwrap_or(0.0);
        let notes = args["notes"].as_str();
        ctx.services.estimations.override_volume(id, volume, notes).await?;
        Ok(json!({ "ok": true, "inquiry_id": id, "volume_m3": volume }))
    }
}

// ── RequestRevisionFromVision ─────────────────────────────────────────────────

pub struct RequestRevisionFromVision;

#[async_trait]
impl Tool for RequestRevisionFromVision {
    fn name(&self) -> &'static str { "request_revision_from_vision" }
    fn description(&self) -> &'static str {
        "Stößt eine erneute Vision-Schätzung für eine Anfrage an. Nur für Inhaber. (Stub: Trigger-Pfad lebt im Vision-Worker.)"
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "inquiry_id": { "type": "string", "format": "uuid" } },
            "required": ["inquiry_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "inquiry_id", self.name())?;
        let status = ctx.services.estimations.request_revision(id).await?;
        Ok(json!({
            "ok": true,
            "inquiry_id": id,
            "queued": status.queued,
            "request_id": status.request_id,
            "reason": status.reason,
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
        }
    }

    #[tokio::test]
    async fn get_estimation_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = GetEstimation
            .execute(&ctx(services), &json!({ "inquiry_id": uuid::Uuid::new_v4() }))
            .await
            .unwrap();
        assert_eq!(r["method"], json!("vision"));
    }

    #[tokio::test]
    async fn override_estimation_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = OverrideEstimation
            .execute(&ctx(services), &json!({ "inquiry_id": uuid::Uuid::new_v4(), "volume_m3": 25.0 }))
            .await
            .unwrap();
        assert_eq!(r["ok"], json!(true));
    }

    #[tokio::test]
    async fn request_revision_from_vision_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = RequestRevisionFromVision
            .execute(&ctx(services), &json!({ "inquiry_id": uuid::Uuid::new_v4() }))
            .await
            .unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["queued"], json!(true));
        assert!(r["request_id"].is_string(), "request_id should be a UUID string");
    }
}
