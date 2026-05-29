//! Settings tools: get settings, get pricing config, update pricing.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::Result;
use crate::roles::Role;
use super::{pending_confirmation, Safety, Tool, ToolCtx};

// ── GetSettings ───────────────────────────────────────────────────────────────

pub struct GetSettings;

#[async_trait]
impl Tool for GetSettings {
    fn name(&self) -> &'static str { "get_settings" }
    fn description(&self) -> &'static str { "Gibt die effektive Preis- und Standardkonfiguration zurück. Nur für Inhaber." }
    fn params_schema(&self) -> Value { json!({ "type": "object", "properties": {} }) }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, _args: &Value) -> Result<Value> {
        let p = ctx.services.settings.get_pricing().await?;
        Ok(json!({ "pricing": p }))
    }
}

// ── GetPricingConfig ──────────────────────────────────────────────────────────

pub struct GetPricingConfig;

#[async_trait]
impl Tool for GetPricingConfig {
    fn name(&self) -> &'static str { "get_pricing_config" }
    fn description(&self) -> &'static str { "Gibt die Preiskonfiguration (Stundensatz, Aufschläge, MwSt.) zurück." }
    fn params_schema(&self) -> Value { json!({ "type": "object", "properties": {} }) }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, _args: &Value) -> Result<Value> {
        let p = ctx.services.settings.get_pricing().await?;
        Ok(json!(p))
    }
}

// ── UpdatePricing (Confirm) ───────────────────────────────────────────────────

pub struct UpdatePricing;

#[async_trait]
impl Tool for UpdatePricing {
    fn name(&self) -> &'static str { "update_pricing" }
    fn description(&self) -> &'static str {
        "Aktualisiert die Preiskonfiguration (wirkt auf alle Angebote). Erfordert Bestätigung."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "patch": {
                    "type": "object",
                    "properties": {
                        "base_rate_eur":          { "type": "number" },
                        "saturday_surcharge_pct": { "type": "number" },
                        "vat_rate_pct":           { "type": "number" },
                        "min_hours":              { "type": "number" }
                    }
                }
            },
            "required": ["patch"]
        })
    }
    fn safety(&self) -> Safety { Safety::Confirm }
    fn min_role(&self) -> Role { Role::Owner }

    fn summarize(&self, args: &Value) -> String {
        let patch = &args["patch"];
        let mut parts: Vec<String> = Vec::new();
        if let Some(v) = patch["base_rate_eur"].as_f64() {
            parts.push(format!("Stundensatz {v:.2} €"));
        }
        if let Some(v) = patch["saturday_surcharge_pct"].as_f64() {
            parts.push(format!("Samstagszuschlag {v:.0}%"));
        }
        if let Some(v) = patch["vat_rate_pct"].as_f64() {
            parts.push(format!("MwSt {v:.0}%"));
        }
        if let Some(v) = patch["min_hours"].as_f64() {
            parts.push(format!("Mindeststunden {v:.1}"));
        }
        let detail = if parts.is_empty() { "(keine Änderungen)".to_string() } else { parts.join(", ") };
        format!("Preiskonfiguration aktualisieren — {detail}? Wirkt auf alle künftigen Angebote.")
    }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        if !ctx.confirmed {
            return Ok(pending_confirmation(self.name(), args, self.summarize(args)));
        }
        Err(crate::error::AssistantError::NotWired(
            "Preiskonfiguration-Update (SettingsService::update_pricing)".to_string(),
        ))
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
    async fn get_settings_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = GetSettings.execute(&ctx(services), &json!({})).await.unwrap();
        assert_eq!(r["pricing"]["base_rate_eur"], json!(45.0));
    }

    #[tokio::test]
    async fn get_pricing_config_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = GetPricingConfig.execute(&ctx(services), &json!({})).await.unwrap();
        assert_eq!(r["vat_rate_pct"], json!(19.0));
    }

    #[tokio::test]
    async fn update_pricing_pending() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = UpdatePricing
            .execute(&ctx(services), &json!({ "patch": { "base_rate_eur": 50.0 } }))
            .await
            .unwrap();
        assert_eq!(r["status"], json!("pending_confirmation"));
    }
}
