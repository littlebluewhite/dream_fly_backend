//! Atomic "INCR, then EXPIRE only on the first increment" counter bump —
//! one Lua script, shared by the global/per-IP HTTP rate limiter
//! (`middleware::rate_limit`) and the auth-module counters
//! (`modules::auth::rate_limit`: failed-login lockout and the
//! forgot-password best-effort bump).
//!
//! This lives in `utils` rather than in either call site's own module
//! because `middleware` must not depend on `auth` (middleware sits in
//! front of every route; auth is just one more module behind it) and
//! `auth` should not depend on `middleware` (auth is domain logic, not
//! HTTP wiring) — `utils` is the common downstream both already import
//! from, so it is the only place that does not create a cross-layer
//! dependency.
//!
//! A plain `INCR` followed by a separate `EXPIRE` races: if the process
//! crashes or the connection drops between the two calls, the counter is
//! left with no TTL and grows forever. Running both in one Lua script
//! makes "increment, and set the TTL only if this is the increment that
//! just created the key" atomic.

use redis::aio::ConnectionManager;

const INCR_EXPIRE_SCRIPT: &str = r#"
local current = redis.call('INCR', KEYS[1])
if current == 1 then
    redis.call('EXPIRE', KEYS[1], ARGV[1])
end
return current
"#;

/// INCR the counter at `key`, setting a `ttl_seconds` expiry only the first
/// time it is created (the increment that makes the count 1), and return
/// the post-increment count.
pub async fn incr_with_ttl(
    redis: &mut ConnectionManager,
    key: &str,
    ttl_seconds: i64,
) -> Result<i64, redis::RedisError> {
    redis::Script::new(INCR_EXPIRE_SCRIPT)
        .key(key)
        .arg(ttl_seconds)
        .invoke_async::<i64>(redis)
        .await
}
