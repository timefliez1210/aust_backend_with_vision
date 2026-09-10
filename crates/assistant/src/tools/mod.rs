//! Tool trait, safety levels, context, and registry.
//!
//! All assistant capabilities are expressed as typed Rust structs implementing
//! the [`Tool`] trait. The [`ToolRegistry`] collects all available tools and
//! filters them by the caller's role at session start.
//!
//! Tool argument validation uses [`jsonschema`] against each tool's declared
//! `params_schema`. The driver loop retries once on validation failure, sending
//! the error back to the LLM so it can self-correct.

pub mod addresses;
pub mod calendar;
pub mod customers;
pub mod emails;
pub mod employees;
pub mod estimates;
pub mod inquiries;
pub mod invoices;
pub mod meta;
pub mod offers;
pub mod reminders;
pub mod reviews;
pub mod settings;

#[cfg(test)]
pub mod testing;

use async_trait::async_trait;
use jsonschema::Validator;
use serde_json::Value;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::error::{AssistantError, Result};
use crate::llm::AssistantLlmProvider;
use crate::memory::durable;
use crate::roles::Role;

/// Safety classification for tools.
///
/// The driver loop uses this to decide whether to execute immediately (Read),
/// confirm before executing (Write), or always pause for explicit confirmation
/// (Confirm — reserved for high-impact actions like sending emails or invoices).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Safety {
    /// Read-only query. Executed immediately without confirmation.
    Read,
    /// State-modifying action. Presented as a pending action for confirmation
    /// when the session role requires it; executed immediately for Owner role.
    Write,
    /// Always pauses for confirmation regardless of role.
    Confirm,
}

/// Dependencies injected into every tool execution.
pub struct ToolCtx {
    /// Database connection pool.
    pub db: PgPool,
    /// LLM provider for any in-tool generation (e.g. offer drafting).
    pub llm: Arc<dyn AssistantLlmProvider>,
    /// Domain service bundle — bridge to `crates/api` business logic.
    /// Tools must prefer this over direct DB access where a service method exists.
    pub services: aust_core::services::ServiceBundle,
    /// Role of the user making the request.
    pub role: Role,
    /// Internal user ID.
    pub user_id: Uuid,
    /// Telegram chat ID of the session.
    pub chat_id: i64,
    /// Session ID for audit logging.
    pub session_id: Uuid,
    /// True only when this execution is the resume of a previously-confirmed
    /// `Safety::Confirm` action. `Safety::Confirm` tools must check this and
    /// return their `pending_confirmation` marker when false; perform the real
    /// side effect when true. Read/Write tools should ignore this flag.
    pub confirmed: bool,
}

/// A typed, schema-validated assistant tool.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Machine-readable identifier (snake_case).
    fn name(&self) -> &'static str;

    /// German description shown to the LLM in the tools preamble.
    fn description(&self) -> &'static str;

    /// JSON Schema object that describes this tool's parameters.
    fn params_schema(&self) -> Value;

    /// Safety level — determines confirmation behaviour.
    fn safety(&self) -> Safety;

    /// Minimum role required to invoke this tool.
    fn min_role(&self) -> Role;

    /// Execute the tool with pre-validated arguments.
    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value>;

    /// Human-readable German summary of the proposed action, shown on the
    /// Telegram confirmation keyboard for `Safety::Confirm` tools.
    ///
    /// Called at enqueue time (before any side effect) so Alex sees the
    /// recipient / amount / target spelled out rather than a bare tool name.
    /// The default returns a generic prompt; Confirm tools should override
    /// it with a concrete summary built from `args`.
    fn summarize(&self, _args: &Value) -> String {
        format!("Soll ich '{}' wirklich ausführen?", self.name())
    }
}

