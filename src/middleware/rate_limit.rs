use std::net::IpAddr;

use axum::extract::{ConnectInfo, Request};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::state::AppState;
use crate::utils::redis_counter::incr_with_ttl;

/// Build a JSON error response for rate-limit rejections. Keeps the
/// call-sites in [`rate_limit_middleware`] and [`strict_rate_limit`] DRY
/// without pulling in `AppError` (which lives one layer above middleware).
fn error_response(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

/// Global per-IP sliding-window length in seconds.
const WINDOW_SECONDS: i64 = 60;
/// Maximum requests per IP per window across ALL endpoints.
///
/// This is deliberately high — it is a DDoS cliff, not a fine-grained throttle.
/// Endpoint-specific quotas (login, password reset, OTP) live next to the
/// endpoint that needs them (see `auth::service` and this file's
/// [`AUTH_*`] constants).
const GLOBAL_MAX_REQUESTS_PER_WINDOW: i64 = 300;

/// Per-IP auth-endpoint bucket: much stricter than the global bucket because
/// anyone hitting `/auth/login` at 60 rpm is almost certainly doing credential
/// stuffing. This bucket is layered ON TOP of the global one.
const AUTH_WINDOW_SECONDS: i64 = 60;
const AUTH_MAX_REQUESTS_PER_WINDOW: i64 = 10;

/// Resolve the client's IP address, preferring the TCP peer address when
/// available. `X-Forwarded-For` is only honored when the server opts in via
/// `APP__SERVER__TRUST_PROXY=true`, because an untrusted XFF header can be
/// trivially spoofed to either bypass or amplify rate limits.
fn extract_client_ip(req: &Request, trust_proxy: bool) -> Option<IpAddr> {
    if trust_proxy {
        if let Some(hdr) = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
        {
            if let Some(first) = hdr.split(',').next() {
                if let Ok(ip) = first.trim().parse::<IpAddr>() {
                    return Some(ip);
                }
            }
        }
    }

    req.extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip())
}

/// Resolve the per-request rate-limit identity: the client IP when
/// available, else a shared "anon" bucket so unidentified sources still get
/// throttled. Shared by [`rate_limit_middleware`] and [`strict_rate_limit`]
/// so both buckets key off the exact same identity for a given request.
fn client_identity(req: &Request, trust_proxy: bool) -> String {
    match extract_client_ip(req, trust_proxy) {
        Some(ip) => ip.to_string(),
        None => {
            // No identity available — fall back to a single bucket so bursts
            // from unidentified sources still get throttled (even if all
            // collapse into one bucket).
            "anon".to_string()
        }
    }
}

/// Global per-IP bucket only (`rate_limit:global:{ip}`, 300/min) — mounted
/// as the sole outer rate-limit layer in `startup.rs`. The stricter
/// per-route auth bucket used to be checked here too, behind
/// `is_auth_endpoint` prefix-sniffing; it now lives in
/// [`strict_rate_limit`], mounted separately via `route_layer` on the auth
/// throttled route group.
pub async fn rate_limit_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, Response> {
    let trust_proxy = state.config.server.trust_proxy;
    let identity = client_identity(&req, trust_proxy);
    let mut redis_conn = state.redis.clone();

    // Global per-IP bucket. Keyed by IP only (NOT by path), so an attacker
    // cannot fan out across endpoints to circumvent the cap.
    let global_key = format!("rate_limit:global:{identity}");
    let global_count = incr_with_ttl(&mut redis_conn, &global_key, WINDOW_SECONDS)
        .await
        .map_err(|e| {
            tracing::error!("Redis global rate limit error: {e}");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
        })?;

    if global_count > GLOBAL_MAX_REQUESTS_PER_WINDOW {
        return Err(error_response(StatusCode::TOO_MANY_REQUESTS, "too many requests"));
    }

    Ok(next.run(req).await)
}

/// Route-layer gate for the auth throttled route group
/// (`auth::routes::throttled_router`, mounted in `startup.rs` the same
/// shape as `admin_api`/`staff_api`). Adds the per-IP auth bucket
/// (`rate_limit:auth:{ip}`, 10/min) ON TOP of [`rate_limit_middleware`]'s
/// global bucket — this middleware is never mounted on its own, only via
/// `route_layer` nested inside the outer global layer.
///
/// Replaces the old `is_auth_endpoint` prefix-sniffing that used to run
/// inline in the combined middleware. Equivalent for legitimate traffic —
/// the 8 routes in `throttled_router` map 1:1 to the old prefix list, none
/// of them have sub-paths — except two accepted edge deltas (neither is
/// legitimate traffic, so these are recorded rather than "fixed"):
///
/// (a) `route_layer` sits inside the body-limit layer (route_layer is
///     innermost; body limit is an outer `.layer()` in `startup.rs`). A
///     >2MB auth request used to charge both buckets before the body-limit
///     413; now it only charges the global bucket, because 413 fires before
///     this middleware ever runs — a request that would have seen 429
///     (bucket already full) now sees 413 instead.
/// (b) `route_layer` only runs on a route match. A 404 on an auth-prefixed
///     path that isn't exactly one of the 8 routes below (e.g.
///     `/api/v1/auth/loginXYZ`) used to charge this bucket via
///     `starts_with`; it now falls through to the 404 handler without
///     charging it (the global bucket still charges).
pub async fn strict_rate_limit(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, Response> {
    let trust_proxy = state.config.server.trust_proxy;
    let identity = client_identity(&req, trust_proxy);
    let mut redis_conn = state.redis.clone();

    // Auth-specific bucket. Layered on top of the global one (checked
    // separately, upstream, by `rate_limit_middleware`), not replacing it.
    let auth_key = format!("rate_limit:auth:{identity}");
    let auth_count = incr_with_ttl(&mut redis_conn, &auth_key, AUTH_WINDOW_SECONDS)
        .await
        .map_err(|e| {
            tracing::error!("Redis auth rate limit error: {e}");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
        })?;

    if auth_count > AUTH_MAX_REQUESTS_PER_WINDOW {
        return Err(error_response(StatusCode::TOO_MANY_REQUESTS, "too many requests"));
    }

    Ok(next.run(req).await)
}
