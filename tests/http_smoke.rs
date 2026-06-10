//! Harness smoke test. Verifies that `spawn_test_app` can build the full
//! router, that the health check responds 200, and that a subsequent call
//! passes rate-limit middleware using the synthetic X-Forwarded-For header.

mod common;

use common::http::spawn_test_app;
use sqlx::PgPool;

#[sqlx::test]
async fn harness_health_check_responds_200(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/health").await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
}

#[sqlx::test]
async fn harness_missing_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    // `/users/me` requires AuthUser. Without a token we expect 401.
    let resp = app.get("/api/v1/users/me").await;
    assert_eq!(resp.status_code(), 401);
}
