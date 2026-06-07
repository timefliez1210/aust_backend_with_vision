//! Service traits — domain-level abstractions that decouple the assistant crate
//! from `crates/api`.
//!
//! `crates/api` provides concrete implementations (`*ServiceImpl`) in its
//! `services/bridge/` module. `crates/assistant` consumes these traits via the
//! `ServiceBundle` injected into `ToolCtx`. This breaks the circular dependency:
//! assistant → core ← api, instead of assistant ↔ api.

pub mod error;
pub mod traits;
pub mod bundle;

pub use bundle::ServiceBundle;
pub use error::ServiceError;
pub use traits::{
    AddressService, AddressPatch, AvailableSlot, CalendarItem, CalendarItemPatch, CalendarService,
    ComputedLineItem, CrewMember, CustomerPatch, CustomerService, DistanceResult, EmailDetail, EmailService,
    EmailSummary, EmployeePatch, EmployeeRecord, EmployeeService, EmployeeWorkloadEntry,
    EstimationService, EstimationSummary, FeedbackRecord, InquiryService, InvoiceDetail,
    InvoiceReminder, InvoiceService, InvoiceSummary, MetricsService, OfferComputation, OfferDraft,
    OfferOverrides, OfferPreview, OfferService, OfferVersion, PipelineMetrics, PricingConfig,
    ReviewRecord, ReviewService, RevisionStatus, SettingsService, TodoRecord, TodoService,
};
