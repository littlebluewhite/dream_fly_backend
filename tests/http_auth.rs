//! HTTP integration tests for `/auth/*` endpoints.
//!
//! Every test spins up a fresh `TestApp` via `spawn_test_app` (which wires
//! the real axum router + middleware stack against a per-test sqlx pool).
//! External services — SMTP + Twilio — are replaced by the in-memory
//! recorders from `common::mocks`. The Google OAuth token endpoint is
//! redirected to a `wiremock` server by overriding `auth.google_token_url`.

mod common;

use common::http::{spawn_test_app, spawn_test_app_with};
use serde_json::json;
use sqlx::PgPool;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------- /auth/register ----------------

#[sqlx::test]
async fn register_creates_user_and_returns_tokens(db: PgPool) {
    let app = spawn_test_app(db).await;

    let resp = app
        .post("/api/v1/auth/register")
        .json(&json!({
            "email": "new@example.com",
            "name": "New User",
            "password": "Password!234",
        }))
        .await;

    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["access_token"].as_str().unwrap().len() > 20);
    assert!(body["refresh_token"].as_str().unwrap().len() > 20);
    assert_eq!(body["user"]["email"], "new@example.com");
    assert_eq!(body["user"]["is_active"], true);
    assert_eq!(body["user"]["roles"], json!(["member"]));
}

#[sqlx::test]
async fn register_duplicate_email_returns_conflict(db: PgPool) {
    let app = spawn_test_app(db).await;

    app.register_member("dup@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/auth/register")
        .json(&json!({
            "email": "dup@example.com",
            "name": "Other",
            "password": "Password!234",
        }))
        .await;

    assert_eq!(resp.status_code(), 409);
}

#[sqlx::test]
async fn register_rejects_short_password(db: PgPool) {
    let app = spawn_test_app(db).await;

    let resp = app
        .post("/api/v1/auth/register")
        .json(&json!({
            "email": "weak@example.com",
            "name": "Weak",
            "password": "short",
        }))
        .await;

    assert_eq!(resp.status_code(), 422);
}

#[sqlx::test]
async fn register_rejects_invalid_email(db: PgPool) {
    let app = spawn_test_app(db).await;

    let resp = app
        .post("/api/v1/auth/register")
        .json(&json!({
            "email": "not-an-email",
            "name": "X",
            "password": "Password!234",
        }))
        .await;

    assert_eq!(resp.status_code(), 422);
}

// ---------------- /auth/login ----------------

#[sqlx::test]
async fn login_with_correct_credentials_returns_tokens(db: PgPool) {
    let app = spawn_test_app(db).await;
    app.register_member("login@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/auth/login")
        .json(&json!({
            "email": "login@example.com",
            "password": "Password!234",
        }))
        .await;

    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(!body["access_token"].as_str().unwrap().is_empty());
    assert_eq!(body["user"]["roles"], json!(["member"]));
}

