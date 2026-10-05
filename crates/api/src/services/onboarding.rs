//! Onboarding a new company (docs/MULTI_TENANT.md, step 6).

use aust_core::tenant::TenantId;
use rand::Rng;
use sqlx::PgPool;
use uuid::Uuid;

use crate::ApiError;

/// A freshly created company and how its first administrator signs in.
#[derive(Debug)]
pub struct NewTenant {
    pub id: TenantId,
    pub admin_email: String,
    /// One-time password; shown once, change it after the first login.
    pub admin_password: String,
}

/// Create a company (`tenants` row) and its first administrator.
///
/// **Caller**: `aust_backend tenant-create` (src/main.rs).
/// **Why**: The tenant's own rows are created inside its scope, so they belong to
/// it. Its profile starts with `name` everywhere; mailbox and bot come from
/// `[tenants.<slug>]` in the config, templates are uploaded via
/// `PUT /api/v1/admin/tenant/templates/{kind}`. Restart the backend afterwards.
pub async fn create_tenant(
    db: &PgPool,
    slug: &str,
    name: &str,
    admin_email: &str,
) -> Result<NewTenant, ApiError> {
    let valid_slug = !slug.is_empty()
        && slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid_slug {
        return Err(ApiError::Validation("Slug: nur a–z, 0–9 und -".into()));
    }
    if name.trim().is_empty() || !admin_email.contains('@') {
        return Err(ApiError::Validation("Name und Admin-E-Mail sind Pflicht".into()));
    }

    // Password hashing first: nothing is written if it fails.
    let password: String = {
        const CHARS: &[u8] = b"abcdefghjkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let mut rng = rand::rng();
        (0..16).map(|_| CHARS[rng.random_range(0..CHARS.len())] as char).collect()
    };
    let hash = crate::routes::auth::hash_password(&password)?;

    // One transaction for company and admin (no company without its admin), across
    // tenants on purpose: the new company exists for no scope yet.
    let mut tx = aust_core::tenant::bypass(db).await?;
    let id = TenantId(Uuid::now_v7());
    sqlx::query(
        "INSERT INTO tenants (id, slug, name, short_name, brand_name, accent_color, domains)
         VALUES ($1, $2, $3, $3, $3, '#ff5a1f', '{}')",
    )
    .bind(id)
    .bind(slug)
    .bind(name.trim())
    .execute(&mut *tx)
    .await?;

    // The tenant is named explicitly — without it the admin would default to the
    // connection's tenant, i.e. Aust.
    sqlx::query(
        "INSERT INTO users (id, email, password_hash, name, role, tenant_id, created_at, updated_at)
         VALUES ($1, $2, $3, 'Administrator', 'admin', $4, now(), now())",
    )
    .bind(Uuid::now_v7())
    .bind(admin_email)
    .bind(&hash)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(NewTenant { id, admin_email: admin_email.to_string(), admin_password: password })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The new company and its admin exist, the admin belongs to the new company
    /// (not Aust), and the profile starts with the company's name.
    #[sqlx::test(migrations = "../../migrations")]
    async fn creates_the_company_and_its_admin(
        opts: sqlx::postgres::PgPoolOptions,
        conn: sqlx::postgres::PgConnectOptions,
    ) {
        let pool = crate::tenant_aware(opts).connect_with(conn).await.unwrap();
        let t = create_tenant(&pool, "zweite", "Zweite Umzüge", "chefin@zweite.de").await.unwrap();
        assert_eq!(t.admin_password.len(), 16);
        let (tenant_id, role): (Uuid, String) =
            sqlx::query_as("SELECT tenant_id, role FROM users WHERE email = 'chefin@zweite.de'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(tenant_id, t.id.0);
        assert_eq!(role, "admin");
        let profile = aust_core::tenant::scope(t.id, aust_core::tenant::profile(&pool)).await.unwrap();
        assert_eq!(profile.brand_name, "Zweite Umzüge");

        assert!(create_tenant(&pool, "Zweite!", "x", "a@b.de").await.is_err(), "bad slug");
    }
}
