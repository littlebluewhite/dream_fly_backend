//! HTTP integration tests for `/auth/*` endpoints.
//!
//! Every test spins up a fresh `TestApp` via `spawn_test_app` (which wires
//! the real axum router + middleware stack against a per-test sqlx pool).
//! External services — SMTP + Twilio — are replaced by the in-memory
//! recorders from `common::mocks`. The Google OAuth token endpoint is
//! redirected to a `wiremock` server by overriding `auth.google_token_url`;
//! the two happy-path tests below additionally override `auth.google_jwks_url`
//! and sign a real RS256 id_token so `google_oauth::verify_google_id_token`
//! runs unmodified (see the `GOOGLE_TEST_*` constants near the bottom).

mod common;

use common::http::{spawn_test_app, spawn_test_app_with};
use common::latest_notification;
use redis::AsyncCommands;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;
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

/// `auth::service::google_auth`'s one deliberately asymmetric branch: a
/// brand-new Google user gets a welcome notification (`created_new_user`
/// branch), but linking Google to an existing password account does not
/// resend it (see the comment above `if created_new_user` in
/// `src/modules/auth/service.rs`). Exercising this for real means the id_token
/// must pass real RS256 signature verification against a `wiremock`-served
/// JWKS — not just the token-exchange redirect the test above uses — so
/// these two tests sign with a fixed, test-only RSA key and serve its public
/// half from a mocked `auth.google_jwks_url`.
///
/// PKCS#1 DER-encoded RSA-2048 test-only private key, base64 (never used for
/// anything but signing tokens in this file). Must be PKCS#1
/// (`openssl rsa -traditional -outform DER`), not PKCS#8 — this project's
/// `jsonwebtoken` dependency is built with `features = ["rust_crypto"]` and
/// `default-features = false`, which excludes the `use_pem` feature (so no
/// `EncodingKey::from_rsa_pem`); `EncodingKey::from_rsa_der` expects the
/// traditional PKCS#1 layout, not PKCS#8's `AlgorithmIdentifier`-wrapped one.
const GOOGLE_TEST_PRIV_DER_B64: &str = "MIIEowIBAAKCAQEArvNjgLtycikxZlHRKVZHyUtvhgqovSIo0relMN1QNlxvFuo9dUJ8Q089t7suZo/Zz9sbCvKpfMpPA46zZyAmiOYvC5oB4ex7jUxbjhpSU0rAq9+SoO7bfFjo2tWWzPFG/FBwOoz3gzTZkn6RlZpPXnVAo3wK5XprfDbRBO1imBlnnTMDo5GmM46YsZb71VYfp0THOmsE/9mvBB5fUPBWpQl7eT2a06ripUwxCRZEzPHWjjkP303W1oWqr3KNF10yZmMlCkrnxqcKurlyxI2E1w/Fc2K8Hh1D/IZ5dYKt8Pb8s0hwxs1DspvXEL0iPhcMW0BDqxTi8gTNzYPYtZkwdQIDAQABAoIBABvCEb5N33N2Dji4EAnxPtwVHDmGDO5PUm9WhH77ilvJsDWQTlaByUILu1TgvdS3i71TPBfxVwtt9PnxRQ0+eGa9sOa0FYrhUNwjKp6iFgBRqr7KbxMaOthgqfd4rp/PQ25Km/fqQGZAtymra9FzBZdM3sfhqT/uO8oeT20q9fsAP5ulCOak8DU2FDOLILut6DkBQPTGdSXJ65DmBXGsFaie4CoU1vvlqxOeDw1s5UgPYgxuNgkvwDskOmHpMaMjIva8BUrBa2EYHkyzhnjup0xAjbb0yTaa6rHw1GbDCx4QK4qGiDibmna7aVelouZjfpQSv4Ate2KYo9lkq0+fymkCgYEA4SJeybF1rDKv9qXBlTn64nX1XVe6DEn+y1aJXqwFbRG8krLfCvkmW6Qw91obbJoEGUModNX9m1YgVNqXYjSnMnPkaE1y8NKXdNVFP+/cqz8iJ2Eayy0yUJZyqVOQF1MgA3I0f7YoRJnkNb+CWDt3f1REHlBBUse5uF6C8BBxC1kCgYEAxu+06lPzhJ0ikp576H3uv9UH2HnIkKmE3ClH3HFNVUfgrlRFJAIQsXD8iGxd6iETUYac9MoVWcex6316wy4TrkZvi94E9a3QtbpGiyRhkcZEC43gj0J3fXfzV//Wr2GVIEvEYjzPY7h5+yPkr/MqLyt5sP4Perg8jz56Xs41Fn0CgYBTjvInYdoO43Ez1imXPUHEs4sx7dF7pisPRTsPDEGnTaHzwLfP1tFJyhLye1saX7+NsMNfOd06viiZ1dfB91DnBOSNYdF7WG4mStG8/UWluXTvsLbFGi1Gg9Bi0ET2oz+Kh+S8Udt4OrXczQuPu+KKO7hcl+Tm2IIxz8JBX5jVYQKBgA43FLtl0lHYlJ7bekkrroLAqzXZxe4oXtkIjhz/b6I3Z6OtW99t0lmLlE//RlqzkFjUAKUxR4NJ1LnaFoqZ4UgjulbJP5t6lx5VODM7H0m2XChjM/eorTcm+hmAq4uOsoRDRb4rUDp09Spv7yhvfMUwGxr9nIeNYK5vrXjWzU5VAoGBAKiG031/8yxMLVH0rRxFZ+xoCla8fAw135PziT6ZOEABqKvTcaRBedVA3zigiKg8wdH1sBKv7/HjsQc3lXS7BHtoT+KQBa36yvdMaye6XgVg1wA41WgBdLYeBQcSuiAxWkbxjPVSSz0UNe1jMnZqNe+ZMTrrkwuh3X1Yl4Szadba";
/// Base64url (no padding) RSA modulus of the same test key's public half —
/// the JWK `n`. Computed once via `openssl rsa -pubin -modulus` piped through
/// a one-off base64url encode, and independently verified byte-exact by
/// round-tripping a signed token through `DecodingKey::from_rsa_components`
/// (the exact function `google_oauth::verify_google_id_token` calls).
const GOOGLE_TEST_JWK_N: &str = "rvNjgLtycikxZlHRKVZHyUtvhgqovSIo0relMN1QNlxvFuo9dUJ8Q089t7suZo_Zz9sbCvKpfMpPA46zZyAmiOYvC5oB4ex7jUxbjhpSU0rAq9-SoO7bfFjo2tWWzPFG_FBwOoz3gzTZkn6RlZpPXnVAo3wK5XprfDbRBO1imBlnnTMDo5GmM46YsZb71VYfp0THOmsE_9mvBB5fUPBWpQl7eT2a06ripUwxCRZEzPHWjjkP303W1oWqr3KNF10yZmMlCkrnxqcKurlyxI2E1w_Fc2K8Hh1D_IZ5dYKt8Pb8s0hwxs1DspvXEL0iPhcMW0BDqxTi8gTNzYPYtZkwdQ";
const GOOGLE_TEST_JWK_E: &str = "AQAB";

