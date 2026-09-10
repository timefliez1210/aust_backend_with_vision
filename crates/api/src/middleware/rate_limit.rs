use axum::{
    extract::{ConnectInfo, Request},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
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

/// Is this peer allowed to tell us who the real client is?
///
/// In production the only thing that ever connects to this process is Apache on the
/// same host, reached over loopback or the Docker bridge. Anything arriving from a
/// public address is talking to us directly and its headers are just user input.
fn is_trusted_proxy(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

/// The client IP as reported by a trusted proxy: the LAST entry in `X-Forwarded-For`.
///
/// Each proxy appends the address it received the connection from, so the rightmost
/// entry is the one our own proxy observed. Everything to its left was supplied by the
/// caller and can say anything at all.
fn forwarded_client_ip(request: &Request) -> Option<IpAddr> {
    request
        .headers()
        .get_all("X-Forwarded-For")
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|s| s.split(','))
        .filter_map(|ip| ip.trim().parse::<IpAddr>().ok())
        .next_back()
}

/// Extracts the real client IP from the request.
///
/// **Caller**: `apply_rate_limit()`.
/// **Why**: In production the backend sits behind Apache, so the socket address is
/// always the proxy and the real client is in `X-Forwarded-For`.
///
/// The header is only believed when the connection came from a trusted proxy, and then
/// only its rightmost entry. Trusting the leftmost value unconditionally, as this used
/// to, let the caller choose their own bucket: a new `X-Forwarded-For` per request and
/// the limit on `/auth/login`, `/customer/auth/*` and `/employee/auth/*` was gone.
///
/// Falls back to `0.0.0.0` when there is no peer address and no usable header, which
/// puts those requests in one shared bucket — the safe direction to fail.
fn extract_client_ip(request: &Request) -> IpAddr {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());

    match peer {
        Some(peer) if is_trusted_proxy(peer) => forwarded_client_ip(request).unwrap_or(peer),
        // A direct connection from the internet: the socket is the only honest source.
        Some(peer) => peer,
        None => forwarded_client_ip(request).unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)),
    }
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
        tracing::warn!(client_ip = %ip, "Rate limit exceeded on rate-limited endpoint");
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::ConnectInfo;

    fn req(peer: Option<&str>, xff: Option<&str>) -> Request {
        let mut r = Request::new(axum::body::Body::empty());
        if let Some(p) = peer {
            let addr: SocketAddr = p.parse().unwrap();
            r.extensions_mut().insert(ConnectInfo(addr));
        }
        if let Some(v) = xff {
            r.headers_mut().insert("X-Forwarded-For", v.parse().unwrap());
        }
        r
    }

    /// The header is user input. Believing its leftmost value let a caller pick their
    /// own bucket and walk straight past the limit on every auth endpoint.
    #[test]
    fn a_direct_caller_cannot_choose_their_own_bucket() {
        let ip = extract_client_ip(&req(Some("203.0.113.9:5555"), Some("1.2.3.4")));
        assert_eq!(ip.to_string(), "203.0.113.9", "the socket is the only honest source");
    }

    /// Behind the real proxy the client is the entry our own proxy appended — the last
    /// one. Anything to its left was written by the caller.
    #[test]
    fn behind_the_proxy_the_rightmost_entry_wins() {
        let ip = extract_client_ip(&req(Some("127.0.0.1:5555"), Some("1.2.3.4, 198.51.100.7")));
        assert_eq!(ip.to_string(), "198.51.100.7");
    }

    /// A spoofed chain from a trusted proxy still resolves to what the proxy saw.
    #[test]
    fn a_spoofed_chain_collapses_to_the_observed_client() {
        let a = extract_client_ip(&req(Some("127.0.0.1:5555"), Some("9.9.9.9, 198.51.100.7")));
        let b = extract_client_ip(&req(Some("127.0.0.1:5555"), Some("8.8.8.8, 198.51.100.7")));
        assert_eq!(a, b, "rotating the spoofed prefix must not change the bucket");
    }

    /// No header from the proxy at all: fall back to the proxy itself rather than to a
    /// single shared bucket.
    #[test]
    fn a_trusted_peer_without_a_header_is_used_as_is() {
        let ip = extract_client_ip(&req(Some("127.0.0.1:5555"), None));
        assert_eq!(ip.to_string(), "127.0.0.1");
    }
}
