//! Service trait definitions — one trait per business domain.
//!
//! Each trait carries only the methods the assistant tools need today. Add new
//! methods here when new tool functionality requires them, and add matching impls
//! in `crates/api/src/services/bridge/`.

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::error::ServiceError;
use crate::models::{CustomerSnapshot, InquiryListItem, InquiryResponse, Services};

// ── Shared lightweight DTOs ───────────────────────────────────────────────────

/// A drafted offer returned to the assistant — contains pricing summary without
/// the full PDF pipeline result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OfferDraft {
    pub offer_id: Uuid,
    pub inquiry_id: Uuid,
    pub status: String,
    pub persons: i32,
    pub hours: f64,
    pub rate_cents: i64,
    pub total_netto_cents: i64,
    pub total_brutto_cents: i64,
    pub offer_number: Option<String>,
}

/// Pure pricing computation result — everything computed by the pricing engine,
/// before any PDF render, S3 upload, or DB write.
///
/// Returned by `preview_offer`; also used internally by `commit_offer_draft`
/// before proceeding to the persistence/render phase.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OfferComputation {
    /// Number of moving helpers (Umzugshelfer).
    pub persons: u32,
    /// Estimated hours for the move.
    pub hours: f64,
    /// Effective hourly rate in euro cents per person.
    pub rate_cents: i64,
    /// Total netto price in euro cents (sum of all line items).
    pub total_netto_cents: i64,
    /// Total brutto price in euro cents (`netto * 1.19`, rounded).
    pub total_brutto_cents: i64,
    /// Serialised line items (descriptions, quantities, unit prices).
    pub line_items: Vec<ComputedLineItem>,
    /// Whether a Saturday surcharge was applied.
    pub saturday_surcharge_applied: bool,
    /// Fahrkostenpauschale total in euro cents (may be 0 if no addresses).
    pub fahrt_cents: i64,
}

/// A single computed line item within an `OfferComputation`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ComputedLineItem {
    pub description: String,
    pub quantity: f64,
    pub unit_price_eur: f64,
    /// Set for flat-total items (Fahrkostenpauschale, Nürnbergerversicherung).
    pub flat_total_eur: Option<f64>,
    pub is_labor: bool,
    /// Effective line total in euro cents.
    pub line_total_cents: i64,
}

/// Preview returned by `preview_offer` — pure computation, no side effects.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OfferPreview {
    pub inquiry_id: Uuid,
    pub computation: OfferComputation,
    /// Short human-readable customer name for display.
    pub customer_name: String,
    /// Moving date as formatted string (DD.MM.YYYY or "nach Vereinbarung").
    pub moving_date: String,
    /// Volume in m³ as estimated for this inquiry.
    pub volume_m3: f64,
    /// Distance in km (stored or 0.0 if unknown).
    pub distance_km: f64,
}

/// Optional overrides for the pricing computation.
///
/// Used by `preview_offer` and `commit_offer_draft`. All fields are `Option`
/// so the caller only overrides what they care about; the engine fills in the rest.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OfferOverrides {
    /// Override the number of helpers (Umzugshelfer).
    pub crew_size: Option<u32>,
    /// Override the estimated hours.
    pub hours: Option<f64>,
    /// Override the hourly rate in EUR (not cents).
    pub rate_eur: Option<f64>,
    /// Override the total netto price in euro cents (rate is back-calculated).
    pub price_netto_cents: Option<i64>,
    /// Override the volume used for pricing (m³). When set, replaces the inquiry's
    /// stored `estimated_volume_m3` for this computation only — does not mutate
    /// the inquiry row.
    pub volume_m3: Option<f64>,
    /// Surcharge percentage to apply on top of the computed netto (0–100).
    pub surcharge_pct: Option<f64>,
    /// Flat discount in euro cents subtracted from the netto total.
    pub discount_eur_cents: Option<i64>,
    /// Whether to include packing service (overrides inquiry flag).
    pub packing: Option<bool>,
    /// Whether to include assembly service (overrides inquiry flag).
    pub assembly: Option<bool>,
    /// Internal notes for the offer record.
    pub notes_internal: Option<String>,
    /// Notes shown to the customer.
    pub notes_customer: Option<String>,
    /// Override offer validity date.
    pub valid_until: Option<NaiveDate>,
    /// Payment terms text override.
    pub payment_terms: Option<String>,
}