/// A registry of all available tools, filtered by role.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Build the registry with all built-in tools pre-registered.
    pub fn new() -> Self {
        let mut registry = Self { tools: vec![] };
        // Inquiries
        registry.register(Box::new(inquiries::CreateInquiry));
        registry.register(Box::new(inquiries::GetInquiry));
        registry.register(Box::new(inquiries::ListInquiries));
        registry.register(Box::new(inquiries::SearchInquiries));
        registry.register(Box::new(inquiries::AddInquiryNote));
        registry.register(Box::new(inquiries::UpdateInquiryStatus));
        registry.register(Box::new(inquiries::SetInquiryServices));
        registry.register(Box::new(inquiries::RequestInfoFromCustomer));
        registry.register(Box::new(inquiries::CancelInquiry));
        registry.register(Box::new(inquiries::CreateInquiryAppointment));

        // Offers
        registry.register(Box::new(offers::PreviewOffer));
        registry.register(Box::new(offers::CommitOfferDraft));
        registry.register(Box::new(offers::RecomputeOffer));
        registry.register(Box::new(offers::ApplyNaturalLanguageOverride));
        // SendOfferToCustomer is unregistered until OfferService::send (SMTP + PDF
        // attach) is plumbed — its execute() only returns NotWired, so exposing it
        // would walk Alex to a confirmation for an action the agent cannot perform.
        // Re-register here once the send path lands. Offer sending: use admin panel.
        registry.register(Box::new(offers::CancelOffer));
        registry.register(Box::new(offers::GetOfferHistory));
        registry.register(Box::new(offers::MarkOfferAccepted));
        registry.register(Box::new(offers::MarkOfferRejected));

        // Calendar
        registry.register(Box::new(calendar::GetCalendar));
        registry.register(Box::new(calendar::FindAvailableSlots));
        registry.register(Box::new(calendar::GetEmployeeAssignments));
        registry.register(Box::new(calendar::GetAssignedCrew));
        registry.register(Box::new(calendar::CreateCalendarItem));
        registry.register(Box::new(calendar::UpdateCalendarItem));
        registry.register(Box::new(calendar::DeleteCalendarItem));
        registry.register(Box::new(calendar::ScheduleInquiry));
        registry.register(Box::new(calendar::SetInquiryCrew));
        registry.register(Box::new(calendar::ReassignTermin));
        registry.register(Box::new(calendar::CancelTermin));
        registry.register(Box::new(calendar::AssignEmployee));
        registry.register(Box::new(calendar::SetEmployeeSchedule));

        // Customers
        registry.register(Box::new(customers::GetCustomer));
        registry.register(Box::new(customers::CreateCustomer));
        registry.register(Box::new(customers::SearchCustomers));
        registry.register(Box::new(customers::ListCustomerInquiries));
        registry.register(Box::new(customers::UpdateCustomer));
        registry.register(Box::new(customers::AddCustomerNote));
        registry.register(Box::new(customers::MergeCustomers));

        // Employees
        registry.register(Box::new(employees::ListEmployees));
        registry.register(Box::new(employees::GetEmployee));
        registry.register(Box::new(employees::GetEmployeeWorkload));
        registry.register(Box::new(employees::UpdateEmployee));
        registry.register(Box::new(employees::SetEmployeeActive));

        // Emails
        registry.register(Box::new(emails::ListInbox));
        registry.register(Box::new(emails::GetEmail));
        registry.register(Box::new(emails::ListThread));
        registry.register(Box::new(emails::DraftReply));
        // SendEmail: wired in 51afc90 via EmailService::send (SMTP). Sends a
        // standalone email to any recipient without an inquiry/thread.
        registry.register(Box::new(emails::SendEmail));
        registry.register(Box::new(emails::MarkEmailHandled));
        registry.register(Box::new(emails::CategorizeEmail));

        // Invoices
        registry.register(Box::new(invoices::ListInvoices));
        registry.register(Box::new(invoices::GetInvoice));
        registry.register(Box::new(invoices::ListInvoiceReminders));
        registry.register(Box::new(invoices::CreateInvoice));
        registry.register(Box::new(invoices::UpdateInvoiceStatus));
        registry.register(Box::new(invoices::RecordPayment));
        registry.register(Box::new(invoices::ListDueDunning));
        registry.register(Box::new(invoices::SendPaymentReminder));
        // SendInvoice stays unregistered until the invoice SMTP+PDF send path is
        // exposed via InvoiceService — it is NotWired-only today. Use the admin panel
        // to send an invoice. Dunning mail (SendPaymentReminder) does not need the PDF
        // pipeline — it is a plain email — so it is wired.
        registry.register(Box::new(invoices::VoidInvoice));

        // Estimates
        registry.register(Box::new(estimates::GetEstimation));
        registry.register(Box::new(estimates::OverrideEstimation));
        registry.register(Box::new(estimates::RequestRevisionFromVision));

        // Addresses
        registry.register(Box::new(addresses::GetDistance));
        registry.register(Box::new(addresses::UpdateInquiryAddresses));

        // Settings
        registry.register(Box::new(settings::GetSettings));
        registry.register(Box::new(settings::GetPricingConfig));
        // UpdatePricing unregistered until SettingsService exposes a pricing
        // mutation — execute() is NotWired-only today. Re-register when wired.

        // Reviews
        registry.register(Box::new(reviews::ListReviews));
        registry.register(Box::new(reviews::ListDueReviewRequests));
        registry.register(Box::new(reviews::SendReviewRequest));
        registry.register(Box::new(reviews::ListFeedback));
        registry.register(Box::new(reviews::CreateFeedback));
        registry.register(Box::new(reviews::RespondToReview));
        registry.register(Box::new(reviews::MarkFeedbackResolved));

        // Meta
        registry.register(Box::new(meta::Remember));
        registry.register(Box::new(meta::Recall));
        registry.register(Box::new(meta::DailyBriefing));
        registry.register(Box::new(meta::WeeklyPipeline));
        registry.register(Box::new(meta::DailyMetrics));
        registry.register(Box::new(meta::CreateTodo));
        registry.register(Box::new(meta::ListTodos));
        registry.register(Box::new(meta::ResolveTodo));

        // Reminders
        registry.register(Box::new(reminders::SetReminder));
        registry.register(Box::new(reminders::ListReminders));
        registry.register(Box::new(reminders::CancelReminder));

        registry
    }

    /// Add a tool to the registry.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    /// Return only the tools the given role is allowed to use.
    pub fn tools_for_role(&self, role: Role) -> Vec<&dyn Tool> {
        self.tools
            .iter()
            .filter(|t| role.satisfies(t.min_role()))
            .map(|t| t.as_ref())
            .collect()
    }

    /// Find a tool by name, checking that the role is permitted.
    pub fn get(&self, name: &str, role: Role) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name && role.satisfies(t.min_role()))
            .map(|t| t.as_ref())
    }

    /// Validate `args` against the tool's JSON schema.
    ///
    /// Returns `Ok(())` on success, or [`AssistantError::ArgValidation`] on failure.
    pub fn validate_args(tool: &dyn Tool, args: &Value) -> Result<()> {
        let schema_value = tool.params_schema();
        let compiled = Validator::new(&schema_value).map_err(|e| {
            AssistantError::ArgValidation {
                tool: tool.name().to_string(),
                message: format!("Schema compilation error: {e}"),
            }
        })?;

        if let Err(error) = compiled.validate(args) {
            return Err(AssistantError::ArgValidation {
                tool: tool.name().to_string(),
                message: error.to_string(),
            });
        }
        Ok(())
    }

    /// Build the tool schemas for the LLM tool-calling API (role-filtered).
    pub fn schemas_for_role(&self, role: Role) -> Vec<crate::llm::ToolSchema> {
        self.tools_for_role(role)
            .into_iter()
            .map(|t| crate::llm::ToolSchema {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.params_schema(),
            })
            .collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper used by `Safety::Confirm` tools.
///
/// Returns a JSON payload that describes the proposed action without performing
/// any side effect. The driver layer / Telegram approval UI is expected to
/// detect this shape and turn it into a `pending_actions` row + inline keyboard.
///
/// Keeping the side-effect-free return contract here means tools stay easy to
/// unit-test against `MockServiceBundle` (which uses a dangling DB pool).
pub(crate) fn pending_confirmation(
    tool_name: &str,
    args: &Value,
    summary_de: impl Into<String>,
) -> Value {
    serde_json::json!({
        "status": "pending_confirmation",
        "tool_name": tool_name,
        "args": args,
        "summary_de": summary_de.into(),
    })
}

/// Helper: parse a required UUID argument from a JSON object.
pub(crate) fn parse_uuid(args: &Value, key: &str, tool: &str) -> Result<Uuid> {
    args[key]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| AssistantError::ArgValidation {
            tool: tool.to_string(),
            message: format!("{key} must be a valid UUID"),
        })
}

