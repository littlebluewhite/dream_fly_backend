//! Play Google for tests of the real `GoogleOAuthClient` adapter: sign
//! Google-shaped id_tokens with a fixed test-only RSA key and serve the token
//! exchange + JWKS endpoints from a `wiremock` server. Point
//! `auth.google_token_url` / `auth.google_jwks_url` at `{uri}/oauth/token` /
//! `{uri}/certs` (see [`google_config`]).
//!
//! Service-level tests don't need any of this — they use
//! `mocks::FakeGoogleIdentity` instead.

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use dream_fly_backend::config::AuthConfig;

/// PKCS#1 DER-encoded RSA-2048 test-only private key, base64 (never used for
/// anything but signing test tokens). Must be PKCS#1
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
/// (the exact function the adapter's JWKS verification calls).
const GOOGLE_TEST_JWK_N: &str = "rvNjgLtycikxZlHRKVZHyUtvhgqovSIo0relMN1QNlxvFuo9dUJ8Q089t7suZo_Zz9sbCvKpfMpPA46zZyAmiOYvC5oB4ex7jUxbjhpSU0rAq9-SoO7bfFjo2tWWzPFG_FBwOoz3gzTZkn6RlZpPXnVAo3wK5XprfDbRBO1imBlnnTMDo5GmM46YsZb71VYfp0THOmsE_9mvBB5fUPBWpQl7eT2a06ripUwxCRZEzPHWjjkP303W1oWqr3KNF10yZmMlCkrnxqcKurlyxI2E1w_Fc2K8Hh1D_IZ5dYKt8Pb8s0hwxs1DspvXEL0iPhcMW0BDqxTi8gTNzYPYtZkwdQ";
const GOOGLE_TEST_JWK_E: &str = "AQAB";

/// The audience `common::test_auth_config()` expects (`google_client_id`).
pub const TEST_AUDIENCE: &str = "test-client";

/// Sign a Google-shaped id_token (verified email) with the fixed test key.
pub fn sign_id_token(sub: &str, email: &str, aud: &str, kid: &str) -> String {
    sign_id_token_with(sub, email, aud, kid, true)
}

/// Like [`sign_id_token`], but with an explicit `email_verified` claim.
pub fn sign_id_token_with(
    sub: &str,
    email: &str,
    aud: &str,
    kid: &str,
    email_verified: bool,
) -> String {
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
        email_verified,
    };
    let key = EncodingKey::from_rsa_der(&der);
    encode(&header, &claims, &key).expect("sign test id_token")
}

/// JWKS response body: a single key matching the fixed test private key
/// under `kid`.
pub fn jwks_body(kid: &str) -> serde_json::Value {
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

/// Mount `POST /oauth/token` returning `id_token`.
pub async fn mount_token(upstream: &MockServer, id_token: &str) {
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id_token": id_token })))
        .mount(upstream)
        .await;
}

/// Mount `GET /certs` serving the test key under `kid`.
pub async fn mount_jwks(upstream: &MockServer, kid: &str) {
    Mock::given(method("GET"))
        .and(path("/certs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks_body(kid)))
        .mount(upstream)
        .await;
}

/// Start a `wiremock` server that plays Google for one identity: the token
/// exchange returns an id_token for `sub`/`email` signed under `kid`, and the
/// JWKS endpoint serves that `kid`.
pub async fn mount_google(sub: &str, email: &str, kid: &str) -> MockServer {
    let upstream = MockServer::start().await;
    mount_token(&upstream, &sign_id_token(sub, email, TEST_AUDIENCE, kid)).await;
    mount_jwks(&upstream, kid).await;
    upstream
}

/// `common::test_auth_config()` with the token/JWKS URLs pointed at
/// `upstream`.
pub fn google_config(upstream: &MockServer) -> AuthConfig {
    let mut cfg = super::test_auth_config();
    cfg.google_token_url = format!("{}/oauth/token", upstream.uri());
    cfg.google_jwks_url = format!("{}/certs", upstream.uri());
    cfg
}
