//! Which company (tenant) the running task works for.
//!
//! The tenant travels as a tokio task-local: auth middleware wraps each request in
//! [`scope`], and the database pool copies it into the Postgres session setting
//! `app.tenant_id` on every connection acquire (see `aust_api::create_pool`). SQL
//! never has to name the tenant — column defaults and row-level security read
//! `current_tenant_id()` instead. See `docs/MULTI_TENANT.md`.
//!
//! Outside a scope (background jobs, tests, a `tokio::spawn` that did not carry the
//! scope over) [`current`] is `None` and the database falls back to [`AUST`] while
//! Aust is the only tenant.

use serde::{Deserialize, Serialize};
use std::future::Future;
use uuid::Uuid;

/// A tenant's id (`tenants.id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(transparent)]
#[serde(transparent)]
pub struct TenantId(pub Uuid);

/// Aust Umzüge — tenant #1, owner of every row that existed before multi-tenancy.
/// Must match the id seeded in `migrations/20261004120000_tenants.sql`.
pub const AUST: TenantId = TenantId(Uuid::from_u128(0x0190aa57_0000_7000_8000_000000000001));

/// A company's own words: names, phone, review link. Loaded from `tenants` for the
/// running tenant ([`profile`]) and handed to
/// whatever writes a mail, subject or document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct TenantProfile {
    pub id: TenantId,
    /// Full company name — invoices, reminders, the register export.
    pub name: String,
    /// Short name — OTP mails and short signatures.
    pub short_name: String,
    /// Brand as written in offer mails and auto-replies.
    pub brand_name: String,
    /// Owner / supervisor, printed on timesheets.
    pub owner_name: String,
    /// Phone number exactly as printed in customer mails.
    pub phone: String,
    /// Home town, for "ein Umzugsunternehmen in …".
    pub city: String,
    /// Link for review requests.
    pub review_url: String,
}

/// The profile of the tenant this pool connection works for (`current_tenant_id()`).
pub async fn profile(pool: &sqlx::PgPool) -> Result<TenantProfile, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, name, short_name, brand_name, owner_name, phone, city, review_url \
         FROM tenants WHERE id = current_tenant_id()",
    )
    .fetch_one(pool)
    .await
}

tokio::task_local! {
    static CURRENT: TenantId;
}

/// The tenant of the running task, if it runs inside [`scope`].
pub fn current() -> Option<TenantId> {
    CURRENT.try_with(|t| *t).ok()
}

/// Run `f` on behalf of `tenant`. Every pool connection `f` acquires works for it.
pub async fn scope<F: Future>(tenant: TenantId, f: F) -> F::Output {
    CURRENT.scope(tenant, f).await
}

/// `tokio::spawn` that keeps the caller's tenant. A plain `tokio::spawn` starts
/// outside any scope.
pub fn spawn<F>(f: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match current() {
        Some(t) => tokio::spawn(CURRENT.scope(t, f)),
        None => tokio::spawn(f),
    }
}

/// Value for the `app.tenant_id` session setting: the tenant's id, or `""` outside
/// a scope (which `current_tenant_id()` treats as unset).
pub fn session_value() -> String {
    current().map(|t| t.0.to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scope_sets_and_spawn_keeps_the_tenant() {
        assert_eq!(current(), None);
        let other = TenantId(Uuid::now_v7());
        let (inside, spawned) = scope(other, async {
            (current(), spawn(async { current() }).await.unwrap())
        })
        .await;
        assert_eq!(inside, Some(other));
        assert_eq!(spawned, Some(other));
        assert_eq!(current(), None);
        assert_eq!(session_value(), "");
    }

    #[test]
    fn aust_id_matches_the_migration() {
        assert_eq!(AUST.0.to_string(), "0190aa57-0000-7000-8000-000000000001");
    }
}