/// A calendar item as returned by the service layer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CalendarItem {
    pub id: Uuid,
    pub title: String,
    pub category: String,
    pub scheduled_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    /// Start time of day (e.g. `10:00`), if set. Lives in `calendar_items` /
    /// `inquiries`; previously dropped, which left the assistant blind to the
    /// hour of every appointment.
    pub start_time: Option<NaiveTime>,
    /// End time of day, if set.
    pub end_time: Option<NaiveTime>,
    /// Free-text location/address for the appointment, if set.
    pub location: Option<String>,
    /// Origin of this row, so the caller knows which write tools apply:
    /// `"termin"` → a `calendar_items` row (use reassign_termin / delete /
    /// assign_employee with this id); `"auftrag"` → a moving job derived from
    /// an `inquiries` row (the id is an inquiry_id — use set_inquiry_crew /
    /// schedule_inquiry, NOT the calendar_item tools).
    pub kind: String,
}

/// A lightweight email message summary.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmailSummary {
    pub id: Uuid,
    pub subject: String,
    pub from_address: Option<String>,
    pub status: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// A lightweight invoice summary.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InvoiceSummary {
    pub id: Uuid,
    pub invoice_number: String,
    pub status: String,
    pub due_date: Option<NaiveDate>,
    pub sent_at: Option<chrono::DateTime<chrono::Utc>>,
}

// ── New Wave-2 DTOs ───────────────────────────────────────────────────────────

/// A single offer version (history entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferVersion {
    pub offer_id: Uuid,
    pub offer_number: Option<String>,
    pub status: String,
    pub persons: i32,
    pub hours: f64,
    pub total_brutto_cents: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Patch struct for updating a calendar item.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CalendarItemPatch {
    pub title: Option<String>,
    pub category: Option<String>,
    pub scheduled_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub notes: Option<String>,
}

/// A lightweight employee record for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmployeeRecord {
    pub id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub role: Option<String>,
    pub active: bool,
}

/// Patch for updating an employee.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmployeePatch {
    pub phone: Option<String>,
    pub role: Option<String>,
}

/// A lightweight workload entry for an employee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmployeeWorkloadEntry {
    pub date: NaiveDate,
    pub inquiry_id: Option<Uuid>,
    pub calendar_item_id: Option<Uuid>,
    pub title: String,
    pub category: String,
}

/// A single crew member assigned to a termin (calendar item) or an inquiry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrewMember {
    pub employee_id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub job_date: NaiveDate,
    /// Origin of the assignment: `"termin"` (calendar_item_employees) or
    /// `"auftrag"` (inquiry_employees).
    pub source: String,
}

/// Patch for updating customer fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CustomerPatch {
    pub phone: Option<String>,
    pub email: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    /// Anrede: `"Herr"` / `"Frau"` / `"Divers"`.
    pub salutation: Option<String>,
}

/// Fields for creating a new customer (e.g. phone / walk-in intake).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewCustomer {
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    /// `"private"` (default) or `"business"`.
    pub customer_type: Option<String>,
    pub company_name: Option<String>,
    /// `"Herr"`, `"Frau"` or `"Divers"`.
    pub salutation: Option<String>,
}

/// An estimation result summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EstimationSummary {
    pub id: Uuid,
    pub method: String,
    pub status: String,
    pub total_volume_m3: Option<f64>,
    pub confidence_score: Option<f64>,
    pub item_count: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Distance result between two addresses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistanceResult {
    pub from_address_id: Uuid,
    pub to_address_id: Uuid,
    pub distance_km: f64,
    pub duration_minutes: Option<f64>,
}

/// Patch for an address on an inquiry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AddressPatch {
    pub street: Option<String>,
    pub house_number: Option<String>,
    pub city: Option<String>,
    pub postal_code: Option<String>,
    pub country: Option<String>,
    pub floor: Option<String>,
    pub elevator: Option<bool>,
}

