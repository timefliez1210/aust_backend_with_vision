//! Platform administration — companies (tenants) across the whole installation.
//! Only for platform superusers (`users.is_superuser`, set from the command line).
//! See docs/MULTI_TENANT.md.

use std::sync::Arc;

use axum::{extract::State, routing::get, Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use aust_core::models::TokenClaims;

use crate::services::onboarding;
use crate::{ApiError, AppState};

/// `/api/v1/platform` (behind the admin JWT middleware).
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/tenants", get(list_tenants).post(create_tenant))
}

/// Refuse unless the caller is a platform superuser *now*. The token's `su`
/// claim only drives the UI; a revoked flag takes effect on the next request.
async fn require_superuser(state: &AppState, claims: &TokenClaims) -> Result<(), ApiError> {
    let mut tx = aust_core::tenant::bypass(&state.db).await?;
    let su: Option<(bool,)> = sqlx::query_as("SELECT is_superuser FROM users WHERE id = $1")
        .bind(claims.sub)
        .fetch_optional(&mut *tx)
        .await?;
    match su {
        Some((true,)) => Ok(()),
        _ => Err(ApiError::Forbidden("Nur für Plattform-Administratoren".into())),
    }
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub(crate) struct TenantRow {
    id: Uuid,
    slug: String,
    name: String,
    domains: Vec<String>,
    created_at: DateTime<Utc>,
    users: i64,
    customers: i64,
    inquiries: i64,
}

/// `GET /api/v1/platform/tenants` — every company with a few counts.
async fn list_tenants(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
) -> Result<Json<Vec<TenantRow>>, ApiError> {
    require_superuser(&state, &claims).await?;
    let mut tx = aust_core::tenant::bypass(&state.db).await?;
    let rows = sqlx::query_as(
        "SELECT t.id, t.slug, t.name, t.domains, t.created_at,
                (SELECT count(*) FROM users u WHERE u.tenant_id = t.id) AS users,
                (SELECT count(*) FROM customers c WHERE c.tenant_id = t.id AND c.merged_into IS NULL) AS customers,
                (SELECT count(*) FROM inquiries i WHERE i.tenant_id = t.id) AS inquiries
         FROM tenants t
         ORDER BY t.created_at, t.id",
    )
    .fetch_all(&mut *tx)
    .await?;
    Ok(Json(rows))
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateTenant {
    slug: String,
    name: String,
    admin_email: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct CreatedTenant {
    id: Uuid,
    slug: String,
    admin_email: String,
    /// Shown once. The new company's admin changes it after the first login.
    admin_password: String,
}

/// `POST /api/v1/platform/tenants` — a new company and its first admin. The
/// admin can sign in right away; mailbox, bot and domains need configuration
/// and a restart (docs/MULTI_TENANT.md, "Onboarding a company").
async fn create_tenant(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Json(req): Json<CreateTenant>,
) -> Result<Json<CreatedTenant>, ApiError> {
    require_superuser(&state, &claims).await?;
    let slug = req.slug.trim().to_lowercase();
    let email = req.admin_email.trim().to_lowercase();
    let t = onboarding::create_tenant(&state.db, &slug, req.name.trim(), &email)
        .await
        .map_err(|e| {
            let constraint = match &e {
                ApiError::Database(sqlx::Error::Database(db)) => db.constraint().map(str::to_owned),
                _ => None,
            };
            match constraint.as_deref() {
                Some("tenants_slug_key") => ApiError::Conflict(format!("Kürzel „{slug}“ ist schon vergeben")),
                Some("users_email_key") => ApiError::Conflict(format!("{email} hat schon ein Konto")),
                _ => e,
            }
        })?;
    tracing::info!(tenant = %t.id.0, %slug, by = %claims.email, "Company created");
    Ok(Json(CreatedTenant {
        id: t.id.0,
        slug,
        admin_email: t.admin_email,
        admin_password: t.admin_password,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn admin(pool: &sqlx::PgPool, superuser: bool) -> TokenClaims {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO users (id, email, password_hash, name, role, is_superuser)
             VALUES ($1, $2, 'x', 'Test', 'admin', $3)",
        )
        .bind(id)
        .bind(format!("{id}@example.com"))
        .bind(superuser)
        .execute(pool)
        .await
        .unwrap();
        TokenClaims {
            sub: id,
            email: format!("{id}@example.com"),
            role: aust_core::models::UserRole::Admin,
            exp: usize::MAX,
            iat: 0,
            typ: aust_core::models::TokenType::Access,
            tid: None,
            // The claim alone must not open the door — the database decides.
            su: true,
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn only_a_superuser_sees_and_creates_companies(pool: sqlx::PgPool) {
        let state = Arc::new(crate::test_helpers::test_app_state_with_pool(pool.clone()).await);

        let plain = admin(&pool, false).await;
        let denied = list_tenants(State(state.clone()), Extension(plain)).await;
        assert!(matches!(denied, Err(ApiError::Forbidden(_))), "an su claim without the flag is refused");

        let su = admin(&pool, true).await;
        let Json(created) = create_tenant(
            State(state.clone()),
            Extension(su.clone()),
            Json(CreateTenant { slug: "Zweite".into(), name: "Zweite Umzüge".into(), admin_email: "Chefin@Zweite.de".into() }),
        )
        .await
        .unwrap();
        assert_eq!(created.slug, "zweite");
        assert_eq!(created.admin_email, "chefin@zweite.de");

        let Json(list) = list_tenants(State(state.clone()), Extension(su.clone())).await.unwrap();
        assert_eq!(list.iter().map(|t| t.slug.as_str()).collect::<Vec<_>>(), ["aust", "zweite"]);
        assert_eq!(list[1].users, 1);

        let again = create_tenant(
            State(state),
            Extension(su),
            Json(CreateTenant { slug: "zweite".into(), name: "x".into(), admin_email: "y@zweite.de".into() }),
        )
        .await;
        assert!(matches!(again, Err(ApiError::Conflict(_))));
    }
}
