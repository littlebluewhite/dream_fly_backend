//! Tests for the production Google identity adapter (`GoogleOAuthClient`),
//! driven directly through the `GoogleIdentityProvider` port against a
//! `wiremock` server playing Google (see `common::google`). No database, no
//! router: login rules live in `service_auth.rs` (with a fake provider) and
//! the route wiring in `http_auth.rs`'s single end-to-end test.

mod common;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use dream_fly_backend::error::AppError;
use dream_fly_backend::utils::google_oauth::{GoogleIdentityProvider, GoogleOAuthClient};

use common::google::{
    TEST_AUDIENCE, google_config, mount_google, mount_jwks, mount_token, sign_id_token,
    sign_id_token_with,
};

fn adapter(upstream: &MockServer) -> GoogleOAuthClient {
    GoogleOAuthClient::new(&google_config(upstream), reqwest::Client::new())
}

fn expect_bad_request(result: Result<impl std::fmt::Debug, AppError>) -> String {
    match result {
        Err(AppError::BadRequest(msg)) => msg,
        other => panic!("expected BadRequest, got {other:?}"),
    }
}

#[tokio::test]
async fn verified_code_yields_identity() {
    let upstream = mount_google("google-sub-happy", "happy@example.com", "kid-happy").await;

    let identity = adapter(&upstream)
        .verify_code("auth-code")
        .await
        .expect("verify code");

    assert_eq!(identity.sub, "google-sub-happy");
    assert_eq!(identity.email, "happy@example.com");
    assert_eq!(identity.name, None);
    assert_eq!(identity.picture, None);
}

#[tokio::test]
async fn failed_code_exchange_is_bad_request() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_string("invalid_grant"))
        .mount(&upstream)
        .await;

    let msg = expect_bad_request(adapter(&upstream).verify_code("bad-code").await);
    assert_eq!(msg, "Google authentication failed");
}

/// A fresh adapter starts with an empty JWKS cache and its first
/// verification populates it. When Google then rotates its signing key, the
/// cached (still in-TTL) key set misses the new `kid`; only the kid-miss
/// force-refresh re-fetch can find it. Without that fallback the second call
/// would be a BadRequest.
#[tokio::test]
async fn refetches_jwks_on_kid_rotation() {
    let upstream = MockServer::start().await;
    let google = adapter(&upstream);

    // Phase 1: kid A end-to-end — token exchange + JWKS both serve kid A.
    let token_a = sign_id_token(
        "google-sub-rotation",
        "rotation@example.com",
        TEST_AUDIENCE,
        "kid-a",
    );
    mount_token(&upstream, &token_a).await;
    mount_jwks(&upstream, "kid-a").await;
    google.verify_code("code-1").await.expect("phase 1");

    // Phase 2: Google "rotated" to kid B. reset() drops BOTH mocks (they
    // share `upstream`), so re-mount both — a JWKS-only remount would fail the
    // token exchange before ever reaching the kid miss.
    upstream.reset().await;
    let token_b = sign_id_token(
        "google-sub-rotation",
        "rotation@example.com",
        TEST_AUDIENCE,
        "kid-b",
    );
    mount_token(&upstream, &token_b).await;
    mount_jwks(&upstream, "kid-b").await;

    let identity = google
        .verify_code("code-2")
        .await
        .expect("phase 2 must succeed via the kid-miss force-refresh");
    assert_eq!(identity.sub, "google-sub-rotation");
}

#[tokio::test]
async fn unverified_email_is_rejected() {
    let upstream = MockServer::start().await;
    let token = sign_id_token_with(
        "google-sub-unverified",
        "unverified@example.com",
        TEST_AUDIENCE,
        "kid-unverified",
        false,
    );
    mount_token(&upstream, &token).await;
    mount_jwks(&upstream, "kid-unverified").await;

    let msg = expect_bad_request(adapter(&upstream).verify_code("auth-code").await);
    assert_eq!(msg, "Google email is not verified");
}

#[tokio::test]
async fn token_for_another_audience_is_rejected() {
    let upstream = MockServer::start().await;
    let token = sign_id_token(
        "google-sub-wrong-aud",
        "wrong-aud@example.com",
        "some-other-client",
        "kid-wrong-aud",
    );
    mount_token(&upstream, &token).await;
    mount_jwks(&upstream, "kid-wrong-aud").await;

    let msg = expect_bad_request(adapter(&upstream).verify_code("auth-code").await);
    assert_eq!(msg, "Google authentication failed");
}
