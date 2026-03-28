use axum::{extract::Request, http::HeaderValue, middleware::Next, response::Response};
use tracing::Instrument;
use uuid::Uuid;

/// A request-scoped identifier injected into request extensions by `set_request_id`.
///
/// **Caller**: downstream handlers can extract this via `Extension<RequestId>` if needed.
/// **Why**: Correlating log lines across the async call chain (email → inquiry → offer)
///          requires a stable ID per request. Without it, concurrent requests interleave
///          log output with no way to group them.
#[derive(Clone)]
pub struct RequestId(pub String);

/// Generates a unique request ID, wraps the handler in a named tracing span, and echoes
/// the ID back on the response as `X-Request-ID`.
///
/// **Caller**: `lib.rs` — applied as a global layer inside the top-level router.
/// **Why**: The email → inquiry → estimation → offer pipeline spawns multiple async tasks.
///          A stable request ID in every tracing span makes it possible to reconstruct the
///          full causal chain from logs.
///
/// Behaviour:
/// - If the incoming request already carries `X-Request-ID` (set by a reverse proxy), that
///   value is reused so the ID propagates end-to-end from the client.
/// - Otherwise a UUID v7 (time-ordered) is generated.
/// - The ID is recorded as `request_id` on a `tracing::info_span` that wraps the inner
///   handler, so it appears in every log line emitted during that request.
/// - The ID is also written to the `X-Request-ID` response header so callers can correlate
///   client-side errors with server-side logs.
pub async fn set_request_id(mut request: Request, next: Next) -> Response {
    let request_id = request
        .headers()
        .get("X-Request-ID")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_owned())
        .unwrap_or_else(|| Uuid::now_v7().to_string());

    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let span = tracing::info_span!("request", request_id = %request_id);

    let mut response = async move { next.run(request).await }
        .instrument(span)
        .await;

    if let Ok(val) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert("X-Request-ID", val);
    }

    response
}
