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
use aust_core::tenant::{self, TenantId, AUST};
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
        .route("/logo", put(upload_logo))
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

/// The editable company profile: names, contact, letterhead (address, bank, tax),
/// console colour, assistant persona. `has_logo` is read-only — the logo has its
/// own upload.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub(crate) struct ProfileDto {
    pub(crate) name: String,
    pub(crate) short_name: String,
    pub(crate) brand_name: String,
    #[serde(default)]
    pub(crate) owner_name: String,
    #[serde(default)]
    pub(crate) phone: String,
    #[serde(default)]
    pub(crate) street: String,
    #[serde(default)]
    pub(crate) postal_code: String,
    #[serde(default)]
    pub(crate) city: String,
    #[serde(default)]
    pub(crate) email: String,
    #[serde(default)]
    pub(crate) website: String,
    #[serde(default)]
    pub(crate) agb_url: String,
    #[serde(default)]
    pub(crate) review_url: String,
    #[serde(default)]
    pub(crate) bank_name: String,
    #[serde(default)]
    pub(crate) iban: String,
    #[serde(default)]
    pub(crate) bic: String,
    #[serde(default)]
    pub(crate) tax_number: String,
    #[serde(default)]
    pub(crate) vat_id: String,
    #[serde(default)]
    pub(crate) depot_address: String,
    #[serde(default = "default_accent")]
    pub(crate) accent_color: String,
    #[serde(default)]
    pub(crate) soul_md: Option<String>,
    #[serde(default)]
    pub(crate) has_logo: bool,
}

fn default_accent() -> String {
    "#ff5a1f".into()
}

const PROFILE_COLUMNS: &str = "name, short_name, brand_name, owner_name, phone, street, \
     postal_code, city, email, website, agb_url, review_url, bank_name, iban, bic, tax_number, \
     vat_id, depot_address, accent_color, soul_md, logo IS NOT NULL AS has_logo";

impl ProfileDto {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        if [&self.name, &self.short_name, &self.brand_name].iter().any(|s| s.trim().is_empty()) {
            return Err(ApiError::Validation("Firmenname, Kurzname und Marke dürfen nicht leer sein".into()));
        }
        let accent_ok = self.accent_color.len() == 7
            && self.accent_color.starts_with('#')
            && self.accent_color[1..].chars().all(|c| c.is_ascii_hexdigit());
        if !accent_ok {
            return Err(ApiError::Validation("Akzentfarbe als #rrggbb angeben".into()));
        }
        if let Some(md) = &self.soul_md {
            aust_assistant::soul::parse(md)
                .map_err(|e| ApiError::Validation(format!("Persona ungültig: {e}")))?;
        }
        Ok(())
    }
}

/// A company's profile (any company: the caller has already checked the right).
pub(crate) async fn load_profile(db: &sqlx::PgPool, tenant: TenantId) -> Result<ProfileDto, ApiError> {
    let mut tx = tenant::bypass(db).await?;
    sqlx::query_as(&format!("SELECT {PROFILE_COLUMNS} FROM tenants WHERE id = $1"))
        .bind(tenant)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::NotFound("Firma nicht gefunden".into()))
}

/// Replace a company's profile and re-derive its document templates.
pub(crate) async fn save_profile(
    db: &sqlx::PgPool,
    tenant: TenantId,
    p: &ProfileDto,
) -> Result<ProfileDto, ApiError> {
    p.validate()?;
    let mut tx = tenant::bypass(db).await?;
    let row: ProfileDto = sqlx::query_as(&format!(
        "UPDATE tenants SET name = $2, short_name = $3, brand_name = $4, owner_name = $5, \
             phone = $6, street = $7, postal_code = $8, city = $9, email = $10, website = $11, \
             agb_url = $12, review_url = $13, bank_name = $14, iban = $15, bic = $16, \
             tax_number = $17, vat_id = $18, depot_address = $19, accent_color = $20, soul_md = $21 \
         WHERE id = $1 RETURNING {PROFILE_COLUMNS}"
    ))
    .bind(tenant)
    .bind(p.name.trim())
    .bind(p.short_name.trim())
    .bind(p.brand_name.trim())
    .bind(p.owner_name.trim())
    .bind(p.phone.trim())
    .bind(p.street.trim())
    .bind(p.postal_code.trim())
    .bind(p.city.trim())
    .bind(p.email.trim())
    .bind(p.website.trim())
    .bind(p.agb_url.trim())
    .bind(p.review_url.trim())
    .bind(p.bank_name.trim())
    .bind(p.iban.trim())
    .bind(p.bic.trim())
    .bind(p.tax_number.trim())
    .bind(p.vat_id.trim())
    .bind(p.depot_address.trim())
    .bind(&p.accent_color)
    .bind(&p.soul_md)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::NotFound("Firma nicht gefunden".into()))?;
    tx.commit().await?;
    crate::services::letterhead::refresh(db, tenant).await?;
    Ok(row)
}

/// Store a company's logo and re-derive its document templates.
pub(crate) async fn save_logo(db: &sqlx::PgPool, tenant: TenantId, bytes: &[u8]) -> Result<(), ApiError> {
    crate::services::letterhead::validate_logo(bytes)?;
    let mut tx = tenant::bypass(db).await?;
    let changed = sqlx::query("UPDATE tenants SET logo = $2 WHERE id = $1")
        .bind(tenant)
        .bind(bytes)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if changed == 0 {
        return Err(ApiError::NotFound("Firma nicht gefunden".into()));
    }
    tx.commit().await?;
    crate::services::letterhead::refresh(db, tenant).await?;
    Ok(())
}

/// `GET /api/v1/admin/tenant`
async fn get_profile(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
) -> Result<Json<ProfileDto>, ApiError> {
    require_admin(&claims)?;
    Ok(Json(load_profile(&state.db, claims.tenant()).await?))
}

/// `PUT /api/v1/admin/tenant` — the caller's own company. Every mail, document and
/// the console read from it.
async fn update_profile(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Json(p): Json<ProfileDto>,
) -> Result<Json<ProfileDto>, ApiError> {
    require_admin(&claims)?;
    Ok(Json(save_profile(&state.db, claims.tenant(), &p).await?))
}

/// `PUT /api/v1/admin/tenant/logo` — body is the image (PNG, JPEG or WebP).
async fn upload_logo(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&claims)?;
    save_logo(&state.db, claims.tenant(), &body).await?;
    Ok(Json(serde_json::json!({ "bytes": body.len() })))
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
