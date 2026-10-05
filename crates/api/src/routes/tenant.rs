//! The company itself: console branding, profile, document templates.
//! See docs/MULTI_TENANT.md (step 6).

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Path, State},
    routing::{get, put},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};

use aust_core::models::TokenClaims;
use aust_core::tenant::{self, AUST};
use aust_offer_generator::templates::{self as doc_templates, TemplateKind};

use crate::routes::admin::require_admin;
use crate::{ApiError, AppState};

/// `GET /api/v1/tenant` — public.
pub fn public_router() -> Router<Arc<AppState>> {
    Router::new().route("/tenant", get(branding))
}

/// `/api/v1/admin/tenant` — the running company's own settings (admin only).
pub fn admin_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(get_profile).put(update_profile))
        .route("/templates/{kind}", put(upload_template))
}

/// What the console shows before and after login.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct Branding {
    name: String,
    mark: String,
    place: String,
    accent: String,
}

/// Initials for the console's mark: the first letter of the first two words.
fn mark(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

/// `GET /api/v1/tenant` — branding of the company whose domain the console is
/// served from (`Origin`, see `middleware::scope_by_origin`); Aust otherwise.
async fn branding(State(state): State<Arc<AppState>>) -> Result<Json<Branding>, ApiError> {
    let (short_name, city, accent): (String, String, String) = sqlx::query_as(
        "SELECT short_name, city, accent_color FROM tenants WHERE id = current_tenant_id()",
    )
    .fetch_one(&state.db)
    .await?;
    Ok(Json(Branding { mark: mark(&short_name), name: short_name, place: city, accent }))
}

/// The editable company profile.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub(crate) struct ProfileDto {
    name: String,
    short_name: String,
    brand_name: String,
    owner_name: String,
    phone: String,
    city: String,
    review_url: String,
    depot_address: String,
    accent_color: String,
    soul_md: Option<String>,
}

const PROFILE_COLUMNS: &str = "name, short_name, brand_name, owner_name, phone, city, \
     review_url, depot_address, accent_color, soul_md";

/// `GET /api/v1/admin/tenant`
async fn get_profile(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
) -> Result<Json<ProfileDto>, ApiError> {
    require_admin(&claims)?;
    let row = sqlx::query_as(&format!(
        "SELECT {PROFILE_COLUMNS} FROM tenants WHERE id = current_tenant_id()"
    ))
    .fetch_one(&state.db)
    .await?;
    Ok(Json(row))
}

/// `PUT /api/v1/admin/tenant` — replace the profile. Every mail, document and the
/// console read from it, so empty names are refused.
async fn update_profile(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Json(p): Json<ProfileDto>,
) -> Result<Json<ProfileDto>, ApiError> {
    require_admin(&claims)?;
    if [&p.name, &p.short_name, &p.brand_name].iter().any(|s| s.trim().is_empty()) {
        return Err(ApiError::Validation("Firmenname, Kurzname und Marke dürfen nicht leer sein".into()));
    }
    let accent_ok = p.accent_color.len() == 7
        && p.accent_color.starts_with('#')
        && p.accent_color[1..].chars().all(|c| c.is_ascii_hexdigit());
    if !accent_ok {
        return Err(ApiError::Validation("Akzentfarbe als #rrggbb angeben".into()));
    }
    if let Some(md) = &p.soul_md {
        aust_assistant::soul::parse(md)
            .map_err(|e| ApiError::Validation(format!("Persona ungültig: {e}")))?;
    }
    let row = sqlx::query_as(&format!(
        "UPDATE tenants SET name = $1, short_name = $2, brand_name = $3, owner_name = $4, \
             phone = $5, city = $6, review_url = $7, depot_address = $8, accent_color = $9, \
             soul_md = $10 \
         WHERE id = current_tenant_id() RETURNING {PROFILE_COLUMNS}"
    ))
    .bind(&p.name)
    .bind(&p.short_name)
    .bind(&p.brand_name)
    .bind(&p.owner_name)
    .bind(&p.phone)
    .bind(&p.city)
    .bind(&p.review_url)
    .bind(&p.depot_address)
    .bind(&p.accent_color)
    .bind(&p.soul_md)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(row))
}

/// `PUT /api/v1/admin/tenant/templates/{kind}` — the request body is the file
/// (XLSX, or the PDF page for `clearing_page_2`). Takes effect immediately.
/// Aust's templates are compiled in and are changed in the repository instead.
async fn upload_template(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(kind): Path<String>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&claims)?;
    let kind = TemplateKind::parse(&kind)
        .ok_or_else(|| ApiError::NotFound(format!("Unbekannte Vorlage: {kind}")))?;
    let tenant = claims.tenant();
    if tenant == AUST {
        return Err(ApiError::BadRequest(
            "Aust nutzt die eingebauten Vorlagen (templates/ im Repository).".into(),
        ));
    }
    let looks_right = match kind {
        TemplateKind::ClearingPage2 => body.starts_with(b"%PDF"),
        _ => body.starts_with(b"PK"),
    };
    if !looks_right {
        return Err(ApiError::Validation("Datei passt nicht zur Vorlage (XLSX bzw. PDF erwartet)".into()));
    }
    sqlx::query(
        "INSERT INTO tenant_templates (kind, content) VALUES ($1, $2)
         ON CONFLICT (tenant_id, kind) DO UPDATE SET content = EXCLUDED.content, updated_at = now()",
    )
    .bind(kind.as_str())
    .bind(body.as_ref())
    .execute(&state.db)
    .await?;
    doc_templates::register(tenant, kind, body.to_vec());
    debug_assert_eq!(tenant::current(), Some(tenant));
    Ok(Json(serde_json::json!({ "kind": kind.as_str(), "bytes": body.len() })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_are_the_initials_of_the_first_two_words() {
        assert_eq!(mark("Aust Umzüge"), "AU");
        assert_eq!(mark("zweite umzüge gmbh"), "ZU");
        assert_eq!(mark("Solo"), "S");
    }

    /// Aust's console gets exactly what `lib/tenant.svelte.ts` starts with, so
    /// nothing flashes when the branding arrives.
    #[sqlx::test(migrations = "../../migrations")]
    async fn aust_branding_matches_the_console_defaults(pool: sqlx::PgPool) {
        let state = Arc::new(crate::test_helpers::test_app_state_with_pool(pool).await);
        let Json(b) = branding(State(state)).await.unwrap();
        assert_eq!(
            b,
            Branding {
                name: "Aust Umzüge".into(),
                mark: "AU".into(),
                place: "Hildesheim".into(),
                accent: "#ff5a1f".into(),
            }
        );
    }
}
