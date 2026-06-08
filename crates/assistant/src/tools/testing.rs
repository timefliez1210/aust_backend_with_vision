//! Test helpers: mock implementations of every `aust_core::services` trait,
//! plus a `mock_bundle()` factory for use in tool tests.
//!
//! Compiled only under `cfg(test)` to keep mocks out of release builds.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{NaiveDate, Utc};
use uuid::Uuid;

use aust_core::models::{CustomerSnapshot, InquiryListItem, InquiryResponse, InquiryStatus, Services};
use aust_core::services::{
    AddressPatch, AddressService, AvailableSlot, CalendarItem, CalendarItemPatch, CalendarService,
    ComputedLineItem, CrewMember, CustomerPatch, CustomerService, DistanceResult, EmailDetail, EmailService,
    EmailSummary, EmployeePatch, EmployeeRecord, EmployeeService, EmployeeWorkloadEntry,
    EstimationService, EstimationSummary, FeedbackRecord, InquiryService, InvoiceDetail,
    InvoiceReminder, InvoiceService, InvoiceSummary, MetricsService, OfferComputation, OfferDraft,
    OfferOverrides as CoreOfferOverrides, OfferPreview, OfferService, OfferVersion,
    PipelineMetrics, PricingConfig, ReviewRecord, ReviewService, RevisionStatus, ServiceBundle,
    ServiceError, SettingsService, TodoRecord, TodoService,
};

// ── Inquiry mock ──────────────────────────────────────────────────────────────

pub struct MockInquiryService {
    pub inquiry_id: Uuid,
}

#[async_trait]
impl InquiryService for MockInquiryService {
    async fn get_inquiry(&self, id: Uuid) -> Result<InquiryResponse, ServiceError> {
        if id != self.inquiry_id {
            return Err(ServiceError::NotFound(format!("inquiry {id}")));
        }
        Ok(InquiryResponse {
            id,
            status: InquiryStatus::Estimated,
            source: "test".to_string(),
            services: Default::default(),
            volume_m3: Some(20.0),
            distance_km: Some(15.0),
            scheduled_date: None,
            start_time: chrono::NaiveTime::from_hms_opt(8, 0, 0).unwrap_or_default(),
            end_time: chrono::NaiveTime::from_hms_opt(17, 0, 0).unwrap_or_default(),
            notes: None,
            customer_message: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            offer_sent_at: None,
            accepted_at: None,
            service_type: None,
            submission_mode: None,
            recipient: None,
            billing_address: None,
            effective_billing_address: None,
            customer: None,
            origin_address: None,
            destination_address: None,
            stop_address: None,
            estimation: None,
            items: vec![],
            offer: None,
            estimations: vec![],
            employees: vec![],
            end_date: None,
            is_multi_day: false,
            has_pauschale: false,
        })
    }

    async fn list_inquiries(
        &self,
        _status_filter: Option<&str>,
        _limit: u32,
    ) -> Result<Vec<InquiryListItem>, ServiceError> {
        Ok(vec![])
    }

    async fn search_inquiries(
        &self,
        _query: &str,
        _limit: u32,
    ) -> Result<Vec<InquiryListItem>, ServiceError> {
        Ok(vec![])
    }

    async fn add_note(
        &self,
        _id: Uuid,
        _text: &str,
        _author_role: &str,
    ) -> Result<(), ServiceError> {
        Ok(())
    }

    async fn update_status(
        &self,
        id: Uuid,
        _new_status: &str,
        _reason: Option<&str>,
    ) -> Result<InquiryResponse, ServiceError> {
        self.get_inquiry(id).await
    }

    async fn set_services(&self, _id: Uuid, _services: Services) -> Result<(), ServiceError> {
        Ok(())
    }

    async fn cancel_inquiry(&self, _id: Uuid, _reason: &str) -> Result<(), ServiceError> {
        Ok(())
    }
}

// ── Offer mock ────────────────────────────────────────────────────────────────

pub struct MockOfferService {
    pub inquiry_id: Uuid,
    pub offer_id: Uuid,
}

#[async_trait]
impl OfferService for MockOfferService {
    #[allow(deprecated)]
    async fn draft_offer(&self, inquiry_id: Uuid) -> Result<OfferDraft, ServiceError> {
        self.commit_offer_draft(inquiry_id, None).await
    }

