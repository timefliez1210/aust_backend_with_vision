//! Review/feedback tools: list reviews, list feedback, respond, mark resolved.

use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::error::Result;
use crate::roles::Role;
use super::{parse_str, parse_uuid, Safety, Tool, ToolCtx};

// ── ListReviews ───────────────────────────────────────────────────────────────

pub struct ListReviews;

#[async_trait]
impl Tool for ListReviews {
    fn name(&self) -> &'static str { "list_reviews" }
    fn description(&self) -> &'static str { "Listet Kundenrezensionen / Bewertungsanfragen im Zeitraum." }
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
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let from = args["from"].as_str().and_then(|s| s.parse::<NaiveDate>().ok());
        let to = args["to"].as_str().and_then(|s| s.parse::<NaiveDate>().ok());
        let items = ctx.services.reviews.list_reviews(from, to).await?;
        let count = items.len();
        Ok(json!({ "reviews": items, "count": count }))
    }
}

// ── ListFeedback ──────────────────────────────────────────────────────────────

pub struct ListFeedback;

#[async_trait]
impl Tool for ListFeedback {
    fn name(&self) -> &'static str { "list_feedback" }
    fn description(&self) -> &'static str { "Listet interne Feedback-Reports (optional nur ungelöste). Nur für Inhaber." }
    fn params_schema(&self) -> Value {
        json!({ "type": "object", "properties": { "unresolved_only": { "type": "boolean" } } })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let unresolved = args["unresolved_only"].as_bool().unwrap_or(false);
        let items = ctx.services.reviews.list_feedback(unresolved).await?;
        let count = items.len();
        Ok(json!({ "feedback": items, "count": count }))
    }
}

// ── CreateFeedback ────────────────────────────────────────────────────────────

pub struct CreateFeedback;

#[async_trait]
impl Tool for CreateFeedback {
    fn name(&self) -> &'static str { "create_feedback" }
    fn description(&self) -> &'static str {
        "Meldet einen Bug oder Feature-Wunsch in die Entwickler-Pipeline (feedback_reports). \
         Nutze das, wenn du selbst auf einen Fehler/Backend-Defekt stößt (z. B. ein Tool liefert \
         einen Systemfehler) oder Alex sich ein neues Feature wünscht. report_type: 'bug' bei \
         Fehlern, 'feature' bei Wünschen. title kurz und prägnant; description mit Kontext \
         (was wurde versucht, welche IDs, welche Fehlermeldung). priority default 'medium'."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "report_type": { "type": "string", "enum": ["bug", "feature"] },
                "title":       { "type": "string", "minLength": 1 },
                "description": { "type": "string" },
                "priority":    { "type": "string", "enum": ["low", "medium", "high", "critical"] },
                "location":    { "type": "string", "description": "Bereich/Seite/Tool, z. B. 'assistant: set_offer_status' oder '/admin/calendar'." }
            },
            "required": ["report_type", "title"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let report_type = parse_str(args, "report_type", self.name())?;
        let title = parse_str(args, "title", self.name())?;
        let priority = args["priority"].as_str().unwrap_or("medium");
        let description = args["description"].as_str();
        let location = args["location"].as_str();
        match ctx
            .services
            .reviews
            .create_feedback(report_type, priority, title, description, location)
            .await
        {
            Ok(rec) => Ok(json!({
                "ok": true,
                "id": rec.id,
                "report_type": report_type,
                "priority": priority,
                "title": title
            })),
            Err(aust_core::services::ServiceError::Validation(msg)) => {
                Ok(json!({ "ok": false, "message": msg }))
            }
            Err(e) => Err(e.into()),
        }
    }
}

// ── RespondToReview ───────────────────────────────────────────────────────────

pub struct RespondToReview;

#[async_trait]
impl Tool for RespondToReview {
    fn name(&self) -> &'static str { "respond_to_review" }
    fn description(&self) -> &'static str {
        "Speichert einen Antwortentwurf zu einer Bewertung. Sendet NICHT. Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id":       { "type": "string", "format": "uuid" },
                "draft_de": { "type": "string", "minLength": 1 }
            },
            "required": ["id", "draft_de"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        let draft = parse_str(args, "draft_de", self.name())?;
        // Returns Validation error if the underlying schema can't hold a draft —
        // surfaced as a soft error so the chat can show "noch nicht unterstützt".
        match ctx.services.reviews.set_review_response_draft(id, draft).await {
            Ok(()) => Ok(json!({ "ok": true })),
            Err(aust_core::services::ServiceError::Validation(msg)) => {
                Ok(json!({ "ok": false, "message": msg }))
            }
            Err(e) => Err(e.into()),
        }
    }
}

// ── MarkFeedbackResolved ──────────────────────────────────────────────────────

pub struct MarkFeedbackResolved;

#[async_trait]
impl Tool for MarkFeedbackResolved {
    fn name(&self) -> &'static str { "mark_feedback_resolved" }
    fn description(&self) -> &'static str { "Markiert einen Feedback-Report als erledigt. Nur für Inhaber." }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id":    { "type": "string", "format": "uuid" },
                "notes": { "type": "string" }
            },
            "required": ["id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        let notes = args["notes"].as_str();
        ctx.services.reviews.mark_feedback_resolved(id, notes).await?;
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
    async fn list_reviews_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = ListReviews.execute(&ctx(services), &json!({})).await.unwrap();
        assert_eq!(r["count"], json!(0));
    }

    #[tokio::test]
    async fn list_feedback_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = ListFeedback.execute(&ctx(services), &json!({ "unresolved_only": true })).await.unwrap();
        assert_eq!(r["count"], json!(1));
    }

    #[tokio::test]
    async fn create_feedback_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = CreateFeedback
            .execute(&ctx(services), &json!({
                "report_type": "bug",
                "title": "offers.updated_at fehlt",
                "description": "accept schlug fehl",
                "priority": "high"
            }))
            .await
            .unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["report_type"], json!("bug"));
    }

    #[tokio::test]
    async fn respond_to_review_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = RespondToReview
            .execute(&ctx(services), &json!({ "id": uuid::Uuid::new_v4(), "draft_de": "Danke" }))
            .await
            .unwrap();
        assert_eq!(r["ok"], json!(true));
    }

    #[tokio::test]
    async fn mark_feedback_resolved_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = MarkFeedbackResolved
            .execute(&ctx(services), &json!({ "id": uuid::Uuid::new_v4() }))
            .await
            .unwrap();
        assert_eq!(r["ok"], json!(true));
    }
}