/// Helper: parse a required date (YYYY-MM-DD) argument from a JSON object.
pub(crate) fn parse_date(args: &Value, key: &str, tool: &str) -> Result<chrono::NaiveDate> {
    parse_date_str(args[key].as_str()).ok_or_else(|| AssistantError::ArgValidation {
        tool: tool.to_string(),
        message: format!("{key} must be a date, YYYY-MM-DD or DD.MM.YYYY"),
    })
}

/// Accept both the ISO form and the German one.
///
/// The whole conversation is in German, so a model asked to move a Termin to the 16th
/// will happily emit `"16.07.2026"`. `str::parse` rejects that, and every optional-date
/// site turned the rejection into `None` — the patch left the date untouched and Josie
/// still reported "Termin verschoben".
pub(crate) fn parse_date_str(s: Option<&str>) -> Option<chrono::NaiveDate> {
    let s = s?.trim();
    if let Ok(d) = s.parse::<chrono::NaiveDate>() {
        return Some(d);
    }
    // `%Y` happily reads a bare "26" as the year 26, so a two-digit year would book the
    // job two millennia ago instead of failing where Alex can see it. Insist on four.
    let year_is_four_digits = s
        .rsplit('.')
        .next()
        .is_some_and(|y| y.len() == 4 && y.chars().all(|c| c.is_ascii_digit()));
    if !year_is_four_digits {
        return None;
    }
    chrono::NaiveDate::parse_from_str(s, "%d.%m.%Y").ok()
}

