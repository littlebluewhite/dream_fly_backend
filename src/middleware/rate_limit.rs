use std::net::IpAddr;

use axum::extract::{ConnectInfo, Request};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::state::AppState;

/// Build a JSON error response for rate-limit rejections. Keeps the four
/// call-sites in [`rate_limit_middleware`] DRY without pulling in `AppError`
/// (which lives one layer above middleware).
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

/// Atomic INCR + EXPIRE — increments the counter and, only if this is the
/// first request in the window, sets the TTL. A plain `INCR` followed by a
/// separate `EXPIRE` has a race: if `EXPIRE` fails (or the connection drops
/// between the two), the counter lives forever.
///
/// Returns the new counter value.
const INCR_EXPIRE_SCRIPT: &str = r#"
local current = redis.call('INCR', KEYS[1])
if current == 1 then
    redis.call('EXPIRE', KEYS[1], ARGV[1])
end
return current
"#;

async fn atomic_incr_with_ttl(
    redis: &mut redis::aio::ConnectionManager,
    key: &str,
    ttl_seconds: i64,
) -> Result<i64, redis::RedisError> {
    redis::Script::new(INCR_EXPIRE_SCRIPT)
        .key(key)
        .arg(ttl_seconds)
        .invoke_async::<i64>(redis)
        .await
}

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

/// Auth endpoints that deserve a stricter bucket. Anything matching one of
/// these prefixes pays both the global AND the auth quota.
fn is_auth_endpoint(path: &str) -> bool {
    // All of these take user credentials (or trigger an OTP/email/SMS that
    // costs us money) and therefore must be throttled hard.
    path.starts_with("/api/v1/auth/login")
        || path.starts_with("/api/v1/auth/register")
        || path.starts_with("/api/v1/auth/refresh")
        || path.starts_with("/api/v1/auth/password/forgot")
        || path.starts_with("/api/v1/auth/password/reset")
        || path.starts_with("/api/v1/auth/otp/send")
        || path.starts_with("/api/v1/auth/otp/verify")
        || path.starts_with("/api/v1/auth/google")
}

pub async fn rate_limit_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, Response> {
    let trust_proxy = state.config.server.trust_proxy;

    let identity = match extract_client_ip(&req, trust_proxy) {
        Some(ip) => ip.to_string(),
        None => {
            // No identity available — fall back to a single bucket so bursts
            // from unidentified sources still get throttled (even if all
            // collapse into one bucket).
            "anon".to_string()
        }
    };

    let path = req.uri().path().to_string();
    let mut redis_conn = state.redis.clone();

    // 1. Global per-IP bucket. Keyed by IP only (NOT by path), so an attacker
    //    cannot fan out across endpoints to circumvent the cap.
    let global_key = format!("rate_limit:global:{identity}");
    let global_count = atomic_incr_with_ttl(&mut redis_conn, &global_key, WINDOW_SECONDS)
        .await
        .map_err(|e| {
            tracing::error!("Redis global rate limit error: {e}");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
        })?;

    if global_count > GLOBAL_MAX_REQUESTS_PER_WINDOW {
        return Err(error_response(StatusCode::TOO_MANY_REQUESTS, "too many requests"));
    }

    // 2. Auth-specific bucket, only for auth endpoints. Layered, not replacing.
    if is_auth_endpoint(&path) {
        let auth_key = format!("rate_limit:auth:{identity}");
        let auth_count = atomic_incr_with_ttl(&mut redis_conn, &auth_key, AUTH_WINDOW_SECONDS)
            .await
            .map_err(|e| {
                tracing::error!("Redis auth rate limit error: {e}");
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
            })?;

        if auth_count > AUTH_MAX_REQUESTS_PER_WINDOW {
            return Err(error_response(StatusCode::TOO_MANY_REQUESTS, "too many requests"));
        }
    }

    Ok(next.run(req).await)
}