#[sqlx::test]
async fn login_with_wrong_password_returns_unauthorized(db: PgPool) {
    let app = spawn_test_app(db).await;
    app.register_member("login2@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/auth/login")
        .json(&json!({
            "email": "login2@example.com",
            "password": "WrongPass!234",
        }))
        .await;

    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn login_unknown_email_returns_unauthorized(db: PgPool) {
    let app = spawn_test_app(db).await;

    let resp = app
        .post("/api/v1/auth/login")
        .json(&json!({
            "email": "ghost@example.com",
            "password": "Password!234",
        }))
        .await;

    assert_eq!(resp.status_code(), 401);
}

// ---------------- /auth/refresh ----------------

#[sqlx::test]
async fn refresh_rotates_tokens_and_invalidates_old(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("refresh@example.com", "Password!234").await;

    // First refresh should succeed and issue a NEW refresh token.
    let resp = app
        .post("/api/v1/auth/refresh")
        .json(&json!({ "refresh_token": user.refresh_token }))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    let new_refresh = body["refresh_token"].as_str().unwrap().to_string();
    assert_ne!(new_refresh, user.refresh_token);

    // Reusing the OLD refresh token must now revoke the family → 401.
    let resp2 = app
        .post("/api/v1/auth/refresh")
        .json(&json!({ "refresh_token": user.refresh_token }))
        .await;
    assert_eq!(resp2.status_code(), 401);
}

#[sqlx::test]
async fn refresh_with_garbage_token_returns_unauthorized(db: PgPool) {
    let app = spawn_test_app(db).await;

    let resp = app
        .post("/api/v1/auth/refresh")
        .json(&json!({ "refresh_token": "not-a-jwt" }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

// ---------------- /auth/logout ----------------

#[sqlx::test]
async fn logout_is_idempotent(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("logout@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/auth/logout")
        .json(&json!({ "refresh_token": user.refresh_token }))
        .await;
    assert_eq!(resp.status_code(), 200);

    // Second logout of the same token: still 200 (idempotent by design).
    let resp2 = app
        .post("/api/v1/auth/logout")
        .json(&json!({ "refresh_token": user.refresh_token }))
        .await;
    assert_eq!(resp2.status_code(), 200);
}

// ---------------- /auth/google ----------------

#[sqlx::test]
async fn google_auth_upstream_error_returns_bad_request(db: PgPool) {
    // Spin up a wiremock server that returns 400 for /oauth/token, then
    // point the app's google_token_url at it.
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_string("invalid_grant"))
        .mount(&upstream)
        .await;

    let url = format!("{}/oauth/token", upstream.uri());
    let app = spawn_test_app_with(db, |cfg| {
        cfg.auth.google_token_url = url;
    })
    .await;

    let resp = app
        .post("/api/v1/auth/google")
        .json(&json!({ "code": "fake-authorization-code" }))
        .await;

    assert_eq!(resp.status_code(), 400);
}

// ---------------- /auth/otp/send ----------------

#[sqlx::test]
async fn otp_send_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;

    let resp = app
        .post("/api/v1/auth/otp/send")
        .json(&json!({ "phone": "+15551234567" }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn otp_send_authenticated_records_sms(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("otp@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/auth/otp/send")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "phone": "+15551234567" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());

    // MockSmsClient should have recorded exactly one OTP message.
    let sent = app.sms.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "+15551234567");
    // last_otp_code returns the 6-digit code stashed in the mock.
    let code = app.sms.last_otp_code().expect("otp code recorded");
    assert_eq!(code.len(), 6);
}

// ---------------- /auth/otp/verify ----------------

#[sqlx::test]
async fn otp_verify_round_trip_marks_phone_verified(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("otpv@example.com", "Password!234").await;

    // Send first to populate Redis with a code for this user.
    app.post("/api/v1/auth/otp/send")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "phone": "+15551234567" }))
        .await;
    let code = app.sms.last_otp_code().expect("code");

    let resp = app
        .post("/api/v1/auth/otp/verify")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "phone": "+15551234567", "code": code }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());

    // DB invariant: phone_verified flipped to true.
    let verified: (bool,) = sqlx::query_as("SELECT phone_verified FROM users WHERE id = $1")
        .bind(user.user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(verified.0);
}

#[sqlx::test]
async fn otp_verify_wrong_code_returns_bad_request(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("otpw@example.com", "Password!234").await;

    app.post("/api/v1/auth/otp/send")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "phone": "+15551234567" }))
        .await;

    let resp = app
        .post("/api/v1/auth/otp/verify")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "phone": "+15551234567", "code": "000000" }))
        .await;
    assert_eq!(resp.status_code(), 400);
}

// ---------------- /auth/password/forgot ----------------

#[sqlx::test]
async fn forgot_password_for_existing_user_records_email(db: PgPool) {
    // `forgot_password` is rate-limited at 3 per email per hour on Redis, so
    // use a per-test unique address to avoid leftover counters from prior runs.
    let app = spawn_test_app(db).await;
    let email = format!("forgot-{}@example.com", uuid::Uuid::now_v7());
    app.register_member(&email, "Password!234").await;

    let resp = app
        .post("/api/v1/auth/password/forgot")
        .json(&json!({ "email": email }))
        .await;
    assert_eq!(resp.status_code(), 200);

    // The email send is spawned as a background task — wait for it.
    let sent = app.email.wait_for(1, 1000).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, email);
}

#[sqlx::test]
async fn forgot_password_for_unknown_email_still_returns_200(db: PgPool) {
    // Defence against account enumeration: the handler MUST NOT differ its
    // response based on existence.
    let app = spawn_test_app(db).await;
    let email = format!("ghost-{}@example.com", uuid::Uuid::now_v7());

    let resp = app
        .post("/api/v1/auth/password/forgot")
        .json(&json!({ "email": email }))
        .await;
    assert_eq!(resp.status_code(), 200);

    // No email should have been recorded (the user doesn't exist).
    let sent = app.email.wait_for(1, 200).await;
    assert!(sent.is_empty());
}

// ---------------- /auth/password/reset ----------------

#[sqlx::test]
async fn reset_password_with_invalid_token_returns_bad_request(db: PgPool) {
    let app = spawn_test_app(db).await;

    let resp = app
        .post("/api/v1/auth/password/reset")
        .json(&json!({
            "token": "totally-bogus-token",
            "new_password": "NewPassword!234",
        }))
        .await;
    assert_eq!(resp.status_code(), 400);
}