/// Company pricing configuration (editable fields only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingConfig {
    pub base_rate_eur: f64,
    pub saturday_surcharge_pct: f64,
    pub vat_rate_pct: f64,
    pub min_hours: f64,
}

/// A review/rating entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRecord {
    pub id: Uuid,
    pub inquiry_id: Option<Uuid>,
    pub rating: Option<i32>,
    pub text: Option<String>,
    pub response_draft: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// A feedback report entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackRecord {
    pub id: Uuid,
    pub inquiry_id: Option<Uuid>,
    pub category: Option<String>,
    pub description: String,
    pub resolved: bool,
    pub notes: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Funnel metrics for the weekly pipeline overview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineMetrics {
    pub period_from: NaiveDate,
    pub period_to: NaiveDate,
    pub inquiries_total: i64,
    pub offers_sent: i64,
    pub scheduled: i64,
    pub invoiced: i64,
    pub paid: i64,
    pub revenue_netto_cents: i64,
}

/// A todo item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoRecord {
    pub id: Uuid,
    pub session_id: Uuid,
    pub text: String,
    pub due: Option<NaiveDate>,
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub resolved_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// An active reminder the assistant fires back to a Telegram chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReminderRecord {
    pub id: Uuid,
    pub chat_id: i64,
    pub text: String,
    pub due_at: chrono::DateTime<chrono::Utc>,
    /// `"none"` (one-shot) or `"recurring"` (every ~3h within business hours).
    pub recurrence: String,
    /// `"manual"` or `"email"` (auto-created by the email nag reconciler).
    pub source: String,
    pub active: bool,
    pub fired_count: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// An available calendar slot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableSlot {
    pub date: NaiveDate,
    pub available_crew: i32,
}

/// Detailed invoice record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceDetail {
    pub id: Uuid,
    pub invoice_number: String,
    pub inquiry_id: Option<Uuid>,
    pub status: String,
    pub due_date: Option<NaiveDate>,
    pub sent_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// An invoice reminder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceReminder {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub level: i32,
    pub sent_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// A full email record including body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailDetail {
    pub id: Uuid,
    pub subject: String,
    pub from_address: Option<String>,
    pub to_address: Option<String>,
    pub body_text: Option<String>,
    pub status: Option<String>,
    pub direction: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

// ── Inquiry ───────────────────────────────────────────────────────────────────

/// Read + mutate inquiries.
#[async_trait]
pub trait InquiryService: Send + Sync {
    /// Fetch the full canonical inquiry response for a given ID.
    async fn get_inquiry(&self, id: Uuid) -> Result<InquiryResponse, ServiceError>;

    /// List inquiries, optionally filtered.
    /// `status_filter` is a SQL-style text value (e.g. `"pending"`), `None` means all.
    async fn list_inquiries(
        &self,
        status_filter: Option<&str>,
        limit: u32,
    ) -> Result<Vec<InquiryListItem>, ServiceError>;

    /// Full-text search across customer name, addresses, and notes.
    async fn search_inquiries(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<InquiryListItem>, ServiceError>;

    /// Append a plain-text note to an inquiry.
    async fn add_note(
        &self,
        id: Uuid,
        text: &str,
        author_role: &str,
    ) -> Result<(), ServiceError>;

    /// Transition an inquiry to a new status, validating the transition.
    async fn update_status(
        &self,
        id: Uuid,
        new_status: &str,
        reason: Option<&str>,
    ) -> Result<InquiryResponse, ServiceError>;

    /// Overwrite the service flags (packing, assembly, …) for an inquiry.
    async fn set_services(
        &self,
        id: Uuid,
        services: Services,
    ) -> Result<(), ServiceError>;

    /// Cancel an inquiry.
    async fn cancel_inquiry(&self, id: Uuid, reason: &str) -> Result<(), ServiceError>;
}

// ── Offer ─────────────────────────────────────────────────────────────────────

/// Generate and query offers.
#[async_trait]
pub trait OfferService: Send + Sync {
    /// Draft an offer for an inquiry using the pricing engine.
    ///
    /// Stores the offer record in `draft` status. Does NOT send it to the customer.
    ///
    /// # Deprecation
    /// Prefer [`commit_offer_draft`] — this is a thin alias kept for backward
    /// compatibility with callers that predate the preview/commit split.
    #[deprecated(note = "use commit_offer_draft")]
    async fn draft_offer(&self, inquiry_id: Uuid) -> Result<OfferDraft, ServiceError>;

