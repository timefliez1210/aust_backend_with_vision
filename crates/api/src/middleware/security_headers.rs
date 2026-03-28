use axum::{extract::Request, http::HeaderValue, middleware::Next, response::Response};

/// Injects standard security headers on every outbound response.
///
/// **Caller**: `lib.rs` — applied as a global layer to the top-level router.
/// **Why**: Defence-in-depth for the API. Prevents MIME sniffing, clickjacking,
///          information leakage, and enforces HTTPS for return visitors.
///
/// Headers set:
/// - `X-Content-Type-Options: nosniff` — browser must honor declared Content-Type
/// - `X-Frame-Options: DENY` — disallow embedding in any frame
/// - `Referrer-Policy: strict-origin-when-cross-origin` — limit referrer leakage
/// - `X-Permitted-Cross-Domain-Policies: none` — block Flash/Silverlight cross-domain
/// - `Strict-Transport-Security` — force HTTPS for 1 year (production only)
/// - `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'`
pub async fn set_security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();

    headers.insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("X-Frame-Options", HeaderValue::from_static("DENY"));
    headers.insert(
        "Referrer-Policy",
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        "X-Permitted-Cross-Domain-Policies",
        HeaderValue::from_static("none"),
    );
    headers.insert(
        "Strict-Transport-Security",
        HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );
    headers.insert(
        "Content-Security-Policy",
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );

    response
}
