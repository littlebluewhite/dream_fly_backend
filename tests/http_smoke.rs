//! Harness smoke test. Verifies that `spawn_test_app` can build the full
//! router and that a call passes rate-limit middleware using the synthetic
//! X-Forwarded-For header. The `/health` body lives in `tests/http_health.rs`.

mod common;

use common::http::spawn_test_app;
use sqlx::PgPool;

#[sqlx::test]
async fn harness_missing_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    // `/users/me` requires AuthUser. Without a token we expect 401.
    let resp = app.get("/api/v1/users/me").await;
    assert_eq!(resp.status_code(), 401);
}
