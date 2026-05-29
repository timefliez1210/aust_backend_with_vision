//! Meta tools: durable memory (remember/recall), briefing, pipeline metrics, todos.

use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::error::{AssistantError, Result};
use crate::memory::durable::{self, MemoryKind, RememberParams};
use crate::roles::Role;
use super::{find_memory_to_supersede, parse_str, parse_uuid, Safety, Tool, ToolCtx};

/// Owner-only: store a structured preference, fact, rule, or pattern in durable memory.
///
/// If a memory with the same scope + key already exists it is superseded (not deleted).
pub struct Remember;

#[async_trait]
impl Tool for Remember {
    fn name(&self) -> &'static str {
        "remember"
    }

    fn description(&self) -> &'static str {
        "Speichert eine Erinnerung (Präferenz, Fakt, Regel oder Muster) dauerhaft. Vorhandene Einträge mit gleichem Schlüssel werden ersetzt (nicht gelöscht). Nur für Inhaber."
    }

    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["preference", "fact", "rule", "pattern"],
                    "description": "Art der Erinnerung"
                },
                "scope": {
                    "type": "string",
                    "description": "Gültigkeitsbereich: 'global', 'customer:<uuid>', 'employee:<uuid>', 'inquiry:<uuid>'"
                },
                "key": {
                    "type": "string",
                    "description": "Eindeutiger Schlüssel innerhalb des Bereichs"
                },
                "value": {
                    "description": "Wert der Erinnerung (beliebige JSON-Struktur)"
                }
            },
            "required": ["kind", "scope", "key", "value"]
        })
    }

    fn safety(&self) -> Safety {
        // B6: Changed from Write to Confirm so Alex always reviews what the agent
        // wants to remember before it is durably stored. This prevents prompt-injection
        // attacks where adversary-controlled email/inquiry content reaches the LLM and
        // plants a durable rule via a `remember` tool call.
        Safety::Confirm
    }

    fn min_role(&self) -> Role {
        Role::Owner
    }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let kind_str = args["kind"].as_str().ok_or_else(|| AssistantError::ArgValidation {
            tool: self.name().to_string(),
            message: "kind is required".to_string(),
        })?;

        let kind = match kind_str {
            "preference" => MemoryKind::Preference,
            "fact" => MemoryKind::Fact,
            "rule" => MemoryKind::Rule,
            "pattern" => MemoryKind::Pattern,
            other => {
                return Err(AssistantError::ArgValidation {
                    tool: self.name().to_string(),
                    message: format!("Unknown kind: '{other}'"),
                })
            }
        };

        let scope = args["scope"].as_str().ok_or_else(|| AssistantError::ArgValidation {
            tool: self.name().to_string(),
            message: "scope is required".to_string(),
        })?;

        let key = args["key"].as_str().ok_or_else(|| AssistantError::ArgValidation {
            tool: self.name().to_string(),
            message: "key is required".to_string(),
        })?;

        let value = args["value"].clone();

        let params = RememberParams {
            kind,
            scope,
            key,
            value,
            source: "user_explicit",
            confidence: 1.0,
        };

        // Check if a memory with the same scope+key already exists.
        let existing_id = find_memory_to_supersede(&ctx.db, scope, key).await?;

        let new_id = if let Some(old_id) = existing_id {
            durable::supersede(&ctx.db, old_id, params).await?
        } else {
            durable::remember(&ctx.db, params).await?
        };

        Ok(json!({
            "id": new_id,
            "message": "Erinnerung gespeichert.",
            "superseded_existing": existing_id.is_some(),
        }))
    }
}

// ── Recall ────────────────────────────────────────────────────────────────────

/// List active durable memories matching optional filters.
pub struct Recall;

#[async_trait]
impl Tool for Recall {
    fn name(&self) -> &'static str { "recall" }
    fn description(&self) -> &'static str {
        "Listet gespeicherte Erinnerungen, optional gefiltert nach Bereich, Art und Schlüssel."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "scope_filter": { "type": "string" },
                "kind_filter":  { "type": "string", "enum": ["preference", "fact", "rule", "pattern"] },
                "key_filter":   { "type": "string" }
            }
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let scope = args["scope_filter"].as_str();
        let kind = match args["kind_filter"].as_str() {
            Some("preference") => Some(MemoryKind::Preference),
            Some("fact") => Some(MemoryKind::Fact),
            Some("rule") => Some(MemoryKind::Rule),
            Some("pattern") => Some(MemoryKind::Pattern),
            _ => None,
        };
        let key = args["key_filter"].as_str();
        let memories = durable::recall(&ctx.db, scope, kind).await?;
        let filtered: Vec<_> = memories
            .into_iter()
            .filter(|m| key.is_none_or(|k| m.key == k))
            .collect();
        let count = filtered.len();
        Ok(json!({ "memories": filtered, "count": count }))
    }
}

// ── DailyBriefing ─────────────────────────────────────────────────────────────