    async fn preview_offer(
        &self,
        inquiry_id: Uuid,
        _overrides: Option<CoreOfferOverrides>,
    ) -> Result<OfferPreview, ServiceError> {
        if inquiry_id != self.inquiry_id {
            return Err(ServiceError::NotFound(format!("inquiry {inquiry_id}")));
        }
        Ok(OfferPreview {
            inquiry_id,
            computation: OfferComputation {
                persons: 3,
                hours: 5.0,
                rate_cents: 4500,
                total_netto_cents: 67500,
                total_brutto_cents: 80325,
                line_items: vec![ComputedLineItem {
                    description: "3 Umzugshelfer".to_string(),
                    quantity: 5.0,
                    unit_price_eur: 45.0,
                    flat_total_eur: None,
                    is_labor: true,
                    line_total_cents: 67500,
                }],
                saturday_surcharge_applied: false,
                fahrt_cents: 0,
            },
            customer_name: "Max Mustermann".to_string(),
            moving_date: "01.06.2026".to_string(),
            volume_m3: 20.0,
            distance_km: 15.0,
        })
    }

    async fn commit_offer_draft(
        &self,
        inquiry_id: Uuid,
        _overrides: Option<CoreOfferOverrides>,
    ) -> Result<OfferDraft, ServiceError> {
        if inquiry_id != self.inquiry_id {
            return Err(ServiceError::NotFound(format!("inquiry {inquiry_id}")));
        }
        Ok(OfferDraft {
            offer_id: self.offer_id,
            inquiry_id,
            status: "draft".to_string(),
            persons: 3,
            hours: 5.0,
            rate_cents: 4500,
            total_netto_cents: 67500,
            total_brutto_cents: 80325,
            offer_number: Some("2026-0001".to_string()),
        })
    }

    async fn get_offer(&self, _inquiry_id: Uuid) -> Result<Option<OfferDraft>, ServiceError> {
        Ok(None)
    }

    async fn list_offer_versions(
        &self,
        _inquiry_id: Uuid,
    ) -> Result<Vec<OfferVersion>, ServiceError> {
        Ok(vec![OfferVersion {
            offer_id: self.offer_id,
            offer_number: Some("2026-0001".to_string()),
            status: "draft".to_string(),
            persons: 3,
            hours: 5.0,
            total_brutto_cents: 80325,
            created_at: Utc::now(),
        }])
    }

    async fn apply_nl_override(
        &self,
        inquiry_id: Uuid,
        _instruction_de: &str,
    ) -> Result<OfferDraft, ServiceError> {
        // Delegate to commit_offer_draft for the mock — just return a draft.
        self.commit_offer_draft(inquiry_id, None).await
    }

    async fn mark_offer_accepted(
        &self,
        _inquiry_id: Uuid,
        _source: &str,
    ) -> Result<(), ServiceError> {
        Ok(())
    }

    async fn mark_offer_rejected(
        &self,
        _inquiry_id: Uuid,
        _source: &str,
        _reason: Option<&str>,
    ) -> Result<(), ServiceError> {
        Ok(())
    }
}

// ── Calendar mock ─────────────────────────────────────────────────────────────

pub struct MockCalendarService;

#[async_trait]
impl CalendarService for MockCalendarService {
    async fn get_range(
        &self,
        from: NaiveDate,
        _to: NaiveDate,
    ) -> Result<Vec<CalendarItem>, ServiceError> {
        Ok(vec![CalendarItem {
            id: Uuid::new_v4(),
            title: "Testumzug".to_string(),
            category: "moving".to_string(),
            scheduled_date: Some(from),
            end_date: None,
            kind: "termin".to_string(),
        }])
    }

    async fn find_available_slots(
        &self,
        earliest: NaiveDate,
        _latest: NaiveDate,
    ) -> Result<Vec<AvailableSlot>, ServiceError> {
        Ok(vec![AvailableSlot { date: earliest, available_crew: 3 }])
    }

    async fn create_item(
        &self,
        scheduled_date: NaiveDate,
        category: &str,
        title: &str,
        _notes: Option<&str>,
        end_date: Option<NaiveDate>,
    ) -> Result<CalendarItem, ServiceError> {
        Ok(CalendarItem {
            id: Uuid::new_v4(),
            title: title.to_string(),
            category: category.to_string(),
            scheduled_date: Some(scheduled_date),
            end_date,
            kind: "termin".to_string(),
        })
    }