/// Sign a Google-shaped id_token with the fixed test RSA key above.
///
/// `kid` should be unique per test: `google_oauth::verify_google_id_token`'s
/// JWKS cache is a single process-wide slot, not keyed by URL, so two tests
/// in this binary sharing a `kid` could observe each other's cached JWKS. A
/// distinct `kid` per test guarantees a same-request refetch on a miss (the
/// same fallback path production takes on real Google key rotation) instead
/// of silently reusing another test's key — deterministic regardless of
/// `cargo test`'s execution order/parallelism.
fn sign_google_test_id_token(sub: &str, email: &str, aud: &str, kid: &str) -> String {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};

    #[derive(serde::Serialize)]
    struct Claims<'a> {
        sub: &'a str,
        aud: &'a str,
        iss: &'a str,
        exp: i64,
        iat: i64,
        email: &'a str,
        email_verified: bool,
    }

    let der = STANDARD
        .decode(GOOGLE_TEST_PRIV_DER_B64)
        .expect("decode test RSA key DER");
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.to_string());
    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        sub,
        aud,
        iss: "https://accounts.google.com",
        exp: now + 3600,
        iat: now,
        email,
        email_verified: true,
    };
    let key = EncodingKey::from_rsa_der(&der);
    encode(&header, &claims, &key).expect("sign test id_token")
}

