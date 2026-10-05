//! Which company an unauthenticated request is for (docs/MULTI_TENANT.md, step 6).
//!
//! A form on a company's website posts with that site as `Origin` (or, failing
//! that, `Referer`). If the host is one of a tenant's `domains`, the request runs
//! in that tenant's scope; otherwise it stays unscoped, which the database treats
//! as Aust — exactly as before. Authenticated routes set their own scope inside
//! this one (the token or session decides), so this only matters for public ones.

use axum::{extract::Request, http::header, middleware::Next, response::Response};

pub async fn scope_by_origin(request: Request, next: Next) -> Response {
    let tenant = [header::ORIGIN, header::REFERER]
        .into_iter()
        .filter_map(|h| request.headers().get(h)?.to_str().ok())
        .find_map(aust_core::tenant::by_origin);
    match tenant {
        Some(t) => aust_core::tenant::scope(t, next.run(request)).await,
        None => next.run(request).await,
    }
}