    async fn update_item(
        &self,
        id: Uuid,
        patch: CalendarItemPatch,
    ) -> Result<CalendarItem, ServiceError> {
        Ok(CalendarItem {
            id,
            title: patch.title.unwrap_or_else(|| "Updated".to_string()),
            category: patch.category.unwrap_or_else(|| "moving".to_string()),
            scheduled_date: patch.scheduled_date,
            end_date: patch.end_date,
            kind: "termin".to_string(),
        })
    }

    async fn delete_item(&self, _id: Uuid) -> Result<(), ServiceError> {
        Ok(())
    }

    async fn schedule_inquiry(
        &self,
        _inquiry_id: Uuid,
        date: NaiveDate,
        _crew: Vec<Uuid>,
        _notes: Option<&str>,
    ) -> Result<CalendarItem, ServiceError> {
        Ok(CalendarItem {
            id: Uuid::new_v4(),
            title: "Umzug".to_string(),
            category: "moving".to_string(),
            scheduled_date: Some(date),
            end_date: None,
            kind: "termin".to_string(),
        })
    }

    async fn reassign_termin(
        &self,
        termin_id: Uuid,
        new_date: Option<NaiveDate>,
        _new_crew: Option<Vec<Uuid>>,
    ) -> Result<CalendarItem, ServiceError> {
        Ok(CalendarItem {
            id: termin_id,
            title: "Umzug".to_string(),
            category: "moving".to_string(),
            scheduled_date: new_date,
            end_date: None,
            kind: "termin".to_string(),
        })
    }

    async fn cancel_termin(&self, _id: Uuid, _reason: &str) -> Result<(), ServiceError> {
        Ok(())
    }

    async fn assign_employee(
        &self,
        _calendar_item_id: Uuid,
        _employee_id: Uuid,
    ) -> Result<(), ServiceError> {
        Ok(())
    }

    async fn get_employee_assignments(
        &self,
        _employee_id: Uuid,
        from: NaiveDate,
        _to: NaiveDate,
    ) -> Result<Vec<EmployeeWorkloadEntry>, ServiceError> {
        Ok(vec![EmployeeWorkloadEntry {
            date: from,
            inquiry_id: Some(Uuid::new_v4()),
            calendar_item_id: None,
            title: "Umzug".to_string(),
            category: "moving".to_string(),
        }])
    }

    async fn get_assigned_crew(&self, _id: Uuid) -> Result<Vec<CrewMember>, ServiceError> {
        Ok(vec![CrewMember {
            employee_id: Uuid::new_v4(),
            first_name: "Test".to_string(),
            last_name: "Mitarbeiter".to_string(),
            job_date: NaiveDate::from_ymd_opt(2026, 6, 12).unwrap(),
            source: "termin".to_string(),
        }])
    }

    async fn set_inquiry_crew(
        &self,
        _inquiry_id: Uuid,
        crew: Vec<Uuid>,
        date: Option<NaiveDate>,
    ) -> Result<Vec<CrewMember>, ServiceError> {
        let job_date = date.unwrap_or_else(|| NaiveDate::from_ymd_opt(2026, 6, 12).unwrap());
        Ok(crew
            .into_iter()
            .map(|employee_id| CrewMember {
                employee_id,
                first_name: "Test".to_string(),
                last_name: "Mitarbeiter".to_string(),
                job_date,
                source: "auftrag".to_string(),
            })
            .collect())
    }
}

// ── Customer mock ─────────────────────────────────────────────────────────────

pub struct MockCustomerService {
    pub customer_id: Uuid,
}

fn mock_customer_snapshot(id: Uuid) -> CustomerSnapshot {
    CustomerSnapshot {
        id,
        name: Some("Max Mustermann".to_string()),
        salutation: Some("Herr".to_string()),
        first_name: Some("Max".to_string()),
        last_name: Some("Mustermann".to_string()),
        email: Some("max@example.com".to_string()),
        phone: Some("+491700000000".to_string()),
        customer_type: None,
        company_name: None,
    }
}

#[async_trait]
impl CustomerService for MockCustomerService {
    async fn get(&self, id: Uuid) -> Result<CustomerSnapshot, ServiceError> {
        if id != self.customer_id {
            return Err(ServiceError::NotFound(format!("customer {id}")));
        }
        Ok(mock_customer_snapshot(id))
    }

