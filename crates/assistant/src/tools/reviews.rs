//! Review/feedback tools: list reviews, list feedback, respond, mark resolved.

use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::error::Result;
use crate::roles::Role;
use super::{parse_str, parse_uuid, pending_confirmation, Safety, Tool, ToolCtx};

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

// ── ListDueReviewRequests ─────────────────────────────────────────────────────

pub struct ListDueReviewRequests;

#[async_trait]
impl Tool for ListDueReviewRequests {
    fn name(&self) -> &'static str { "list_due_review_requests" }
    fn description(&self) -> &'static str {
        "Listet die fälligen Bewertungsanfragen: abgeschlossene Umzüge, bei denen Alex die \
         Google-Rezension auf 'später' gelegt hat und das Datum jetzt erreicht ist. Nutze das für \
         'Welche Bewertungen stehen an?' oder 'Sollen wir noch jemanden um eine Rezension bitten?'. \
         Zum Senden: send_review_request mit der inquiry_id."
    }
    fn params_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, _args: &Value) -> Result<Value> {
        let items = ctx.services.reviews.list_due_review_requests().await?;
        let count = items.len();
        Ok(json!({ "review_requests": items, "count": count }))
    }
}

// ── SendReviewRequest (Confirm) ───────────────────────────────────────────────

pub struct SendReviewRequest;

#[async_trait]
impl Tool for SendReviewRequest {
    fn name(&self) -> &'static str { "send_review_request" }
    fn description(&self) -> &'static str {
        "Entscheidet über die Google-Bewertungsanfrage zu einer Anfrage. action='now' sendet die \
         Bewertungs-E-Mail sofort an den Kunden; action='later' verschiebt die Erinnerung um \
         remind_after_days Tage (Standard 3); action='skip' fragt bei diesem Auftrag nie wieder. \
         Erfordert Bestätigung, weil bei 'now' eine E-Mail an den Kunden rausgeht."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id":        { "type": "string", "format": "uuid" },
                "action":            { "type": "string", "enum": ["now", "later", "skip"] },
                "remind_after_days": {
                    "type": "integer", "minimum": 1, "maximum": 90,
                    "description": "Nur bei action='later'. Standard 3."
                }
            },
            "required": ["inquiry_id", "action"]
        })
    }
    fn safety(&self) -> Safety { Safety::Confirm }
    fn min_role(&self) -> Role { Role::Owner }

    fn summarize(&self, args: &Value) -> String {
        let id = args["inquiry_id"].as_str().unwrap_or("?");
        match args["action"].as_str().unwrap_or("now") {
            "later" => {
                let days = args["remind_after_days"].as_i64().unwrap_or(3);
                format!("Bewertungsanfrage für Anfrage {id} um {days} Tage verschieben?")
            }
            "skip" => format!("Bewertungsanfrage für Anfrage {id} überspringen?"),
            _ => format!("Bewertungs-E-Mail (Google-Rezension) für Anfrage {id} jetzt senden?"),
        }
    }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let inquiry_id = parse_uuid(args, "inquiry_id", self.name())?;
        let action = parse_str(args, "action", self.name())?;
        let days = args["remind_after_days"].as_u64().map(|d| d as u32);

        // 'later' and 'skip' touch nobody outside the company; only an actual send
        // needs Alex to sign off.
        if action == "now" && !ctx.confirmed {
            return Ok(pending_confirmation(self.name(), args, self.summarize(args)));
        }

        match ctx
            .services
            .reviews
            .decide_review_request(inquiry_id, action, days)
            .await
        {
            Ok(status) => Ok(json!({ "ok": true, "status": status })),
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
    async fn list_due_review_requests_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = ListDueReviewRequests.execute(&ctx(services), &json!({})).await.unwrap();
        assert_eq!(r["count"], json!(1));
        assert_eq!(r["review_requests"][0]["customer_name"], json!("Frau Schilling"));
    }

    /// Sending a review mail reaches the customer, so it must not fire before Alex
    /// has confirmed — the unconfirmed call returns a confirmation request instead.
    #[tokio::test]
    async fn send_review_request_now_requires_confirmation() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let r = SendReviewRequest
            .execute(
                &ctx(services),
                &json!({ "inquiry_id": uuid::Uuid::new_v4(), "action": "now" }),
            )
            .await
            .unwrap();
        assert_ne!(r["ok"], json!(true), "must not send before confirmation");
        assert!(
            r.to_string().contains("send_review_request"),
            "expected a pending-confirmation payload, got {r}"
        );
    }

    /// Deferring or skipping touches nobody outside the company, so it goes through
    /// without a confirmation round-trip.
    #[tokio::test]
    async fn send_review_request_later_and_skip_need_no_confirmation() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let later = SendReviewRequest
            .execute(
                &ctx(services),
                &json!({ "inquiry_id": uuid::Uuid::new_v4(), "action": "later", "remind_after_days": 5 }),
            )
            .await
            .unwrap();
        assert_eq!(later["ok"], json!(true));
        assert_eq!(later["status"], json!("pending"));

        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let skip = SendReviewRequest
            .execute(
                &ctx(services),
                &json!({ "inquiry_id": uuid::Uuid::new_v4(), "action": "skip" }),
            )
            .await
            .unwrap();
        assert_eq!(skip["status"], json!("skipped"));
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
