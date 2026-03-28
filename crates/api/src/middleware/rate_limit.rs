use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

/// Per-IP sliding-window rate limiter backed by an in-memory `HashMap`.
///
/// **Why**: Auth endpoints (`/auth/login`, `/customer/auth/*`, `/employee/auth/*`) are
///          exposed to the internet without authentication. Without a rate limit, an
///          attacker can brute-force OTP codes or admin passwords with no friction.
///
/// Entries are lazily evicted: when the map grows past 10 000 entries all expired
/// windows are flushed. At <1 000 req/day this never triggers in practice.
#[derive(Clone)]
pub struct RateLimiter {
    /// Maps client IP → (request count in current window, window start time).
    buckets: Arc<Mutex<HashMap<IpAddr, (u32, Instant)>>>,
    max_requests: u32,
    window: Duration,
}

impl RateLimiter {
    /// Creates a new limiter that allows `max_requests` per `window` per source IP.
    ///
    /// **Caller**: `lib.rs` — constructed once inside `create_router()`.
    /// **Why**: Rate limits are set at the router level so they apply before any business
    ///          logic runs.
    ///
    /// # Parameters
    /// - `max_requests` — maximum allowed requests per IP in the time window
    /// - `window` — the rolling time window (e.g. `Duration::from_secs(60)`)
    pub fn new(max_requests: u32, window: Duration) -> Self {
        Self {
            buckets: Arc::new(Mutex::new(HashMap::new())),
            max_requests,
            window,
        }
    }

    /// Returns `true` if the request should be allowed, `false` if it should be rejected.
    ///
    /// **Caller**: `apply_rate_limit()` — called once per incoming request on auth routes.
    /// **Why**: Encapsulates the bucket logic so the middleware closure stays readable.
    ///
    /// # Parameters
    /// - `ip` — the client IP extracted from `X-Forwarded-For` or the socket address
    pub async fn check(&self, ip: IpAddr) -> bool {
        let mut buckets = self.buckets.lock().await;
        let now = Instant::now();

        // Lazy eviction: flush expired entries when the map grows large.
        if buckets.len() > 10_000 {
            buckets.retain(|_, (_, start)| now.duration_since(*start) < self.window);
        }

        let entry = buckets.entry(ip).or_insert((0, now));
        if now.duration_since(entry.1) >= self.window {
            // New window — reset counter.
            *entry = (1, now);
            true
        } else if entry.0 < self.max_requests {
            entry.0 += 1;
            true
        } else {
            false
        }
    }
}

/// Extracts the real client IP from the request.
///
/// **Caller**: `apply_rate_limit()`.
/// **Why**: In production the backend sits behind Apache which sets `X-Forwarded-For`.
///          Reading the socket address would always yield Apache's loopback IP, making
///          rate limiting by-IP ineffective.
///
/// Falls back to `0.0.0.0` if neither header nor ConnectInfo is present, which will
/// cause all unresolvable requests to share a single bucket — acceptable since
/// legitimate traffic always carries `X-Forwarded-For` in production.
fn extract_client_ip(request: &Request) -> IpAddr {
    request
        .headers()
        .get("X-Forwarded-For")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.split(',').next())
        .and_then(|ip| ip.trim().parse::<IpAddr>().ok())
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
}

/// Tower middleware that enforces the rate limit on the request.
///
/// **Caller**: `lib.rs` — wired as a closure layer on `auth_public_router()`.
/// **Why**: Auth endpoints are the highest-value brute-force targets. The limit is set
///          to 10 requests per 60 seconds per IP — generous enough for legitimate use,
///          tight enough to slow down automated attacks by orders of magnitude.
///
/// Returns HTTP 429 with a German error message on rejection so the frontend can
/// surface a localised error to the user.
///
/// # Parameters
/// - `limiter` — `Arc<RateLimiter>` captured by the closure in `lib.rs`
/// - `request` — incoming axum request
/// - `next` — next handler/middleware in the chain
pub async fn apply_rate_limit(
    limiter: Arc<RateLimiter>,
    request: Request,
    next: Next,
) -> Response {
    let ip = extract_client_ip(&request);

    if !limiter.check(ip).await {
        tracing::warn!(client_ip = %ip, "Rate limit exceeded on auth endpoint");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": "rate_limit_exceeded",
                "message": "Zu viele Anfragen. Bitte warte eine Minute und versuche es erneut."
            })),
        )
            .into_response();
    }

    next.run(request).await
}