    async fn create(
        &self,
        new: aust_core::services::NewCustomer,
    ) -> Result<CustomerSnapshot, ServiceError> {
        let mut snap = mock_customer_snapshot(Uuid::new_v4());
        snap.first_name = new.first_name;
        snap.last_name = new.last_name;
        snap.phone = new.phone;
        snap.email = new.email;
        Ok(snap)
    }

    async fn search(
        &self,
        _query: &str,
        _limit: u32,
    ) -> Result<Vec<CustomerSnapshot>, ServiceError> {
        Ok(vec![mock_customer_snapshot(self.customer_id)])
    }

    async fn list_inquiries_for(
        &self,
        _customer_id: Uuid,
    ) -> Result<Vec<InquiryListItem>, ServiceError> {
        Ok(vec![])
    }

    async fn update(
        &self,
        id: Uuid,
        _patch: CustomerPatch,
    ) -> Result<CustomerSnapshot, ServiceError> {
        Ok(mock_customer_snapshot(id))
    }

    async fn add_note(&self, _id: Uuid, _text: &str) -> Result<(), ServiceError> {
        Ok(())
    }

    async fn merge(&self, keep_id: Uuid, _merge_id: Uuid) -> Result<CustomerSnapshot, ServiceError> {
        Ok(mock_customer_snapshot(keep_id))
    }
}

// ── Email mock ────────────────────────────────────────────────────────────────

pub struct MockEmailService;

#[async_trait]
impl EmailService for MockEmailService {
    async fn list_inbox(&self, _limit: u32) -> Result<Vec<EmailSummary>, ServiceError> {
        Ok(vec![EmailSummary {
            id: Uuid::new_v4(),
            subject: "Test Anfrage".to_string(),
            from_address: Some("kunde@example.com".to_string()),
            status: Some("unread".to_string()),
            created_at: Utc::now(),
        }])
    }

    async fn get_email(&self, id: Uuid) -> Result<EmailDetail, ServiceError> {
        Ok(EmailDetail {
            id,
            subject: "Test".to_string(),
            from_address: Some("kunde@example.com".to_string()),
            to_address: Some("angebot@aust-umzuege.de".to_string()),
            body_text: Some("Hallo".to_string()),
            status: Some("received".to_string()),
            direction: Some("inbound".to_string()),
            created_at: Utc::now(),
        })
    }

    async fn list_thread(&self, _customer_id: Uuid) -> Result<Vec<EmailDetail>, ServiceError> {
        Ok(vec![])
    }

    async fn mark_handled(&self, _id: Uuid) -> Result<(), ServiceError> {
        Ok(())
    }

    async fn categorize(&self, _id: Uuid, _label: &str) -> Result<(), ServiceError> {
        Ok(())
    }
}

// ── Invoice mock ──────────────────────────────────────────────────────────────

pub struct MockInvoiceService;

#[async_trait]
impl InvoiceService for MockInvoiceService {
    async fn create_from_inquiry(
        &self,
        inquiry_id: Uuid,
    ) -> Result<InvoiceSummary, ServiceError> {
        Ok(InvoiceSummary {
            id: Uuid::new_v4(),
            invoice_number: format!("R-{inquiry_id}"),
            status: "draft".to_string(),
            due_date: None,
            sent_at: None,
        })
    }

    async fn list(
        &self,
        _status_filter: Option<&str>,
    ) -> Result<Vec<InvoiceSummary>, ServiceError> {
        Ok(vec![InvoiceSummary {
            id: Uuid::new_v4(),
            invoice_number: "R-2026-0001".to_string(),
            status: "sent".to_string(),
            due_date: None,
            sent_at: Some(Utc::now()),
        }])
    }

    async fn get(&self, id: Uuid) -> Result<InvoiceDetail, ServiceError> {
        Ok(InvoiceDetail {
            id,
            invoice_number: "R-2026-0001".to_string(),
            inquiry_id: None,
            status: "sent".to_string(),
            due_date: None,
            sent_at: Some(Utc::now()),
            created_at: Utc::now(),
        })
    }

    async fn list_reminders(
        &self,
        _invoice_id: Uuid,
    ) -> Result<Vec<InvoiceReminder>, ServiceError> {
        Ok(vec![])
    }

    async fn update_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<InvoiceDetail, ServiceError> {
        Ok(InvoiceDetail {
            id,
            invoice_number: "R-2026-0001".to_string(),
            inquiry_id: None,
            status: status.to_string(),
            due_date: None,
            sent_at: Some(Utc::now()),
            created_at: Utc::now(),
        })
    }

