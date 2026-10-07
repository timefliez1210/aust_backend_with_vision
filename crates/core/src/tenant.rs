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
    /// Depot: start and end of every route (Fahrkostenpauschale, admin map).
    pub depot_address: String,
}

/// The profile of the tenant this pool connection works for (`current_tenant_id()`).
pub async fn profile(pool: &sqlx::PgPool) -> Result<TenantProfile, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, name, short_name, brand_name, owner_name, phone, city, review_url, depot_address \
         FROM tenants WHERE id = current_tenant_id()",
    )
    .fetch_one(pool)
    .await
}

tokio::task_local! {
    static CURRENT: TenantId;
}

static SLUGS: std::sync::OnceLock<std::collections::HashMap<TenantId, String>> =
    std::sync::OnceLock::new();

/// Remember every tenant's slug (`tenants.slug`), so config lookups keyed by slug
/// (`[tenants.<slug>]`) can follow the running tenant. Called once at startup.
pub fn register_slugs(slugs: std::collections::HashMap<TenantId, String>) {
    let _ = SLUGS.set(slugs);
}

/// The running tenant's slug, or `None` for Aust and outside any scope (both use
/// the top-level config). An unknown tenant yields `""`, which matches no section.
pub fn current_slug() -> Option<&'static str> {
    let t = current()?;
    if t == AUST {
        return None;
    }
    Some(SLUGS.get().and_then(|m| m.get(&t)).map(String::as_str).unwrap_or(""))
}

static DOMAINS: std::sync::OnceLock<std::collections::HashMap<String, TenantId>> =
    std::sync::OnceLock::new();

/// Remember which host belongs to which tenant (`tenants.domains`). Called once at
/// startup; hosts are compared lowercase and without port.
pub fn register_domains(domains: std::collections::HashMap<String, TenantId>) {
    let _ = DOMAINS.set(domains.into_iter().map(|(d, t)| (d.to_lowercase(), t)).collect());
}

/// The tenant a host (`www.example.de`, optionally with `:port`) belongs to.
pub fn by_host(host: &str) -> Option<TenantId> {
    let host = host.rsplit_once(':').map_or(host, |(h, port)| {
        if port.chars().all(|c| c.is_ascii_digit()) { h } else { host }
    });
    DOMAINS.get()?.get(&host.to_lowercase()).copied()
}

/// The tenant an `Origin` or `Referer` value (`https://host[:port][/…]`) belongs to.
pub fn by_origin(origin: &str) -> Option<TenantId> {
    let rest = origin.split_once("://").map_or(origin, |(_, r)| r);
    by_host(rest.split('/').next().unwrap_or_default())
}

/// Every tenant's domains, read across tenants (startup).
pub async fn all_domains(pool: &sqlx::PgPool) -> Result<Vec<(String, TenantId)>, sqlx::Error> {
    let mut tx = bypass(pool).await?;
    sqlx::query_as("SELECT unnest(domains), id FROM tenants")
        .fetch_all(&mut *tx)
        .await
}

/// Whether Postgres enforces row-level security for this pool's role — i.e. the
/// role is neither superuser nor BYPASSRLS. Only then may a second company exist:
/// otherwise every unfiltered query would see all companies' rows.
pub async fn rls_enforced(pool: &sqlx::PgPool) -> Result<bool, sqlx::Error> {
    let (bypasses,): (bool,) =
        sqlx::query_as("SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = current_user")
            .fetch_one(pool)
            .await?;
    Ok(!bypasses)
}

/// Every tenant's id and slug, read across tenants (startup, per-tenant jobs).
pub async fn all(pool: &sqlx::PgPool) -> Result<Vec<(TenantId, String)>, sqlx::Error> {
    let mut tx = bypass(pool).await?;
    sqlx::query_as("SELECT id, slug FROM tenants ORDER BY created_at, id")
        .fetch_all(&mut *tx)
        .await
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

/// A transaction that sees every tenant's rows (row-level security bypass).
///
/// Only for lookups that must run before the tenant is known — login by email,
/// session tokens — and for jobs that deliberately span tenants. Read what you
/// need, then continue inside [`scope`] of the tenant you found. The bypass ends
/// with the transaction.
pub async fn bypass(
    pool: &sqlx::PgPool,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('app.tenant_bypass', 'on', true)")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
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
    fn hosts_and_origins_resolve_to_their_tenant() {
        let other = TenantId(Uuid::from_u128(7));
        register_domains(
            [("www.aust-umzuege.de".to_string(), AUST), ("Zweite.de".to_string(), other)].into(),
        );
        assert_eq!(by_origin("https://www.aust-umzuege.de"), Some(AUST));
        assert_eq!(by_origin("https://zweite.de:443/kontakt?x=1"), Some(other));
        assert_eq!(by_host("ZWEITE.DE"), Some(other));
        assert_eq!(by_origin("https://evil.example"), None);
        assert_eq!(by_origin("capacitor://localhost"), None);
    }

    #[test]
    fn aust_id_matches_the_migration() {
        assert_eq!(AUST.0.to_string(), "0190aa57-0000-7000-8000-000000000001");
    }
}
