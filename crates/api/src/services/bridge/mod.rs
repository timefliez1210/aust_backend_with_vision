//! Bridge implementations of the `aust-core` service traits.
//!
//! Each `*ServiceImpl` struct implements one trait from `aust_core::services::traits`
//! and delegates to the existing api repositories / services. They are constructed
//! once during app startup and grouped into a [`aust_core::services::ServiceBundle`]
//! that is passed into the assistant `ToolCtx`.

/// Map `sqlx::Error` into `ServiceError::Db`. Used internally by impls.
pub(crate) fn map_sqlx(e: sqlx::Error) -> aust_core::services::ServiceError {
    aust_core::services::ServiceError::Db(anyhow::Error::new(e))
}

pub mod address_service_impl;
pub mod calendar_service_impl;
pub mod customer_service_impl;
pub mod email_service_impl;
pub mod employee_service_impl;
pub mod estimation_service_impl;
pub mod inquiry_service_impl;
pub mod invoice_service_impl;
pub mod metrics_service_impl;
pub mod offer_service_impl;
pub mod reminder_service_impl;
pub mod review_service_impl;
pub mod settings_service_impl;
pub mod todo_service_impl;

pub use address_service_impl::AddressServiceImpl;
pub use calendar_service_impl::CalendarServiceImpl;
pub use customer_service_impl::CustomerServiceImpl;
pub use email_service_impl::EmailServiceImpl;
pub use employee_service_impl::EmployeeServiceImpl;
pub use estimation_service_impl::EstimationServiceImpl;
pub use inquiry_service_impl::InquiryServiceImpl;
pub use invoice_service_impl::InvoiceServiceImpl;
pub use metrics_service_impl::MetricsServiceImpl;
pub use offer_service_impl::OfferServiceImpl;
pub use reminder_service_impl::ReminderServiceImpl;
pub use review_service_impl::ReviewServiceImpl;
pub use settings_service_impl::SettingsServiceImpl;
pub use todo_service_impl::TodoServiceImpl;

use std::sync::Arc;
use aust_core::services::ServiceBundle;

/// Construct a [`ServiceBundle`] using the shared database pool and config.
///
/// Call once at startup and clone the result for each request context.
pub fn build_service_bundle(
    pool: sqlx::PgPool,
    config: Arc<aust_core::Config>,
    storage: Arc<dyn aust_storage::StorageProvider>,
) -> ServiceBundle {
    ServiceBundle {
        inquiries: Arc::new(InquiryServiceImpl::new(pool.clone())),
        offers: Arc::new(OfferServiceImpl::new(pool.clone(), config.clone(), storage.clone())),
        calendar: Arc::new(CalendarServiceImpl::new(pool.clone())),
        customers: Arc::new(CustomerServiceImpl::new(pool.clone())),
        emails: Arc::new(EmailServiceImpl::new(pool.clone(), config.clone())),
        invoices: Arc::new(InvoiceServiceImpl::new(pool.clone(), storage.clone())),
        employees: Arc::new(EmployeeServiceImpl::new(pool.clone())),
        estimations: Arc::new(EstimationServiceImpl::new(pool.clone())),
        addresses: Arc::new(AddressServiceImpl::new(pool.clone())),
        settings: Arc::new(SettingsServiceImpl::new(pool.clone(), config)),
        reviews: Arc::new(ReviewServiceImpl::new(pool.clone())),
        metrics: Arc::new(MetricsServiceImpl::new(pool.clone())),
        todos: Arc::new(TodoServiceImpl::new(pool.clone())),
        reminders: Arc::new(ReminderServiceImpl::new(pool)),
    }
}
