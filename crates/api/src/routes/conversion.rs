//! Conversion tab — mounted at `/api/v1/admin/conversion` (admin JWT layer, admin role only).
//!
//! One read endpoint; attribution and the funnel live in
//! [`crate::services::conversion_service`].

use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{Query, State},
    routing::get,
};
use chrono::NaiveDate;
use serde::Deserialize;

use aust_core::models::TokenClaims;

use crate::routes::admin::require_admin;
use crate::services::conversion_service::{self, ConversionReport};
use crate::{ApiError, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/", get(report))
}

#[derive(Debug, Deserialize)]
struct RangeQuery {
    /// First lead date (Berlin), inclusive. Omitted = from the beginning.
    from: Option<NaiveDate>,
    /// Last lead date (Berlin), inclusive. Omitted = until today.
    to: Option<NaiveDate>,
}

/// `GET /api/v1/admin/conversion?from=YYYY-MM-DD&to=YYYY-MM-DD`
///
/// **Caller**: admin dashboard, Conversion tab.
/// **Why**: shows per channel (Telefon, Kontaktformular, Kostenloses Angebot, Rückruf, …)
/// how many people got in touch, how far they got and why the rest dropped off.
///
/// # Errors
/// - 403 for non-admin users
/// - 400 when `from` is after `to`
async fn report(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<ConversionReport>, ApiError> {
    require_admin(&claims)?;
    if let (Some(f), Some(t)) = (q.from, q.to)
        && f > t
    {
        return Err(ApiError::BadRequest(
            "Startdatum liegt nach dem Enddatum".into(),
        ));
    }
    Ok(Json(
        conversion_service::report(&state.db, q.from, q.to).await?,
    ))
}