    /// Compute pricing for an inquiry **without** writing anything to the database.
    ///
    /// Pure function: no PDF render, no S3 upload, no DB insert. Safe to call
    /// as many times as needed to explore different overrides.
    async fn preview_offer(
        &self,
        inquiry_id: Uuid,
        overrides: Option<OfferOverrides>,
    ) -> Result<OfferPreview, ServiceError>;

    /// Run the full offer pipeline (PDF render + S3 upload + DB insert) and
    /// return a summary of the committed draft.
    ///
    /// This is what `draft_offer` delegates to.
    async fn commit_offer_draft(
        &self,
        inquiry_id: Uuid,
        overrides: Option<OfferOverrides>,
    ) -> Result<OfferDraft, ServiceError>;

    /// Fetch the current offer for an inquiry (if any).
    async fn get_offer(&self, inquiry_id: Uuid) -> Result<Option<OfferDraft>, ServiceError>;

    /// List all offer versions for an inquiry (newest first).
    async fn list_offer_versions(
        &self,
        inquiry_id: Uuid,
    ) -> Result<Vec<OfferVersion>, ServiceError>;

    /// Mark the active offer for an inquiry as accepted (manual path, e.g. phone).
    async fn mark_offer_accepted(
        &self,
        inquiry_id: Uuid,
        source: &str,
    ) -> Result<(), ServiceError>;

    /// Mark the active offer for an inquiry as rejected.
    async fn mark_offer_rejected(
        &self,
        inquiry_id: Uuid,
        source: &str,
        reason: Option<&str>,
    ) -> Result<(), ServiceError>;