    async fn record_payment(
        &self,
        _invoice_id: Uuid,
        _amount_cents: i64,
        _date: NaiveDate,
        _method: &str,
        _ref_text: Option<&str>,
    ) -> Result<Uuid, ServiceError> {
        Ok(Uuid::new_v4())
    }
}

// ── Employee mock ─────────────────────────────────────────────────────────────

pub struct MockEmployeeService {
    pub employee_id: Uuid,
}

fn mock_employee(id: Uuid) -> EmployeeRecord {
    EmployeeRecord {
        id,
        first_name: "Anna".to_string(),
        last_name: "Schmidt".to_string(),
        email: Some("anna@example.com".to_string()),
        phone: Some("+491700001111".to_string()),
        role: None,
        active: true,
    }
}

#[async_trait]
impl EmployeeService for MockEmployeeService {
    async fn list(&self, _active_only: bool) -> Result<Vec<EmployeeRecord>, ServiceError> {
        Ok(vec![mock_employee(self.employee_id)])
    }

    async fn get(&self, id: Uuid) -> Result<EmployeeRecord, ServiceError> {
        if id != self.employee_id {
            return Err(ServiceError::NotFound(format!("employee {id}")));
        }
        Ok(mock_employee(id))
    }

    async fn get_workload(
        &self,
        _id: Uuid,
        from: NaiveDate,
        _to: NaiveDate,
    ) -> Result<Vec<EmployeeWorkloadEntry>, ServiceError> {
        Ok(vec![EmployeeWorkloadEntry {
            date: from,
            inquiry_id: Some(Uuid::new_v4()),
            calendar_item_id: None,
            title: "Umzug".to_string(),
            category: "moving".to_string(),
        }])
    }

    async fn update(&self, id: Uuid, _patch: EmployeePatch) -> Result<EmployeeRecord, ServiceError> {
        Ok(mock_employee(id))
    }

    async fn set_active(&self, _id: Uuid, _active: bool) -> Result<(), ServiceError> {
        Ok(())
    }
}

// ── Estimation mock ───────────────────────────────────────────────────────────

pub struct MockEstimationService;

#[async_trait]
impl EstimationService for MockEstimationService {
    async fn get(&self, _inquiry_id: Uuid) -> Result<Option<EstimationSummary>, ServiceError> {
        Ok(Some(EstimationSummary {
            id: Uuid::new_v4(),
            method: "vision".to_string(),
            status: "completed".to_string(),
            total_volume_m3: Some(22.5),
            confidence_score: Some(0.85),
            item_count: 30,
            created_at: Utc::now(),
        }))
    }

    async fn override_volume(
        &self,
        _inquiry_id: Uuid,
        _volume_m3: f64,
        _notes: Option<&str>,
    ) -> Result<(), ServiceError> {
        Ok(())
    }

    async fn request_revision(&self, _inquiry_id: Uuid) -> Result<RevisionStatus, ServiceError> {
        Ok(RevisionStatus {
            queued: true,
            reason: None,
            request_id: Some(Uuid::new_v4()),
        })
    }
}

// ── Address mock ──────────────────────────────────────────────────────────────

pub struct MockAddressService;

#[async_trait]
impl AddressService for MockAddressService {
    async fn get_distance(
        &self,
        from_address_id: Uuid,
        to_address_id: Uuid,
    ) -> Result<Option<DistanceResult>, ServiceError> {
        Ok(Some(DistanceResult {
            from_address_id,
            to_address_id,
            distance_km: 15.0,
            duration_minutes: Some(22.0),
        }))
    }

    async fn update_inquiry_addresses(
        &self,
        _inquiry_id: Uuid,
        _from: Option<AddressPatch>,
        _to: Option<AddressPatch>,
    ) -> Result<(), ServiceError> {
        Ok(())
    }
}

// ── Settings mock ─────────────────────────────────────────────────────────────

pub struct MockSettingsService;

#[async_trait]
impl SettingsService for MockSettingsService {
    async fn get_pricing(&self) -> Result<PricingConfig, ServiceError> {
        Ok(PricingConfig {
            base_rate_eur: 45.0,
            saturday_surcharge_pct: 0.0,
            vat_rate_pct: 19.0,
            min_hours: 2.0,
        })
    }
}

