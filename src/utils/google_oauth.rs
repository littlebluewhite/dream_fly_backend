//! Verified Google ID-token handling.
//!
//! Previously `auth::service` split the id_token on `.` and base64-decoded the
//! payload without verifying the signature, on the reasoning that the token
//! came over TLS from Google's token endpoint. That argument is narrow and
//! brittle: any future refactor (One Tap, mobile, hybrid flow) that hands the
//! id_token straight to the same helper becomes a total auth bypass.
//!
//! This module fetches Google's JWKS, caches it for an hour, and verifies the
//! JWT's RS256 signature plus issuer/audience/expiry claims. Fail-closed.

use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::error::AppError;

const GOOGLE_ISSUERS: &[&str] = &["https://accounts.google.com", "accounts.google.com"];
/// Cache JWKS for an hour. Google rotates these roughly daily, so we re-fetch
/// well inside the key lifetime but without hammering their endpoint.
const JWKS_TTL_SECONDS: i64 = 3600;

/// Fields we actually care about on a Google id_token. Anything extra is
/// ignored.
#[derive(Debug, Deserialize, Clone)]
pub struct GoogleIdTokenClaims {
    pub sub: String,
    pub aud: String,
    pub iss: String,
    pub exp: i64,
    pub email: String,
    pub email_verified: bool,
    pub name: Option<String>,
    pub picture: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
    #[allow(dead_code)]
    #[serde(default)]
    alg: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    kty: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct Jwks {
    keys: Vec<Jwk>,
}

/// Per-app JWKS cache: the fetched key set plus its fetch time. Owned by
/// `AppState` rather than a process-wide static, so each app instance —
/// including every test — carries its own cache with a naturally isolated
/// lifecycle. Storage only; the refresh semantics (TTL read-path, single
/// kid-miss force-refresh, no single-flight, last-writer-wins) live in
/// `get_jwks` and are unchanged.
pub struct JwksCache {
    entry: RwLock<Option<(Jwks, DateTime<Utc>)>>,
}

impl JwksCache {
    pub fn new() -> Self {
        Self { entry: RwLock::new(None) }
    }
}

impl Default for JwksCache {
    fn default() -> Self {
        Self::new()
    }
}

async fn fetch_jwks(jwks_url: &str, http: &reqwest::Client) -> Result<Jwks, AppError> {
    http.get(jwks_url)
        .send()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("failed to fetch Google JWKS: {e}")))?
        .error_for_status()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Google JWKS returned non-success: {e}")))?
        .json::<Jwks>()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("failed to parse Google JWKS: {e}")))
}

async fn get_jwks(
    cache: &JwksCache,
    jwks_url: &str,
    http: &reqwest::Client,
    force_refresh: bool,
) -> Result<Jwks, AppError> {
    let slot = &cache.entry;

    if !force_refresh {
        let guard = slot.read().await;
        if let Some((jwks, fetched_at)) = guard.as_ref() {
            if (Utc::now() - *fetched_at).num_seconds() < JWKS_TTL_SECONDS {
                return Ok(jwks.clone());
            }
        }
    }

    let fresh = fetch_jwks(jwks_url, http).await?;
    let mut guard = slot.write().await;
    *guard = Some((fresh.clone(), Utc::now()));
    Ok(fresh)
}