    /// Parse a German natural-language instruction and re-commit the offer draft
    /// with the resulting typed overrides.
    ///
    /// Uses the same rule-based parser as the Telegram edit handler (no LLM call
    /// from within the tool — the tool surface is synchronous with respect to the
    /// LLM budget). For richer parsing the Telegram handler should be used directly.
    async fn apply_nl_override(
        &self,
        inquiry_id: Uuid,
        instruction_de: &str,
    ) -> Result<OfferDraft, ServiceError>;
}

// ── Calendar ──────────────────────────────────────────────────────────────────

/// Query calendar items.
#[async_trait]
pub trait CalendarService: Send + Sync {
    /// Fetch all calendar items in the given date range (inclusive).
    async fn get_range(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<CalendarItem>, ServiceError>;

    /// Find dates with available crew capacity.
    async fn find_available_slots(
        &self,
        earliest: NaiveDate,
        latest: NaiveDate,
    ) -> Result<Vec<AvailableSlot>, ServiceError>;

    /// Create a non-job calendar item (urlaub, krankheit, blocker).
    async fn create_item(
        &self,
        scheduled_date: NaiveDate,
        category: &str,
        title: &str,
        notes: Option<&str>,
        end_date: Option<NaiveDate>,
    ) -> Result<CalendarItem, ServiceError>;

    /// Patch an existing calendar item.
    async fn update_item(
        &self,
        id: Uuid,
        patch: CalendarItemPatch,
    ) -> Result<CalendarItem, ServiceError>;

    /// Delete a calendar item by ID.
    async fn delete_item(&self, id: Uuid) -> Result<(), ServiceError>;

    /// Schedule an inquiry on a date with a crew.
    async fn schedule_inquiry(
        &self,
        inquiry_id: Uuid,
        date: NaiveDate,
        crew: Vec<Uuid>,
        notes: Option<&str>,
    ) -> Result<CalendarItem, ServiceError>;

    /// Reassign a scheduled termin (change date and/or crew).
    async fn reassign_termin(
        &self,
        termin_id: Uuid,
        new_date: Option<NaiveDate>,
        new_crew: Option<Vec<Uuid>>,
    ) -> Result<CalendarItem, ServiceError>;

    /// Cancel a scheduled termin.
    async fn cancel_termin(&self, id: Uuid, reason: &str) -> Result<(), ServiceError>;

    /// Assign a single employee to a calendar item.
    async fn assign_employee(
        &self,
        calendar_item_id: Uuid,
        employee_id: Uuid,
    ) -> Result<(), ServiceError>;

    /// Fetch assignments for an employee in a date range.
    async fn get_employee_assignments(
        &self,
        employee_id: Uuid,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<EmployeeWorkloadEntry>, ServiceError>;

    /// Fetch the crew assigned to a termin (calendar item) **or** an inquiry.
    ///
    /// The `id` may reference either a `calendar_item` or an `inquiry` — both
    /// assignment tables are checked, so the caller does not need to know which
    /// kind of id it holds. Returns an empty vec when nothing is assigned.
    async fn get_assigned_crew(&self, id: Uuid) -> Result<Vec<CrewMember>, ServiceError>;

    /// Replace the crew assigned to an **inquiry** (writes `inquiry_employees`).
    ///
    /// Unlike [`CalendarService::schedule_inquiry`], this does NOT change the
    /// inquiry status and does NOT create a calendar item — it only sets the
    /// crew. The assignment date defaults to the inquiry's `scheduled_date`
    /// when `date` is `None`. Returns the resulting crew.
    async fn set_inquiry_crew(
        &self,
        inquiry_id: Uuid,
        crew: Vec<Uuid>,
        date: Option<NaiveDate>,
    ) -> Result<Vec<CrewMember>, ServiceError>;
}

// ── Customer ──────────────────────────────────────────────────────────────────

/// Query customer data.
#[async_trait]
pub trait CustomerService: Send + Sync {
    /// Fetch a customer snapshot by ID.
    async fn get(&self, id: Uuid) -> Result<CustomerSnapshot, ServiceError>;

    /// Create a new customer record. Requires at least a first/last name or a
    /// company name. Returns the created snapshot.
    async fn create(&self, new: NewCustomer) -> Result<CustomerSnapshot, ServiceError>;

    /// Full-text search across customer name and email.
    async fn search(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<CustomerSnapshot>, ServiceError>;

    /// List all inquiries for a customer.
    async fn list_inquiries_for(
        &self,
        customer_id: Uuid,
    ) -> Result<Vec<InquiryListItem>, ServiceError>;

    /// Update mutable customer fields.
    async fn update(
        &self,
        id: Uuid,
        patch: CustomerPatch,
    ) -> Result<CustomerSnapshot, ServiceError>;

    /// Append a note to a customer record.
    async fn add_note(&self, id: Uuid, text: &str) -> Result<(), ServiceError>;

    /// Merge two customers — reassigns inquiries from `merge_id` to `keep_id`, then
    /// soft-deletes `merge_id`.
    async fn merge(
        &self,
        keep_id: Uuid,
        merge_id: Uuid,
    ) -> Result<CustomerSnapshot, ServiceError>;
}

// ── Email ─────────────────────────────────────────────────────────────────────

/// Query the email inbox.
#[async_trait]
pub trait EmailService: Send + Sync {
    /// List the most recent `limit` email messages.
    async fn list_inbox(&self, limit: u32) -> Result<Vec<EmailSummary>, ServiceError>;

    /// Fetch a full email by ID including body.
    async fn get_email(&self, id: Uuid) -> Result<EmailDetail, ServiceError>;

    /// List all emails for a customer (thread view).
    async fn list_thread(&self, customer_id: Uuid) -> Result<Vec<EmailDetail>, ServiceError>;

    /// Mark an email as handled.
    async fn mark_handled(&self, id: Uuid) -> Result<(), ServiceError>;

    /// Set a label/category on an email.
    async fn categorize(&self, id: Uuid, label: &str) -> Result<(), ServiceError>;
}

// ── Invoice ───────────────────────────────────────────────────────────────────

/// Query invoices.
#[async_trait]
pub trait InvoiceService: Send + Sync {
    /// List invoices, optionally filtered by status string.
    async fn list(&self, status_filter: Option<&str>) -> Result<Vec<InvoiceSummary>, ServiceError>;

    /// Fetch a single invoice by ID.
    async fn get(&self, id: Uuid) -> Result<InvoiceDetail, ServiceError>;

    /// List payment reminders for an invoice.
    async fn list_reminders(
        &self,
        invoice_id: Uuid,
    ) -> Result<Vec<InvoiceReminder>, ServiceError>;

    /// Create a full invoice from an accepted/completed inquiry.
    ///
    /// Reads the active offer, generates the PDF, uploads to S3, inserts the invoice row,
    /// and returns a lightweight summary. Does NOT send the invoice to the customer.
    ///
    /// Returns `ServiceError::Validation` if the inquiry is not in an accepted-or-later
    /// status, or if no active offer exists.
    async fn create_from_inquiry(
        &self,
        inquiry_id: Uuid,
    ) -> Result<InvoiceSummary, ServiceError>;

    /// Update the status of an invoice (paid/overdue/written_off transitions).
    async fn update_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<InvoiceDetail, ServiceError>;

    /// Record a payment against an invoice.
    ///
    /// Inserts a row into `payment_records`. When the cumulative total of payments
    /// for this invoice reaches the invoice's brutto total, the invoice status is
    /// automatically updated to `'paid'`. Returns the new payment record's UUID.
    async fn record_payment(
        &self,
        invoice_id: Uuid,
        amount_cents: i64,
        date: NaiveDate,
        method: &str,
        ref_text: Option<&str>,
    ) -> Result<Uuid, ServiceError>;
}

// ── Employee ──────────────────────────────────────────────────────────────────

/// Query and manage employees.
#[async_trait]
pub trait EmployeeService: Send + Sync {
    /// List all employees, optionally filtering to active-only.
    async fn list(&self, active_only: bool) -> Result<Vec<EmployeeRecord>, ServiceError>;

    /// Fetch a single employee by ID.
    async fn get(&self, id: Uuid) -> Result<EmployeeRecord, ServiceError>;

    /// Fetch workload entries (assignments) for an employee in a date range.
    async fn get_workload(
        &self,
        id: Uuid,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<EmployeeWorkloadEntry>, ServiceError>;

    /// Update mutable employee fields.
    async fn update(&self, id: Uuid, patch: EmployeePatch) -> Result<EmployeeRecord, ServiceError>;

    /// Set the active flag on an employee.
    async fn set_active(&self, id: Uuid, active: bool) -> Result<(), ServiceError>;
}

// ── Estimation ────────────────────────────────────────────────────────────────

/// Status returned by `request_revision`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionStatus {
    /// Whether a new revision was successfully queued.
    pub queued: bool,
    /// Human-readable reason when `queued = false` (e.g. "no photos uploaded yet").
    pub reason: Option<String>,
    /// The UUID of the newly created `vision_revision_requests` row, when queued.
    pub request_id: Option<Uuid>,
}

/// Query and manage volume estimations.
#[async_trait]
pub trait EstimationService: Send + Sync {
    /// Fetch the latest completed estimation for an inquiry.
    async fn get(&self, inquiry_id: Uuid) -> Result<Option<EstimationSummary>, ServiceError>;

    /// Manually override the volume for an inquiry.
    async fn override_volume(
        &self,
        inquiry_id: Uuid,
        volume_m3: f64,
        notes: Option<&str>,
    ) -> Result<(), ServiceError>;

    /// Queue a vision pipeline re-run for an inquiry.
    ///
    /// Inserts a row into `vision_revision_requests` (status = `pending`).
    /// The actual re-run is picked up by the vision worker (Phase 5).
    ///
    /// Returns `RevisionStatus::queued = false` when there are no uploaded photos
    /// for the inquiry (detected by checking `volume_estimations.method`).
    async fn request_revision(&self, inquiry_id: Uuid) -> Result<RevisionStatus, ServiceError>;
}

// ── Address ───────────────────────────────────────────────────────────────────

/// Query and manage address data.
#[async_trait]
pub trait AddressService: Send + Sync {
    /// Look up the stored distance between two addresses (if computed).
    async fn get_distance(
        &self,
        from_address_id: Uuid,
        to_address_id: Uuid,
    ) -> Result<Option<DistanceResult>, ServiceError>;

    /// Update origin and/or destination addresses for an inquiry.
    async fn update_inquiry_addresses(
        &self,
        inquiry_id: Uuid,
        from: Option<AddressPatch>,
        to: Option<AddressPatch>,
    ) -> Result<(), ServiceError>;
}

// ── Settings ──────────────────────────────────────────────────────────────────

/// Query and manage application settings.
#[async_trait]
pub trait SettingsService: Send + Sync {
    /// Fetch the current pricing configuration.
    async fn get_pricing(&self) -> Result<PricingConfig, ServiceError>;
}

// ── Review / Feedback ─────────────────────────────────────────────────────────

/// Query customer reviews and internal feedback.
#[async_trait]
pub trait ReviewService: Send + Sync {
    /// List reviews, optionally in a date range.
    async fn list_reviews(
        &self,
        from: Option<NaiveDate>,
        to: Option<NaiveDate>,
    ) -> Result<Vec<ReviewRecord>, ServiceError>;

    /// List feedback reports, optionally only unresolved.
    async fn list_feedback(
        &self,
        unresolved_only: bool,
    ) -> Result<Vec<FeedbackRecord>, ServiceError>;

    /// File a new feedback report (bug or feature request) into the pipeline.
    ///
    /// `report_type` must be "bug" or "feature"; `priority` one of
    /// "low" / "medium" / "high" / "critical". Returns `Validation` on bad enums.
    async fn create_feedback(
        &self,
        report_type: &str,
        priority: &str,
        title: &str,
        description: Option<&str>,
        location: Option<&str>,
    ) -> Result<FeedbackRecord, ServiceError>;

    /// Set a draft response on a review.
    async fn set_review_response_draft(
        &self,
        id: Uuid,
        draft: &str,
    ) -> Result<(), ServiceError>;

    /// Mark a feedback report as resolved.
    async fn mark_feedback_resolved(
        &self,
        id: Uuid,
        notes: Option<&str>,
    ) -> Result<(), ServiceError>;
}

// ── Metrics ───────────────────────────────────────────────────────────────────

/// Aggregate pipeline and revenue metrics.
#[async_trait]
pub trait MetricsService: Send + Sync {
    /// Return pipeline funnel metrics for the given date range.
    async fn pipeline(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<PipelineMetrics, ServiceError>;
}

// ── Todos ─────────────────────────────────────────────────────────────────────

/// Manage agent to-do items.
#[async_trait]
pub trait TodoService: Send + Sync {
    /// Create a new to-do item.
    async fn create(
        &self,
        session_id: Uuid,
        text: &str,
        due: Option<NaiveDate>,
    ) -> Result<TodoRecord, ServiceError>;

    /// List to-do items, optionally filtering to open-only.
    async fn list(
        &self,
        session_id: Uuid,
        open_only: bool,
    ) -> Result<Vec<TodoRecord>, ServiceError>;

    /// Resolve a to-do item.
    async fn resolve(&self, id: Uuid) -> Result<(), ServiceError>;
}

// ── Reminders ───────────────────────────────────────────────────────────────────

/// Manage active reminders that the background tick fires back to Telegram.
#[async_trait]
pub trait ReminderService: Send + Sync {
    /// Create a reminder for `chat_id`. `recurring` reminders re-fire every ~3h
    /// within business hours until cancelled; otherwise it fires once.
    async fn create(
        &self,
        chat_id: i64,
        text: &str,
        due_at: chrono::DateTime<chrono::Utc>,
        recurring: bool,
    ) -> Result<ReminderRecord, ServiceError>;

    /// List reminders for a chat, optionally only the active ones.
    async fn list(
        &self,
        chat_id: i64,
        active_only: bool,
    ) -> Result<Vec<ReminderRecord>, ServiceError>;

    /// Deactivate a reminder ("turn it off"). Returns the cancelled record.
    async fn cancel(&self, id: Uuid) -> Result<ReminderRecord, ServiceError>;
}
