//! Repository layer — centralises all SQL queries behind typed functions.
//!
//! Each sub-module groups queries by the primary table they operate on.
//! Repository functions take `&PgPool` (or a transaction) as first argument
//! and return domain row types.

pub(crate) mod address_repo;
pub(crate) mod admin_repo;
pub(crate) mod auth_repo;
pub(crate) mod calendar_item_repo;
pub(crate) mod calendar_repo;
pub(crate) mod customer_auth_repo;
pub(crate) mod customer_repo;
pub(crate) mod email_repo;
pub(crate) mod employee_repo;
pub(crate) mod feedback_repo;
pub(crate) mod estimation_repo;
pub(crate) mod inquiry_repo;
pub(crate) mod invoice_repo;
pub(crate) mod offer_repo;
pub(crate) mod review_repo;

// Re-export row types that are used across multiple modules.
pub(crate) use address_repo::AddressRow;
pub(crate) use customer_repo::CustomerRow;
