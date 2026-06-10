//! End-to-end HTTP tests covering every variant of `AppError` → HTTP
//! status + JSON body shape. The error response contract
//! (`{"error": "<message>"}`) is consumed by the frontend, so any accidental
//! refactor that changes the shape — e.g. to `{"message": ...}` or
//! `{"error": {"detail": ...}}` — must break these tests.
//!
//! Each test drives a real endpoint that produces the target variant rather
//! than instantiating `AppError` in-memory, so the assertions include the
//! full middleware stack (tracing, CORS, validation extractor).
//!
//! Variants covered:
//! - NotFound (404)
//! - BadRequest (400)
//! - Unauthorized (401) — from the `AuthUser` extractor
//! - Forbidden (403) — from `require_role`
//! - Conflict (409) — from auth register duplicate email
//! - Validation (422) — from `ValidatedJson` rejecting a too-short password
//! - Payload too large (413) — from the body-limit middleware
//!
//! Internal/Database/Redis (500) variants are intentionally NOT tested here
//! because we can't inject a failing DB without breaking test isolation.
//! The masking guarantee ("internal server error" — no message leak) is
//! verified by inspection of `src/error/mod.rs`.

mod common;

use common::http::spawn_test_app;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

/// Helper: assert the response has the expected status AND the body is
/// `{"error": <string>}` with no extra keys.
fn assert_error_shape(body: &Value, expected_status: u16, status_code: u16) {
    assert_eq!(
        status_code, expected_status,
        "expected status {expected_status}, got {status_code}, body={body}"
    );
    let obj = body.as_object().expect("response body is an object");
    assert_eq!(obj.len(), 1, "error body must have exactly one key, got {obj:?}");
    let err = obj.get("error").expect("missing `error` key");
    assert!(
        err.is_string(),
        "`error` value must be a string, got {err:?}"
    );
    assert!(
        !err.as_str().unwrap().is_empty(),
        "`error` message must not be empty"
    );
}

#[sqlx::test]
async fn not_found_variant_returns_404_with_error_shape(db: PgPool) {
    // Hitting a nonexistent public resource. `courses/{slug-or-id}` is
    // public and resolves to `NotFound` when the slug doesn't exist.
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/courses/totally-missing-slug").await;
    assert_error_shape(&resp.json::<Value>(), 404, resp.status_code().as_u16());
}

#[sqlx::test]
async fn bad_request_variant_returns_400_with_error_shape(db: PgPool) {
    // `schedule::service::get_monthly_schedule` rejects month < 1 or > 12
    // with `AppError::BadRequest`. Public endpoint, no auth needed.
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/schedule?year=2026&month=99").await;
    let body: Value = resp.json();
    assert_error_shape(&body, 400, resp.status_code().as_u16());
    assert!(
        body["error"].as_str().unwrap().contains("month"),
        "error message should mention `month`, got: {body}"
    );
}

#[sqlx::test]
async fn unauthorized_variant_returns_401_with_fixed_message(db: PgPool) {
    // AuthUser extractor rejects a missing Bearer token with Unauthorized.
    // The contract guarantees the message is a fixed "unauthorized" — it
    // must NOT vary by request so that log-based fingerprinting is useless
    // and the frontend can rely on a single sentinel string.
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/users/me").await;
    let body: Value = resp.json();
    assert_error_shape(&body, 401, resp.status_code().as_u16());
    assert_eq!(body["error"], "unauthorized");
}

#[sqlx::test]
async fn forbidden_variant_returns_403_with_error_shape(db: PgPool) {
    // A plain member hitting an admin-only endpoint triggers
    // `auth.require_role("admin")` → Forbidden. The permissions router
    // mounts at `/api/v1/roles` (no `/permissions/` prefix — see
    // `src/modules/permissions/routes.rs`).
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("forbidden@example.com", "Password!234")
        .await;

    let resp = app
        .post("/api/v1/roles")
        .authorization_bearer(&user.access_token)
        .json(&json!({"name": "reviewer", "description": "x"}))
        .await;
    let body: Value = resp.json();
    assert_error_shape(&body, 403, resp.status_code().as_u16());
}