/// JWKS response body for the mocked `auth.google_jwks_url` endpoint: a
/// single key matching the fixed test private key above under `kid`.
fn google_test_jwks_body(kid: &str) -> serde_json::Value {
    json!({
        "keys": [{
            "kid": kid,
            "n": GOOGLE_TEST_JWK_N,
            "e": GOOGLE_TEST_JWK_E,
            "kty": "RSA",
            "alg": "RS256",
        }]
    })
}

#[sqlx::test]
async fn google_auth_new_user_gets_welcome_notification(db: PgPool) {
    let kid = "test-kid-google-new-user";
    let id_token = sign_google_test_id_token(
        "google-sub-new-user",
        "newgoogle@example.com",
        "test-client",
        kid,
    );

    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id_token": id_token })))
        .mount(&upstream)
        .await;
    Mock::given(method("GET"))
        .and(path("/certs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(google_test_jwks_body(kid)))
        .mount(&upstream)
        .await;

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
    let user_id =
        Uuid::parse_str(body["user"]["id"].as_str().expect("user.id")).expect("parse user id");

    // The genuinely-new-Google-user branch (`created_new_user = true`) fires
    // a welcome notification.
    let welcome = latest_notification(&app.db, user_id, "system")
        .await
        .expect("welcome notification row");
    assert_eq!(welcome.0, "Welcome to Dream Fly");
}

#[sqlx::test]
async fn google_auth_linking_existing_account_does_not_resend_welcome(db: PgPool) {
    let kid = "test-kid-google-link-user";
    let id_token = sign_google_test_id_token(
        "google-sub-link-user",
        "linkme@example.com",
        "test-client",
        kid,
    );

    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id_token": id_token })))
        .mount(&upstream)
        .await;
    Mock::given(method("GET"))
        .and(path("/certs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(google_test_jwks_body(kid)))
        .mount(&upstream)
        .await;

    let app = spawn_test_app_with(db, |cfg| {
        cfg.auth.google_token_url = format!("{}/oauth/token", upstream.uri());
        cfg.auth.google_jwks_url = format!("{}/certs", upstream.uri());
    })
    .await;

    // Register a password account first — this already sends a welcome
    // notification (see `register_creates_user_with_hashed_password` in
    // tests/service_auth.rs).
    let user = app.register_member("linkme@example.com", "Password!234").await;

    let welcome_count_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications \
         WHERE user_id = $1 AND type = 'system'::notification_type AND title = 'Welcome to Dream Fly'",
    )
    .bind(user.user_id)
    .fetch_one(&app.db)
    .await
    .expect("count welcome notifications before link");
    assert_eq!(welcome_count_before, 1);

    // Now link Google to that same email — must NOT be treated as a
    // genuinely new user (`created_new_user` stays false).
    let resp = app
        .post("/api/v1/auth/google")
        .json(&json!({ "code": "fake-authorization-code" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());

    let body: serde_json::Value = resp.json();
    let returned_id =
        Uuid::parse_str(body["user"]["id"].as_str().expect("user.id")).expect("parse user id");
    assert_eq!(
        returned_id, user.user_id,
        "google link must resolve to the same existing user"
    );

    let welcome_count_after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications \
         WHERE user_id = $1 AND type = 'system'::notification_type AND title = 'Welcome to Dream Fly'",
    )
    .bind(user.user_id)
    .fetch_one(&app.db)
    .await
    .expect("count welcome notifications after link");
    assert_eq!(
        welcome_count_after, 1,
        "deliberate asymmetry: linking Google to an existing password account must not resend the welcome notification"
    );
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

    // Refactor regression (Step 1): the per-user rate-limit key must still
    // carry a TTL, now set by `redis_counter::incr_with_ttl` instead of the
    // deleted `rate_limit::bump_count`. Read via TestApp's own Redis
    // connection (DB 15) — `common::test_redis()` is DB 0 and would never
    // see a key the app itself wrote.
    let mut redis = app.redis_conn().await;
    let ttl: i64 = redis
        .ttl(format!("otp_rate:{}", user.user_id))
        .await
        .expect("ttl");
    assert!(ttl > 0, "expected otp_rate TTL > 0, got {ttl}");
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