pub struct DailyBriefing;

#[async_trait]
impl Tool for DailyBriefing {
    fn name(&self) -> &'static str { "daily_briefing" }
    fn description(&self) -> &'static str {
        "Erstellt die tägliche Übersicht (Termine, überfällige Rechnungen, offene Angebote, unbearbeitete E-Mails)."
    }
    fn params_schema(&self) -> Value { json!({ "type": "object", "properties": {} }) }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, _args: &Value) -> Result<Value> {
        let briefing = crate::hooks::briefing::assemble(&ctx.db).await?;
        Ok(serde_json::to_value(&briefing)?)
    }
}

// ── WeeklyPipeline ────────────────────────────────────────────────────────────

pub struct WeeklyPipeline;

#[async_trait]
impl Tool for WeeklyPipeline {
    fn name(&self) -> &'static str { "weekly_pipeline" }
    fn description(&self) -> &'static str {
        "Wochenpipeline-Metriken: Anfragen → Angebote → geplant → Rechnungen → bezahlt."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "format": "date" },
                "to":   { "type": "string", "format": "date" }
            }
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let today = chrono::Local::now().date_naive();
        let week_ago = today - chrono::Duration::days(7);
        let from = args["from"].as_str().and_then(|s| s.parse::<NaiveDate>().ok()).unwrap_or(week_ago);
        let to = args["to"].as_str().and_then(|s| s.parse::<NaiveDate>().ok()).unwrap_or(today);
        let metrics = ctx.services.metrics.pipeline(from, to).await?;
        Ok(serde_json::to_value(&metrics)?)
    }
}

// ── CreateTodo ────────────────────────────────────────────────────────────────

pub struct CreateTodo;

#[async_trait]
impl Tool for CreateTodo {
    fn name(&self) -> &'static str { "create_todo" }
    fn description(&self) -> &'static str { "Legt einen To-do-Eintrag in der aktuellen Session an." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "minLength": 1 },
                "due":  { "type": "string", "format": "date" }
            },
            "required": ["text"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let text = parse_str(args, "text", self.name())?;
        let due = args["due"].as_str().and_then(|s| s.parse::<NaiveDate>().ok());
        let todo = ctx.services.todos.create(ctx.session_id, text, due).await?;
        Ok(serde_json::to_value(&todo)?)
    }
}

// ── ListTodos ─────────────────────────────────────────────────────────────────

pub struct ListTodos;

#[async_trait]
impl Tool for ListTodos {
    fn name(&self) -> &'static str { "list_todos" }
    fn description(&self) -> &'static str { "Listet To-do-Einträge der aktuellen Session." }
    fn params_schema(&self) -> Value {
        json!({ "type": "object", "properties": { "open_only": { "type": "boolean" } } })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let open_only = args["open_only"].as_bool().unwrap_or(false);
        let todos = ctx.services.todos.list(ctx.session_id, open_only).await?;
        let count = todos.len();
        Ok(json!({ "todos": todos, "count": count }))
    }
}

// ── ResolveTodo ───────────────────────────────────────────────────────────────

pub struct ResolveTodo;

#[async_trait]
impl Tool for ResolveTodo {
    fn name(&self) -> &'static str { "resolve_todo" }
    fn description(&self) -> &'static str { "Markiert einen To-do-Eintrag als erledigt." }
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
        ctx.services.todos.resolve(id).await?;
        Ok(json!({ "ok": true }))
    }
}

#[cfg(test)]
mod meta_new_tests {
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

    /// B6: Remember must be Safety::Confirm so Alex reviews before any durable write.
    #[test]
    fn remember_is_safety_confirm() {
        assert!(
            matches!(Remember.safety(), Safety::Confirm),
            "Remember must be Safety::Confirm to prevent prompt-injection via external content"
        );
    }

    /// B6: Recall must remain Safety::Read — reads are always safe.
    #[test]
    fn recall_is_safety_read() {
        assert!(matches!(Recall.safety(), Safety::Read));
    }

    #[tokio::test]
    async fn weekly_pipeline_returns_metrics() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = WeeklyPipeline.execute(&ctx(services), &json!({})).await.unwrap();
        assert_eq!(r["inquiries_total"], json!(10));
    }

    #[tokio::test]
    async fn create_todo_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = CreateTodo
            .execute(&ctx(services), &json!({ "text": "Kunde anrufen" }))
            .await
            .unwrap();
        assert_eq!(r["text"], json!("Kunde anrufen"));
    }

    #[tokio::test]
    async fn list_todos_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = ListTodos.execute(&ctx(services), &json!({ "open_only": true })).await.unwrap();
        assert_eq!(r["count"], json!(1));
    }

    #[tokio::test]
    async fn resolve_todo_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = ResolveTodo.execute(&ctx(services), &json!({ "id": uuid::Uuid::new_v4() })).await.unwrap();
        assert_eq!(r["ok"], json!(true));
    }
}