#[sqlx::test]
async fn conflict_variant_returns_409_with_error_shape(db: PgPool) {
    // Duplicate email on `/auth/register` → Conflict.
    let app = spawn_test_app(db).await;
    app.register_member("dup@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/auth/register")
        .json(&json!({
            "email": "dup@example.com",
            "name": "Second",
            "password": "Password!234"
        }))
        .await;
    let body: Value = resp.json();
    assert_error_shape(&body, 409, resp.status_code().as_u16());
}

#[sqlx::test]
async fn validation_variant_returns_422_with_error_shape(db: PgPool) {
    // `ValidatedJson` rejects a too-short password — min length enforced
    // via `#[validate(length(min = 8))]` on the DTO. Goes through the
    // `Validation` variant (not BadRequest).
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/auth/register")
        .json(&json!({
            "email": "weak@example.com",
            "name": "Weak Pass",
            "password": "short"
        }))
        .await;
    let body: Value = resp.json();
    assert_error_shape(&body, 422, resp.status_code().as_u16());
}

#[sqlx::test]
async fn payload_too_large_is_rejected(db: PgPool) {
    // `RequestBodyLimitLayer` in startup caps bodies at 2MB. A 3MB body
    // must be rejected before the handler runs — either as 413 (if the
    // handler uses plain `Json<T>`, whose `IntoResponse` propagates
    // length-limit errors verbatim) or as 422 (via `ValidatedJson`,
    // which normalizes every `JsonRejection` — including
    // `BytesRejection::LengthLimitError` — to `AppError::Validation`).
    //
    // Every current handler uses `ValidatedJson`, so in practice this path
    // fires as 422 today. We accept both so the test survives a future
    // refactor that flips a handler back to plain `Json<T>` without
    // silently letting an oversized body slip through to the handler.
    let app = spawn_test_app(db).await;
    let user = app.register_member("big@example.com", "Password!234").await;

    // 3MB payload with a valid JSON shape.
    let huge_note = "a".repeat(3 * 1024 * 1024);
    let body = json!({"note": huge_note});

    let resp = app
        .post(&format!("/api/v1/coaches/{}/clock-in", Uuid::now_v7()))
        .authorization_bearer(&user.access_token)
        .json(&body)
        .await;

    let status = resp.status_code().as_u16();
    assert!(
        matches!(status, 413 | 422),
        "expected 413 or 422 from body-limit enforcement; status={} body={}",
        status,
        resp.text()
    );
}

#[tokio::test]
async fn internal_variant_masks_sensitive_details_in_500_body() {
    // We can't easily inject a DB failure through a running app without
    // breaking per-test DB isolation, but the masking guarantee is
    // expressible directly against `AppError::into_response()`. Build an
    // `AppError::Internal` carrying a deliberately-sensitive message
    // (imagine it contained a connection string or a secret) and confirm
    // the rendered response body contains ONLY "internal server error"
    // and no trace of the original string.
    use axum::body::to_bytes;
    use axum::response::IntoResponse;
    use dream_fly_backend::error::AppError;

    let sensitive = "postgres://user:super-secret-password@db/internal-leak-canary";
    let err = AppError::Internal(anyhow::anyhow!("{sensitive}"));
    let resp = err.into_response();

    assert_eq!(resp.status(), 500);

    let bytes = to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    let body_str = std::str::from_utf8(&bytes).expect("utf-8 body");

    assert!(
        body_str.contains("internal server error"),
        "expected masked message, got: {body_str}"
    );
    assert!(
        !body_str.contains(sensitive),
        "sensitive details leaked into 500 body: {body_str}"
    );
    assert!(
        !body_str.contains("super-secret-password"),
        "password leaked into 500 body: {body_str}"
    );

    // Also assert the canonical shape: body is `{"error": "..."}`.
    let parsed: Value = serde_json::from_str(body_str).expect("json body");
    let obj = parsed.as_object().unwrap();
    assert_eq!(obj.len(), 1);
    assert_eq!(obj.get("error").unwrap().as_str().unwrap(), "internal server error");
}