// ── Review mock ───────────────────────────────────────────────────────────────

pub struct MockReviewService;

#[async_trait]
impl ReviewService for MockReviewService {
    async fn list_reviews(
        &self,
        _from: Option<NaiveDate>,
        _to: Option<NaiveDate>,
    ) -> Result<Vec<ReviewRecord>, ServiceError> {
        Ok(vec![])
    }

    async fn list_feedback(
        &self,
        _unresolved_only: bool,
    ) -> Result<Vec<FeedbackRecord>, ServiceError> {
        Ok(vec![FeedbackRecord {
            id: Uuid::new_v4(),
            inquiry_id: None,
            category: Some("bug".to_string()),
            description: "Test".to_string(),
            resolved: false,
            notes: None,
            created_at: Utc::now(),
        }])
    }

    async fn create_feedback(
        &self,
        report_type: &str,
        priority: &str,
        title: &str,
        description: Option<&str>,
        _location: Option<&str>,
    ) -> Result<FeedbackRecord, ServiceError> {
        Ok(FeedbackRecord {
            id: Uuid::new_v4(),
            inquiry_id: None,
            category: Some(format!("{report_type}/{priority}")),
            description: description.unwrap_or(title).to_string(),
            resolved: false,
            notes: None,
            created_at: Utc::now(),
        })
    }

    async fn set_review_response_draft(
        &self,
        _id: Uuid,
        _draft: &str,
    ) -> Result<(), ServiceError> {
        Ok(())
    }

    async fn mark_feedback_resolved(
        &self,
        _id: Uuid,
        _notes: Option<&str>,
    ) -> Result<(), ServiceError> {
        Ok(())
    }
}

// ── Metrics mock ──────────────────────────────────────────────────────────────

pub struct MockMetricsService;

#[async_trait]
impl MetricsService for MockMetricsService {
    async fn pipeline(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<PipelineMetrics, ServiceError> {
        Ok(PipelineMetrics {
            period_from: from,
            period_to: to,
            inquiries_total: 10,
            offers_sent: 7,
            scheduled: 5,
            invoiced: 4,
            paid: 3,
            revenue_netto_cents: 250000,
        })
    }
}

// ── Todo mock ─────────────────────────────────────────────────────────────────

pub struct MockTodoService;

#[async_trait]
impl TodoService for MockTodoService {
    async fn create(
        &self,
        session_id: Uuid,
        text: &str,
        due: Option<NaiveDate>,
    ) -> Result<TodoRecord, ServiceError> {
        Ok(TodoRecord {
            id: Uuid::new_v4(),
            session_id,
            text: text.to_string(),
            due,
            status: "open".to_string(),
            created_at: Utc::now(),
            resolved_at: None,
        })
    }

    async fn list(
        &self,
        session_id: Uuid,
        _open_only: bool,
    ) -> Result<Vec<TodoRecord>, ServiceError> {
        Ok(vec![TodoRecord {
            id: Uuid::new_v4(),
            session_id,
            text: "Anrufen".to_string(),
            due: None,
            status: "open".to_string(),
            created_at: Utc::now(),
            resolved_at: None,
        }])
    }

    async fn resolve(&self, _id: Uuid) -> Result<(), ServiceError> {
        Ok(())
    }
}

// ── Bundle factory ────────────────────────────────────────────────────────────

/// A pre-wired `ServiceBundle` with all-mock implementations and a fixed inquiry/offer ID.
pub fn mock_bundle(inquiry_id: Uuid, customer_id: Uuid, offer_id: Uuid) -> ServiceBundle {
    ServiceBundle {
        inquiries: Arc::new(MockInquiryService { inquiry_id }),
        offers: Arc::new(MockOfferService { inquiry_id, offer_id }),
        calendar: Arc::new(MockCalendarService),
        customers: Arc::new(MockCustomerService { customer_id }),
        emails: Arc::new(MockEmailService),
        invoices: Arc::new(MockInvoiceService),
        employees: Arc::new(MockEmployeeService { employee_id: customer_id }),
        estimations: Arc::new(MockEstimationService),
        addresses: Arc::new(MockAddressService),
        settings: Arc::new(MockSettingsService),
        reviews: Arc::new(MockReviewService),
        metrics: Arc::new(MockMetricsService),
        todos: Arc::new(MockTodoService),
    }
}