/// Verify a Google-issued id_token using Google's published RS256 public keys.
///
/// Enforces:
/// - Signature is valid under one of Google's current public keys.
/// - `iss` is one of the Google-documented values.
/// - `aud` equals the provided `expected_audience` (the configured client id).
/// - `exp` is in the future (with 30s leeway to account for clock skew).
/// - `email_verified` must be true (the caller is usually also checking this,
///   but we'd rather fail twice than not at all).
///
/// `jwks_url` is configurable (`auth.google_jwks_url`) so integration tests
/// can redirect it to a `wiremock` server, mirroring `google_token_url`.
pub async fn verify_google_id_token(
    cache: &JwksCache,
    http: &reqwest::Client,
    id_token: &str,
    expected_audience: &str,
    jwks_url: &str,
) -> Result<GoogleIdTokenClaims, AppError> {
    let header = decode_header(id_token)
        .map_err(|_| AppError::BadRequest("invalid id_token header".into()))?;

    if header.alg != Algorithm::RS256 {
        // Refuse unexpected algorithms — this defends against the classic
        // "alg: none" downgrade and key-confusion attacks.
        return Err(AppError::BadRequest("unsupported id_token algorithm".into()));
    }

    let kid = header
        .kid
        .ok_or_else(|| AppError::BadRequest("id_token missing kid".into()))?;

    // First pass: look up in cached JWKS. If Google rotated keys and we
    // don't know about the new kid yet, force a refresh and try once more.
    let jwks = get_jwks(cache, jwks_url, http, false).await?;
    let matched = jwks.keys.iter().find(|k| k.kid == kid).cloned();

    let key = match matched {
        Some(k) => k,
        None => {
            let refreshed = get_jwks(cache, jwks_url, http, true).await?;
            refreshed
                .keys
                .into_iter()
                .find(|k| k.kid == kid)
                .ok_or_else(|| {
                    tracing::warn!(%kid, "Google id_token kid not found in JWKS even after refresh");
                    AppError::BadRequest("Google authentication failed".into())
                })?
        }
    };

    let decoding_key = DecodingKey::from_rsa_components(&key.n, &key.e)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("invalid JWK modulus/exp: {e}")))?;

    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(GOOGLE_ISSUERS);
    validation.set_audience(&[expected_audience]);
    validation.set_required_spec_claims(&["exp", "iat", "sub", "iss", "aud"]);
    validation.leeway = 30;

    let token_data =
        decode::<GoogleIdTokenClaims>(id_token, &decoding_key, &validation).map_err(|e| {
            tracing::warn!(error = %e, "Google id_token verification failed");
            AppError::BadRequest("Google authentication failed".into())
        })?;

    if !token_data.claims.email_verified {
        return Err(AppError::BadRequest("Google email is not verified".into()));
    }

    Ok(token_data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A helper client; never actually used since all tests in this module
    /// fail before any network call.
    fn stub_client() -> reqwest::Client {
        reqwest::Client::new()
    }

    #[tokio::test]
    async fn rejects_garbage_token_header() {
        // `decode_header` fails immediately → BadRequest, no network call.
        let cache = JwksCache::new();
        let err = verify_google_id_token(
            &cache,
            &stub_client(),
            "not-a-jwt",
            "aud",
            "http://127.0.0.1:1/certs",
        )
        .await
        .unwrap_err();
        match err {
            AppError::BadRequest(msg) => {
                assert!(msg.contains("invalid id_token header"), "got: {msg}")
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_alg_none_downgrade_attack() {
        // Craft a JWT whose header declares `alg: none`. This is the
        // classic JWT downgrade attack. Defense-in-depth here is layered:
        //   1. `jsonwebtoken::decode_header` itself refuses to parse
        //      `"alg":"none"` (the `Algorithm` enum has no `None` variant),
        //      so we fail at the header-parse step with "invalid id_token
        //      header".
        //   2. If that ever changes upstream, our explicit
        //      `header.alg != Algorithm::RS256` check catches it with
        //      "unsupported id_token algorithm".
        // Either message is an acceptable rejection. This test guards
        // against a future refactor that accidentally accepts `alg:none`.
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT","kid":"abc"}"#);
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"1","iss":"x","aud":"y","exp":9999999999}"#);
        let forged = format!("{header}.{payload}.");

        let cache = JwksCache::new();
        let err = verify_google_id_token(
            &cache,
            &stub_client(),
            &forged,
            "y",
            "http://127.0.0.1:1/certs",
        )
        .await
        .unwrap_err();
        match err {
            AppError::BadRequest(msg) => {
                assert!(
                    msg.contains("invalid id_token header")
                        || msg.contains("unsupported id_token algorithm"),
                    "expected rejection of alg:none, got: {msg}"
                );
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_rs256_token_missing_kid() {
        // Header is RS256 but no `kid` — we can't look up a key to verify.
        // Must fail at the kid check before attempting any HTTP call to JWKS.
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"1","iss":"x","aud":"y","exp":9999999999}"#);
        // Signature doesn't matter — we never get that far.
        let forged = format!("{header}.{payload}.sig");

        let cache = JwksCache::new();
        let err = verify_google_id_token(
            &cache,
            &stub_client(),
            &forged,
            "y",
            "http://127.0.0.1:1/certs",
        )
        .await
        .unwrap_err();
        match err {
            AppError::BadRequest(msg) => {
                assert!(msg.contains("id_token missing kid"), "got: {msg}")
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }
}
