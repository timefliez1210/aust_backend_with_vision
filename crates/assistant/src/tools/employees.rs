//! Employee tools: list, get, workload, update, set-active.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AssistantError, Result};
use crate::roles::Role;
use super::{parse_date, parse_uuid, pending_confirmation, Safety, Tool, ToolCtx};

// ── ListEmployees ─────────────────────────────────────────────────────────────

pub struct ListEmployees;

#[async_trait]
impl Tool for ListEmployees {
    fn name(&self) -> &'static str { "list_employees" }
    fn description(&self) -> &'static str { "Listet alle Mitarbeiter (optional nur aktive). Nur für Inhaber." }
    fn params_schema(&self) -> Value {
        json!({ "type": "object", "properties": { "active_only": { "type": "boolean" } } })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let active_only = args["active_only"].as_bool().unwrap_or(false);
        let items = ctx.services.employees.list(active_only).await?;
        let count = items.len();
        Ok(json!({ "employees": items, "count": count }))
    }
}

// ── GetEmployee ───────────────────────────────────────────────────────────────

pub struct GetEmployee;

#[async_trait]
impl Tool for GetEmployee {
    fn name(&self) -> &'static str { "get_employee" }
    fn description(&self) -> &'static str {
        "Lädt Mitarbeiterdaten. Operatoren dürfen nur ihr eigenes Profil abrufen."
    }
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
        if ctx.role == Role::Operator && ctx.user_id != id {
            return Err(AssistantError::Forbidden(
                "Operatoren dürfen nur das eigene Profil abrufen.".to_string(),
            ));
        }
        let emp = ctx.services.employees.get(id).await?;
        Ok(json!(emp))
    }
}

// ── GetEmployeeWorkload ───────────────────────────────────────────────────────

pub struct GetEmployeeWorkload;

#[async_trait]
impl Tool for GetEmployeeWorkload {
    fn name(&self) -> &'static str { "get_employee_workload" }
    fn description(&self) -> &'static str { "Listet Einsätze eines Mitarbeiters in einem Zeitraum. Nur für Inhaber." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id":   { "type": "string", "format": "uuid" },
                "from": { "type": "string", "format": "date" },
                "to":   { "type": "string", "format": "date" }
            },
            "required": ["id", "from", "to"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        let from = parse_date(args, "from", self.name())?;
        let to = parse_date(args, "to", self.name())?;
        let entries = ctx.services.employees.get_workload(id, from, to).await?;
        let count = entries.len();
        Ok(json!({ "entries": entries, "count": count }))
    }
}

// ── UpdateEmployee ────────────────────────────────────────────────────────────

pub struct UpdateEmployee;

#[async_trait]
impl Tool for UpdateEmployee {
    fn name(&self) -> &'static str { "update_employee" }
    fn description(&self) -> &'static str { "Aktualisiert Mitarbeiterfelder (Telefon, Rolle). Nur für Inhaber." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id":    { "type": "string", "format": "uuid" },
                "patch": {
                    "type": "object",
                    "properties": {
                        "phone": { "type": "string" },
                        "role":  { "type": "string" }
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
        let patch: aust_core::services::EmployeePatch =
            serde_json::from_value(args["patch"].clone())?;
        let emp = ctx.services.employees.update(id, patch).await?;
        Ok(json!(emp))
    }
}

// ── SetEmployeeActive (Confirm) ───────────────────────────────────────────────

pub struct SetEmployeeActive;

#[async_trait]
impl Tool for SetEmployeeActive {
    fn name(&self) -> &'static str { "set_employee_active" }
    fn description(&self) -> &'static str {
        "Setzt einen Mitarbeiter aktiv oder inaktiv. Erfordert Bestätigung."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id":     { "type": "string", "format": "uuid" },
                "active": { "type": "boolean" }
            },
            "required": ["id", "active"]
        })
    }
    fn safety(&self) -> Safety { Safety::Confirm }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, _ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        let active = args["active"].as_bool().unwrap_or(false);
        let action = if active { "aktivieren" } else { "deaktivieren" };
        Ok(pending_confirmation(
            self.name(),
            args,
            format!("Mitarbeiter {id} {action}?"),
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
    async fn list_employees_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = ListEmployees.execute(&ctx(services), &json!({})).await.unwrap();
        assert_eq!(r["count"], json!(1));
    }

    #[tokio::test]
    async fn get_employee_ok() {
        let id = uuid::Uuid::new_v4();
        // In the mock bundle, employee_id == customer_id by construction.
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), id, uuid::Uuid::new_v4());
        let r = GetEmployee.execute(&ctx(services), &json!({ "id": id })).await.unwrap();
        assert_eq!(r["first_name"], json!("Anna"));
    }

    #[tokio::test]
    async fn get_workload_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = GetEmployeeWorkload
            .execute(
                &ctx(services),
                &json!({ "id": uuid::Uuid::new_v4(), "from": "2026-06-01", "to": "2026-06-30" }),
            )
            .await
            .unwrap();
        assert_eq!(r["count"], json!(1));
    }

    #[tokio::test]
    async fn update_employee_ok() {
        let id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), id, uuid::Uuid::new_v4());
        let r = UpdateEmployee
            .execute(&ctx(services), &json!({ "id": id, "patch": { "phone": "+49170111" } }))
            .await
            .unwrap();
        assert_eq!(r["id"], json!(id.to_string()));
    }

    #[tokio::test]
    async fn set_employee_active_pending() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = SetEmployeeActive
            .execute(&ctx(services), &json!({ "id": uuid::Uuid::new_v4(), "active": false }))
            .await
            .unwrap();
        assert_eq!(r["status"], json!("pending_confirmation"));
    }
}
