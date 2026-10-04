//! The running tenant's profile (`tenants`). See `docs/MULTI_TENANT.md`.

use aust_core::tenant::TenantProfile;
use sqlx::PgPool;

/// The profile of the tenant this connection works for (`current_tenant_id()`).
///
/// **Caller**: anything that writes a customer mail, subject or document.
/// **Why**: names, phone and links are per company; they used to be string literals.
pub(crate) async fn profile(pool: &PgPool) -> Result<TenantProfile, sqlx::Error> {
    aust_core::tenant::profile(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Aust's seeded profile is exactly the text the code used before it moved into
    /// data — every mail and document stays byte-identical.
    #[sqlx::test(migrations = "../../migrations")]
    async fn aust_profile_matches_the_old_literals(pool: PgPool) {
        assert_eq!(profile(&pool).await.unwrap(), crate::test_helpers::aust_profile());
    }
}
