//! `ServiceBundle` — groups all service trait objects for injection into `ToolCtx`.

use std::sync::Arc;

use super::traits::{
    AddressService, CalendarService, CustomerService, EmailService, EmployeeService,
    EstimationService, InquiryService, InvoiceService, MetricsService, OfferService,
    ReviewService, SettingsService, TodoService,
};

/// A cloneable bundle of all domain service trait objects.
///
/// Constructed once at API startup and injected into `ToolCtx` for every tool
/// execution. The assistant crate only sees the trait interface; all concrete
/// DB / pipeline logic lives in `crates/api`.
#[derive(Clone)]
pub struct ServiceBundle {
    pub inquiries: Arc<dyn InquiryService>,
    pub offers: Arc<dyn OfferService>,
    pub calendar: Arc<dyn CalendarService>,
    pub customers: Arc<dyn CustomerService>,
    pub emails: Arc<dyn EmailService>,
    pub invoices: Arc<dyn InvoiceService>,
    pub employees: Arc<dyn EmployeeService>,
    pub estimations: Arc<dyn EstimationService>,
    pub addresses: Arc<dyn AddressService>,
    pub settings: Arc<dyn SettingsService>,
    pub reviews: Arc<dyn ReviewService>,
    pub metrics: Arc<dyn MetricsService>,
    pub todos: Arc<dyn TodoService>,
}