/// An optional date argument that fails loudly when it is present but unreadable.
///
/// `None` means the caller left the field out. A value that cannot be parsed is an
/// error, never a silent `None` — that distinction is what stopped a mistyped date from
/// being reported as a successful reschedule.
pub(crate) fn parse_date_opt(args: &Value, key: &str, tool: &str) -> Result<Option<chrono::NaiveDate>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => parse_date_str(v.as_str())
            .map(Some)
            .ok_or_else(|| AssistantError::ArgValidation {
                tool: tool.to_string(),
                message: format!("{key} must be a date, YYYY-MM-DD or DD.MM.YYYY"),
            }),
    }
}

/// A list of UUIDs that fails loudly on a bad entry.
///
/// The crew lists were parsed with `filter_map(…ok())`, so a malformed employee id was
/// dropped without a word: "plane das mit Kevin und Marco" scheduled the job with one
/// of them, or none, and reported success either way.
pub(crate) fn parse_uuid_list(
    args: &Value,
    key: &str,
    tool: &str,
) -> Result<Option<Vec<uuid::Uuid>>> {
    let arr = match args.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Array(a)) => a,
        Some(_) => {
            return Err(AssistantError::ArgValidation {
                tool: tool.to_string(),
                message: format!("{key} must be an array of employee ids"),
            })
        }
    };

    arr.iter()
        .map(|v| {
            v.as_str()
                .and_then(|s| s.trim().parse::<uuid::Uuid>().ok())
                .ok_or_else(|| AssistantError::ArgValidation {
                    tool: tool.to_string(),
                    message: format!("{key} contains an entry that is not an employee id: {v}"),
                })
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

/// Helper: parse an optional time-of-day string. Accepts `HH:MM` and `HH:MM:SS`.
pub(crate) fn parse_time_opt(s: Option<&str>) -> Option<chrono::NaiveTime> {
    let s = s?;
    chrono::NaiveTime::parse_from_str(s, "%H:%M:%S")
        .or_else(|_| chrono::NaiveTime::parse_from_str(s, "%H:%M"))
        .ok()
}

/// Helper: parse a required string argument.
pub(crate) fn parse_str<'a>(args: &'a Value, key: &str, tool: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .ok_or_else(|| AssistantError::ArgValidation {
            tool: tool.to_string(),
            message: format!("{key} must be a string"),
        })
}

/// Helper: fetch a single active durable memory for a given scope+key (used by meta tool).
pub(crate) async fn find_memory_to_supersede(
    pool: &PgPool,
    scope: &str,
    key: &str,
) -> Result<Option<uuid::Uuid>> {
    let existing = durable::recall(pool, Some(scope), None).await?;
    Ok(existing
        .into_iter()
        .find(|m| m.key == key)
        .map(|m| m.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registry_filters_by_role() {
        let registry = ToolRegistry::new();
        let owner_tools = registry.tools_for_role(Role::Owner);
        let operator_tools = registry.tools_for_role(Role::Operator);

        // Owner should see all tools (including Owner-only ones).
        assert!(owner_tools.len() >= operator_tools.len());

        // CommitOfferDraft is Owner-only — check it's absent for Operator.
        let operator_names: Vec<_> = operator_tools.iter().map(|t| t.name()).collect();
        assert!(!operator_names.contains(&"commit_offer_draft"));
    }

    #[test]
    fn schema_validation_rejects_missing_field() {
        let registry = ToolRegistry::new();
        let tool = registry.get("get_inquiry", Role::Owner).unwrap();
        // Args missing required field "inquiry_id".
        let err = ToolRegistry::validate_args(tool, &json!({})).unwrap_err();
        assert!(matches!(err, AssistantError::ArgValidation { .. }));
    }

    // Build a minimal ToolCtx for execution-path tests. The DB pool is set to a
    // dangling connection because no tool under test touches it directly any
    // more — all data flows through `services` (the mock bundle).
    fn dangling_pool() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid_user:invalid@127.0.0.1:1/invalid")
            .expect("lazy connect cannot fail")
    }

    fn mock_ctx(
        services: aust_core::services::ServiceBundle,
    ) -> ToolCtx {
        ToolCtx {
            db: dangling_pool(),
            llm: std::sync::Arc::new(crate::llm::MockAssistantLlm::always("ok")),
            services,
            role: Role::Owner,
            user_id: uuid::Uuid::nil(),
            chat_id: 0,
            session_id: uuid::Uuid::nil(),
            confirmed: false,
        }
    }

    #[tokio::test]
    async fn get_inquiry_tool_uses_bridge() {
        let inquiry_id = uuid::Uuid::new_v4();
        let services = super::testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let ctx = mock_ctx(services);
        let result = inquiries::GetInquiry
            .execute(&ctx, &json!({"inquiry_id": inquiry_id.to_string()}))
            .await
            .expect("tool should succeed");
        assert_eq!(result["id"], json!(inquiry_id.to_string()));
        assert_eq!(result["status"], json!("estimated"));
    }

    #[tokio::test]
    async fn commit_offer_draft_tool_returns_draft_via_bridge() {
        let inquiry_id = uuid::Uuid::new_v4();
        let offer_id = uuid::Uuid::new_v4();
        let services = super::testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), offer_id);
        let ctx = mock_ctx(services);
        let result = offers::CommitOfferDraft
            .execute(&ctx, &json!({"inquiry_id": inquiry_id.to_string()}))
            .await
            .expect("commit_offer_draft should succeed");
        // Must NOT return the legacy marker payload.
        assert_ne!(result["status"], json!("draft_requested"));
        assert_eq!(result["status"], json!("draft"));
        assert_eq!(result["offer_id"], json!(offer_id.to_string()));
        assert_eq!(result["persons"], json!(3));
        assert!(result["total_brutto_cents"].as_i64().unwrap() > 0);
    }

    #[tokio::test]
    async fn preview_offer_tool_returns_preview_via_bridge() {
        let inquiry_id = uuid::Uuid::new_v4();
        let offer_id = uuid::Uuid::new_v4();
        let services = super::testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), offer_id);
        let ctx = mock_ctx(services);
        let result = offers::PreviewOffer
            .execute(&ctx, &json!({"inquiry_id": inquiry_id.to_string()}))
            .await
            .expect("preview_offer should succeed");
        // Preview returns an OfferPreview, not an OfferDraft.
        assert_eq!(result["inquiry_id"], json!(inquiry_id.to_string()));
        assert!(result["computation"].is_object());
        assert!(result["computation"]["total_brutto_cents"].as_i64().unwrap() > 0);
        assert_eq!(result["computation"]["persons"], json!(3));
    }

    #[tokio::test]
    async fn get_calendar_tool_uses_bridge() {
        let services = super::testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let ctx = mock_ctx(services);
        let result = calendar::GetCalendar
            .execute(&ctx, &json!({"from": "2026-05-01", "to": "2026-05-31"}))
            .await
            .expect("ok");
        assert_eq!(result["count"], json!(1));
    }

    #[tokio::test]
    async fn get_customer_tool_uses_bridge() {
        let customer_id = uuid::Uuid::new_v4();
        let services = super::testing::mock_bundle(uuid::Uuid::new_v4(), customer_id, uuid::Uuid::new_v4());
        let ctx = mock_ctx(services);
        let result = customers::GetCustomer
            .execute(&ctx, &json!({"customer_id": customer_id.to_string()}))
            .await
            .expect("ok");
        assert_eq!(result["id"], json!(customer_id.to_string()));
        assert_eq!(result["email"], json!("max@example.com"));
    }

    #[tokio::test]
    async fn list_inbox_tool_uses_bridge() {
        let services = super::testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let ctx = mock_ctx(services);
        let result = emails::ListInbox
            .execute(&ctx, &json!({}))
            .await
            .expect("ok");
        assert_eq!(result["count"], json!(1));
    }

    #[tokio::test]
    async fn list_invoices_tool_uses_bridge() {
        let services = super::testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let ctx = mock_ctx(services);
        let result = invoices::ListInvoices
            .execute(&ctx, &json!({}))
            .await
            .expect("ok");
        assert_eq!(result["count"], json!(1));
    }

    #[test]
    fn schema_validation_accepts_valid_args() {
        let registry = ToolRegistry::new();
        let tool = registry.get("get_inquiry", Role::Owner).unwrap();
        let args = json!({"inquiry_id": "00000000-0000-0000-0000-000000000001"});
        assert!(ToolRegistry::validate_args(tool, &args).is_ok());
    }

    /// The conversation is German, so the model emits German dates. Rejecting them
    /// silently left the patch untouched while Josie reported "Termin verschoben".
    #[test]
    fn dates_parse_in_both_the_iso_and_the_german_form() {
        use chrono::NaiveDate;
        let expected = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        for form in ["2026-07-16", "16.07.2026", " 16.07.2026 "] {
            assert_eq!(parse_date_str(Some(form)), Some(expected), "form {form:?}");
        }
        // A two-digit year is refused rather than guessed: chrono reads "16.07.26" as
        // the year 26, and a job silently booked two millennia ago is worse than an
        // error Alex can see.
        assert_eq!(parse_date_str(Some("16.07.26")), None);
        assert_eq!(parse_date_str(Some("nächsten Freitag")), None);
        assert_eq!(parse_date_str(None), None);
    }

    /// Absent means "leave it alone"; present-but-unreadable is an error. Collapsing the
    /// two is what turned a mistyped date into a silent no-op reported as success.
    #[test]
    fn an_unreadable_optional_date_is_an_error_not_a_none() {
        let absent = serde_json::json!({});
        assert!(matches!(parse_date_opt(&absent, "new_date", "t"), Ok(None)));

        let null = serde_json::json!({ "new_date": null });
        assert!(matches!(parse_date_opt(&null, "new_date", "t"), Ok(None)));

        let good = serde_json::json!({ "new_date": "16.07.2026" });
        assert!(matches!(parse_date_opt(&good, "new_date", "t"), Ok(Some(_))));

        let bad = serde_json::json!({ "new_date": "irgendwann" });
        assert!(parse_date_opt(&bad, "new_date", "t").is_err());
    }

    /// A crew list used to drop malformed ids without a word, so a job was scheduled
    /// with fewer people than asked for and reported as done.
    #[test]
    fn a_bad_crew_entry_fails_instead_of_vanishing() {
        let a = uuid::Uuid::now_v7();
        let b = uuid::Uuid::now_v7();

        let good = serde_json::json!({ "crew": [a.to_string(), b.to_string()] });
        assert_eq!(parse_uuid_list(&good, "crew", "t").unwrap(), Some(vec![a, b]));

        let absent = serde_json::json!({});
        assert_eq!(parse_uuid_list(&absent, "crew", "t").unwrap(), None);

        let empty = serde_json::json!({ "crew": [] });
        assert_eq!(parse_uuid_list(&empty, "crew", "t").unwrap(), Some(vec![]));

        let bad = serde_json::json!({ "crew": [a.to_string(), "Kevin"] });
        assert!(parse_uuid_list(&bad, "crew", "t").is_err(), "a name is not an id");

        let not_a_list = serde_json::json!({ "crew": a.to_string() });
        assert!(parse_uuid_list(&not_a_list, "crew", "t").is_err());
    }
}
