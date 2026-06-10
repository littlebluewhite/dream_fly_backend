//! Integration tests for the security-header and body-limit middleware layers
//! wired in `src/startup.rs`. Exercises the response header set-layer and
//! the 2MB `RequestBodyLimitLayer` without going near any handler logic.

mod common;

use common::http::spawn_test_app;
use sqlx::PgPool;

#[sqlx::test]
async fn health_response_contains_security_headers(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/health").await;
    assert_eq!(resp.status_code(), 200);

    let headers = resp.headers();
    assert_eq!(
        headers
            .get("x-content-type-options")
            .map(|v| v.to_str().unwrap()),
        Some("nosniff")
    );
    assert_eq!(
        headers
            .get("x-frame-options")
            .map(|v| v.to_str().unwrap()),
        Some("DENY")
    );
    assert_eq!(
        headers
            .get("referrer-policy")
            .map(|v| v.to_str().unwrap()),
        Some("no-referrer")
    );
}

#[sqlx::test]
async fn body_larger_than_2mb_is_rejected(db: PgPool) {
    let app = spawn_test_app(db).await;

    // 3 MB of padding — comfortably over the 2 MB body limit configured in
    // `startup::build_router`. Depending on whether the layer trips via
    // Content-Length (early 413) or via a reader overflow inside
    // `ValidatedJson` (where it surfaces as `AppError::Validation` → 422),
    // we accept either rejection. The handler body MUST NOT execute.
    let big = "x".repeat(3 * 1024 * 1024);
    let payload = format!(
        r#"{{"email":"a@b.com","name":"big","password":"{big}"}}"#
    );

    let resp = app
        .post("/api/v1/auth/register")
        .add_header("content-type", "application/json")
        .bytes(payload.into())
        .await;

    assert!(
        matches!(resp.status_code().as_u16(), 413 | 422),
        "expected 413 or 422 for oversize body, got {}",
        resp.status_code()
    );
}
