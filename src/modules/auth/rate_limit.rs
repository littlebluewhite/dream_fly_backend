//! Rate-limit counting primitives — the Redis INCR/EXPIRE/GET/DEL shapes
//! that `login`, `forgot_password`, and the OTP lifecycle each had inline:
//! a best-effort atomic bump for the failed-login counter, a plain
//! read-with-default and a fire-and-forget clear for that same counter, a
//! bump-and-return-count primitive shared byte-for-byte by the OTP
//! request-rate check and the OTP verify-attempt counter, and a best-effort
//! atomic bump-and-return-count for the forgot-password rate limit. Every
//! TTL/limit constant these flows use lives here too.
//!
//! No policy lives here — "what happens once the count is too high"
//! (lockout, rejection, silent swallow) stays in the calling flow
//! (`service.rs` for login/forgot, `otp.rs` for OTP). These functions only
//! bump, read, or clear a counter at a caller-supplied key; they never
//! decide whether a count is too high, and they never build the key
//! strings themselves — callers keep doing that, so the key formats stay
//! exactly where they were.

use redis::AsyncCommands;

use crate::utils::redis_counter::incr_with_ttl;

/// Max failed login attempts per email before temporary lockout.
pub(super) const LOGIN_MAX_ATTEMPTS: i64 = 10;
/// Lockout window after hitting the threshold (seconds).
pub(super) const LOGIN_LOCKOUT_TTL: i64 = 900; // 15 minutes

/// Maximum OTP requests a single authenticated user may trigger per hour.
pub(super) const OTP_REQUESTS_PER_HOUR: i64 = 3;
/// Maximum failed verification attempts before the OTP is invalidated.
pub(super) const OTP_MAX_ATTEMPTS: i64 = 5;
/// OTP lifetime in seconds.
pub(super) const OTP_TTL_SECONDS: i64 = 300;
/// OTP rate-limit window in seconds.
pub(super) const OTP_RATE_LIMIT_TTL: i64 = 3600;

/// Lifetime of a password-reset token (seconds). Must match the text in
/// `send_password_reset` email body.
pub(super) const PASSWORD_RESET_TTL_SECONDS: i64 = 900; // 15 minutes

/// Atomic INCR + EXPIRE for the failed-login counter. Best-effort: Redis
/// outages must not prevent authentication entirely.
pub(super) async fn bump_login_failure(redis: &mut redis::aio::ConnectionManager, key: &str) {
    let _ = incr_with_ttl(redis, key, LOGIN_LOCKOUT_TTL).await;
}

/// Read the current value of a counter key, defaulting to 0 if it is unset
/// or the read fails.
pub(super) async fn read_count(redis: &mut redis::aio::ConnectionManager, key: &str) -> i64 {
    redis.get(key).await.unwrap_or(0)
}

/// Fire-and-forget delete of a counter key. Errors are swallowed — clearing
/// a counter is a best-effort cleanup, not a correctness requirement.
pub(super) async fn clear_count(redis: &mut redis::aio::ConnectionManager, key: &str) {
    let _: Result<(), _> = redis.del::<_, ()>(key).await;
}

/// Atomic INCR + EXPIRE (same shape as `bump_login_failure`) that returns
/// the post-increment count instead of discarding it. Best-effort: on any
/// Redis error the count defaults to 0 rather than failing the caller.
pub(super) async fn bump_count_best_effort(
    redis: &mut redis::aio::ConnectionManager,
    key: &str,
    ttl_seconds: i64,
) -> i64 {
    incr_with_ttl(redis, key, ttl_seconds).await.unwrap_or(0)
}
