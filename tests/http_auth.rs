//! HTTP integration tests for `/auth/*` endpoints.
//!
//! Every test spins up a fresh `TestApp` via `spawn_test_app` (which wires
//! the real axum router + middleware stack against a per-test sqlx pool).
//! SMTP is replaced by the in-memory recorder from `common::mocks`
//! (`app.email`). Twilio and Google OAuth have no mock here: both are
//! redirected to a `wiremock` server by overriding `sms.twilio_base_url` /
//! `auth.google_token_url` + `auth.google_jwks_url` (see `common::twilio`
//! and `common::google` for the mount helpers). `/auth/google` has a single
//! end-to-end test; its login rules live in `service_auth.rs` (fake
//! provider) and the adapter's edge cases in `google_identity.rs`.

mod common;

use common::google::mount_google;
use common::http::{spawn_test_app, spawn_test_app_with};
use common::twilio::{extract_otp_code, mount_twilio, twilio_sent};
use serde_json::json;
use sqlx::PgPool;
use wiremock::matchers::method;
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

/// Task P4-B2 regression: `birth_date` is deliberately NOT a field on
/// `RegisterRequest` (kept out to minimize signup friction — see
/// `users::dto::CreateUserRequest`/`UpdateProfileRequest` instead). A plain
/// register body with no `birth_date` key must keep working unchanged.
#[sqlx::test]
async fn register_without_birth_date_still_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;

    let resp = app
        .post("/api/v1/auth/register")
        .json(&json!({
            "email": "nobday-register@example.com",
            "name": "No Birthday",
            "password": "Password!234",
        }))
        .await;

    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
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

/// The route wires the real `GoogleOAuthClient` (code exchange + JWKS
/// signature verification, both against `wiremock`) into
/// `auth::service::google_auth` and returns a session for a newly born
/// `member`.
#[sqlx::test]
async fn google_auth_route_end_to_end(db: PgPool) {
    let upstream = mount_google("google-sub-e2e", "e2e-google@example.com", "test-kid-e2e").await;
    let app = spawn_test_app_with(db, |cfg| {
        cfg.auth.google_token_url = format!("{}/oauth/token", upstream.uri());
        cfg.auth.google_jwks_url = format!("{}/certs", upstream.uri());
    })
    .await;

    let resp = app
        .post("/api/v1/auth/google")
        .json(&json!({ "code": "fake-authorization-code" }))
        .await;

    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["access_token"].as_str().unwrap().len() > 20);
    assert!(body["refresh_token"].as_str().unwrap().len() > 20);
    assert_eq!(body["user"]["email"], "e2e-google@example.com");
    assert_eq!(body["user"]["roles"], json!(["member"]));
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
    let twilio_server = MockServer::start().await;
    mount_twilio(&twilio_server).await;

    let app = spawn_test_app_with(db, |cfg| {
        cfg.sms.twilio_base_url = twilio_server.uri();
    })
    .await;
    let user = app.register_member("otp@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/auth/otp/send")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "phone": "+15551234567" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());

    // SmsClient should have posted exactly one message to Twilio's Messages
    // endpoint.
    let sent = twilio_sent(&twilio_server).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "+15551234567");
    assert_eq!(sent[0].from, "+10000000000");

    // First full-text coverage of the real production template — the old
    // mock recorded its own divergent "OTP: {code}" text instead (see
    // `src/utils/sms.rs::send_otp`).
    let code = extract_otp_code(&sent[0].body).expect("otp code in sms body");
    assert_eq!(code.len(), 6);
    assert_eq!(
        sent[0].body,
        format!("Your Dream Fly verification code is: {code}. Valid for 5 minutes.")
    );
}

/// Exercises the real non-2xx branch in `send_sms` (`src/utils/sms.rs`)
/// — unreachable through the previous mock
/// seam, whose `fail_next()` switch short-circuited before any HTTP
/// request was made.
#[sqlx::test]
async fn otp_send_twilio_failure_returns_500(db: PgPool) {
    let twilio_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&twilio_server)
        .await;

    let app = spawn_test_app_with(db, |cfg| {
        cfg.sms.twilio_base_url = twilio_server.uri();
    })
    .await;
    let user = app.register_member("otpfail@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/auth/otp/send")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "phone": "+15551234567" }))
        .await;
    assert_eq!(resp.status_code(), 500, "body={}", resp.text());
}

// ---------------- /auth/otp/verify ----------------

#[sqlx::test]
async fn otp_verify_round_trip_marks_phone_verified(db: PgPool) {
    let twilio_server = MockServer::start().await;
    mount_twilio(&twilio_server).await;

    let app = spawn_test_app_with(db, |cfg| {
        cfg.sms.twilio_base_url = twilio_server.uri();
    })
    .await;
    let user = app.register_member("otpv@example.com", "Password!234").await;

    // Send first to store a code for this user.
    app.post("/api/v1/auth/otp/send")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "phone": "+15551234567" }))
        .await;
    let sent = twilio_sent(&twilio_server).await;
    let code = extract_otp_code(&sent.last().expect("otp sms sent").body).expect("otp code");

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
    let twilio_server = MockServer::start().await;
    mount_twilio(&twilio_server).await;

    let app = spawn_test_app_with(db, |cfg| {
        cfg.sms.twilio_base_url = twilio_server.uri();
    })
    .await;
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
    // `forgot_password` is rate-limited at 3 per email per hour; a per-test
    // unique address keeps this independent of other tests.
    let app = spawn_test_app(db).await;
    let email = format!("forgot-{}@example.com", uuid::Uuid::now_v7());
    app.register_member(&email, "Password!234").await;

    let resp = app
        .post("/api/v1/auth/password/forgot")
        .json(&json!({ "email": email }))
        .await;
    assert_eq!(resp.status_code(), 200);

    // The email send is spawned as a background task — drain it so the
    // assertion below is deterministic instead of racing a fixed sleep.
    app.drain_background().await;
    let sent = app.email.sent();
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

    // No email should have been recorded (the user doesn't exist). The
    // unknown-email branch returns before any spawn (`service.rs`'s
    // `forgot_password`, early `None` return), so draining first is still
    // safe and, unlike a fixed sleep, turns deterministically red if a
    // future regression starts spawning on this branch.
    app.drain_background().await;
    let sent = app.email.sent();
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

// ---------------- Task E2: x-request-id -> outbox correlation_id ----------------

/// Full-chain proof that `SetRequestIdLayer` -> `RequestId` extractor ->
/// `auth::service::register` -> `insert_domain_event_tx` are actually wired
/// together, not just individually unit-tested. `SetRequestIdLayer` never
/// overwrites an existing `x-request-id` header, so the value set here on
/// the request must survive all the way into the outbox row's payload.
#[sqlx::test]
async fn register_with_x_request_id_header_lands_in_outbox_correlation_id(db: PgPool) {
    let app = spawn_test_app(db).await;

    let resp = app
        .post("/api/v1/auth/register")
        .add_header("x-request-id", "rid-http-1")
        .json(&json!({
            "email": "corr-http@example.com",
            "name": "Corr Http",
            "password": "Password!234",
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());

    let correlation_id: String =
        sqlx::query_scalar("SELECT payload->>'correlation_id' FROM events_outbox")
            .fetch_one(&app.db)
            .await
            .expect("user_registered outbox row");
    assert_eq!(correlation_id, "rid-http-1");
}
