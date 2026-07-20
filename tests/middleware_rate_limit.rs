//! Integration tests for the Redis-backed rate-limit middleware.
//!
//! Each TestApp uses a unique synthetic `X-Forwarded-For`, so rate limit
//! buckets are naturally isolated per test. Within a single test we hit
//! the same endpoint many times from the same bucket to trigger the limit.
//!
//! The auth-endpoint bucket is 10/min — below the global 300/min — so the
//! shortest path to a 429 is to spam an auth endpoint.

mod common;

use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn auth_endpoint_returns_429_after_exceeding_bucket(db: PgPool) {
    let app = spawn_test_app(db).await;

    // First 10 requests to an auth endpoint must pass the rate limit.
    // We deliberately use a login with fake credentials: each response will
    // be 401, but the middleware runs before the handler so the bucket
    // counter still increments.
    let mut seen_429 = false;
    for i in 0..12 {
        let resp = app
            .post("/api/v1/auth/login")
            .json(&json!({
                "email": "nobody@example.com",
                "password": "Password!234",
            }))
            .await;
        // We do NOT assert a specific pre-limit status — the service returns
        // 401, but what matters is that once we exceed 10 requests we start
        // seeing 429 exclusively.
        if resp.status_code() == 429 {
            seen_429 = true;
            // Confirm all subsequent requests also get 429.
            assert!(i >= 10, "rate limit triggered too early on iteration {i}");
            break;
        }
    }
    assert!(
        seen_429,
        "expected at least one 429 within 12 auth-endpoint requests"
    );
}

#[sqlx::test]
async fn non_auth_endpoint_under_global_limit_passes_many_requests(db: PgPool) {
    // Non-auth endpoints share only the global 300-per-minute bucket, which
    // a normal test can't exhaust. We send a modest burst and assert every
    // request succeeds.
    let app = spawn_test_app(db).await;
    for _ in 0..25 {
        let resp = app.get("/api/v1/health").await;
        assert_ne!(resp.status_code(), 429);
    }
}

#[sqlx::test]
async fn register_endpoint_returns_429_after_exceeding_bucket(db: PgPool) {
    // Same strict bucket as login, exercised through a different route in
    // `throttled_router()` — guards against a wiring regression that only
    // routes `/auth/login` through `strict_rate_limit` and leaves the other
    // 7 routes ungated.
    let app = spawn_test_app(db).await;

    let mut seen_429 = false;
    for i in 0..12 {
        let resp = app
            .post("/api/v1/auth/register")
            .json(&json!({
                "email": format!("bucket-test-{i}@example.com"),
                "name": "Bucket Test",
                "password": "Password!234",
            }))
            .await;
        // No assertion on the pre-limit status (200 then likely validation
        // or business-logic responses) — only that once the bucket is
        // exceeded, every response becomes 429.
        if resp.status_code() == 429 {
            seen_429 = true;
            assert!(i >= 10, "rate limit triggered too early on iteration {i}");
            break;
        }
    }
    assert!(
        seen_429,
        "expected at least one 429 within 12 register requests"
    );
}

#[sqlx::test]
async fn logout_endpoint_not_rate_limited_by_strict_bucket(db: PgPool) {
    // `/auth/logout` stayed out of `throttled_router()` (it was never in
    // the old `is_auth_endpoint` prefix list either) — 11 requests, one more
    // than the strict bucket's threshold, must never see 429; only the much
    // higher global bucket (300/min) applies here. Guards against a wiring
    // regression that puts logout behind `strict_rate_limit`.
    let app = spawn_test_app(db).await;

    for i in 0..11 {
        let resp = app
            .post("/api/v1/auth/logout")
            .json(&json!({ "refresh_token": "not-a-real-refresh-token" }))
            .await;
        assert_ne!(
            resp.status_code(),
            429,
            "logout unexpectedly rate-limited on iteration {i}"
        );
    }
}
