//! Address tools: get distance, update inquiry addresses.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::Result;
use crate::roles::Role;
use super::{parse_uuid, Safety, Tool, ToolCtx};

// ── GetDistance ───────────────────────────────────────────────────────────────

pub struct GetDistance;

#[async_trait]
impl Tool for GetDistance {
    fn name(&self) -> &'static str { "get_distance" }
    fn description(&self) -> &'static str {
        "Gibt die berechnete Entfernung (km) zwischen zwei Adressen zurück (falls bekannt)."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from_address_id": { "type": "string", "format": "uuid" },
                "to_address_id":   { "type": "string", "format": "uuid" }
            },
            "required": ["from_address_id", "to_address_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let from = parse_uuid(args, "from_address_id", self.name())?;
        let to = parse_uuid(args, "to_address_id", self.name())?;
        let d = ctx.services.addresses.get_distance(from, to).await?;
        Ok(json!(d))
    }
}

// ── UpdateInquiryAddresses ────────────────────────────────────────────────────

pub struct UpdateInquiryAddresses;

#[async_trait]
impl Tool for UpdateInquiryAddresses {
    fn name(&self) -> &'static str { "update_inquiry_addresses" }
    fn description(&self) -> &'static str {
        "Aktualisiert Start- und/oder Zieladresse einer Anfrage und setzt die Entfernung zurück. Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" },
                "from": { "type": "object" },
                "to":   { "type": "object" }
            },
            "required": ["inquiry_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let inquiry_id = parse_uuid(args, "inquiry_id", self.name())?;
        let from: Option<aust_core::services::AddressPatch> = if args["from"].is_object() {
            Some(serde_json::from_value(args["from"].clone())?)
        } else {
            None
        };
        let to: Option<aust_core::services::AddressPatch> = if args["to"].is_object() {
            Some(serde_json::from_value(args["to"].clone())?)
        } else {
            None
        };
        ctx.services.addresses.update_inquiry_addresses(inquiry_id, from, to).await?;
        Ok(json!({ "ok": true, "inquiry_id": inquiry_id }))
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
    async fn get_distance_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = GetDistance
            .execute(
                &ctx(services),
                &json!({ "from_address_id": uuid::Uuid::new_v4(), "to_address_id": uuid::Uuid::new_v4() }),
            )
            .await
            .unwrap();
        assert_eq!(r["distance_km"], json!(15.0));
    }

    #[tokio::test]
    async fn update_inquiry_addresses_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = UpdateInquiryAddresses
            .execute(
                &ctx(services),
                &json!({ "inquiry_id": uuid::Uuid::new_v4(), "from": { "city": "München" } }),
            )
            .await
            .unwrap();
        assert_eq!(r["ok"], json!(true));
    }
}
