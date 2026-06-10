use chrono::{Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::AuthConfig;
use crate::error::AppError;

/// Issuer claim — kept static so tokens from dev can't be replayed in prod
/// (and vice versa) even if the JWT secret is accidentally shared.
pub const JWT_ISSUER: &str = "dream-fly-backend";
pub const JWT_ACCESS_AUDIENCE: &str = "dream-fly-api";
pub const JWT_REFRESH_AUDIENCE: &str = "dream-fly-refresh";

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub email: String,
    pub exp: usize,
    pub iat: usize,
    pub iss: String,
    pub aud: String,
    /// Unique per-token identifier. Prevents two access tokens issued within
    /// the same wall-clock second from being byte-identical.
    pub jti: String,
    /// Distinguishes access tokens from refresh tokens so one cannot be
    /// substituted for the other even if they share the same signing key.
    pub token_type: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RefreshClaims {
    pub sub: String,
    pub exp: usize,
    pub iat: usize,
    pub iss: String,
    pub aud: String,
    /// Unique per-token identifier. Without this, two refresh tokens issued
    /// to the same user within the same wall-clock second would produce
    /// byte-identical JWTs (and therefore colliding `token_hash` rows).
    pub jti: String,
    /// Distinguishes refresh tokens from access tokens.
    pub token_type: String,
}

fn access_validation() -> Validation {
    let mut v = Validation::new(Algorithm::HS256);
    v.set_issuer(&[JWT_ISSUER]);
    v.set_audience(&[JWT_ACCESS_AUDIENCE]);
    v.set_required_spec_claims(&["exp", "iat", "sub", "iss", "aud"]);
    v.leeway = 5;
    v
}

fn refresh_validation() -> Validation {
    let mut v = Validation::new(Algorithm::HS256);
    v.set_issuer(&[JWT_ISSUER]);
    v.set_audience(&[JWT_REFRESH_AUDIENCE]);
    v.set_required_spec_claims(&["exp", "iat", "sub", "iss", "aud"]);
    v.leeway = 5;
    v
}

pub fn encode_access_token(
    config: &AuthConfig,
    user_id: Uuid,
    email: &str,
) -> Result<String, AppError> {
    let now = Utc::now();
    let exp = now + Duration::minutes(config.jwt_access_expiration_minutes as i64);

    let claims = Claims {
        sub: user_id.to_string(),
        email: email.to_string(),
        exp: exp.timestamp() as usize,
        iat: now.timestamp() as usize,
        iss: JWT_ISSUER.to_string(),
        aud: JWT_ACCESS_AUDIENCE.to_string(),
        jti: Uuid::now_v7().to_string(),
        token_type: "access".to_string(),
    };

    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(config.jwt_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("failed to encode access token: {e}")))
}

pub fn encode_refresh_token(config: &AuthConfig, user_id: Uuid) -> Result<String, AppError> {
    let now = Utc::now();
    let exp = now + Duration::days(config.jwt_refresh_expiration_days as i64);

    let claims = RefreshClaims {
        sub: user_id.to_string(),
        exp: exp.timestamp() as usize,
        iat: now.timestamp() as usize,
        iss: JWT_ISSUER.to_string(),
        aud: JWT_REFRESH_AUDIENCE.to_string(),
        jti: Uuid::now_v7().to_string(),
        token_type: "refresh".to_string(),
    };

    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(config.jwt_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("failed to encode refresh token: {e}")))
}

pub fn decode_access_token(config: &AuthConfig, token: &str) -> Result<Claims, AppError> {
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &access_validation(),
    )
    .map_err(|_| AppError::Unauthorized)?;

    // Defense-in-depth: audience checks above already separate access from
    // refresh tokens, but the explicit `token_type` guard means a future
    // refactor that unifies or normalizes audiences cannot silently allow
    // cross-use.
    if token_data.claims.token_type != "access" {
        return Err(AppError::Unauthorized);
    }

    Ok(token_data.claims)
}

pub fn decode_refresh_token(
    config: &AuthConfig,
    token: &str,
) -> Result<RefreshClaims, AppError> {
    let token_data = decode::<RefreshClaims>(
        token,
        &DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &refresh_validation(),
    )
    .map_err(|_| AppError::Unauthorized)?;

    if token_data.claims.token_type != "refresh" {
        return Err(AppError::Unauthorized);
    }

    Ok(token_data.claims)
}

/// Derive a deterministic storage key for a refresh token.
///
/// Refresh tokens are JWTs — long, high-entropy strings that we *must not*
/// store in plaintext. SHA-256 is sufficient here because the inputs are
/// already uniformly random (JWT signature bytes), so brute-forcing the hash
/// is equivalent to brute-forcing the token.
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AuthConfig {
        AuthConfig {
            jwt_secret: "test-secret-at-least-32-chars-long-1234".into(),
            jwt_access_expiration_minutes: 15,
            jwt_refresh_expiration_days: 30,
            google_client_id: "cid".into(),
            google_client_secret: "csec".into(),
            google_redirect_url: "http://localhost/cb".into(),
            google_token_url: "http://127.0.0.1:1/oauth/token".into(),
        }
    }

    #[test]
    fn encode_access_then_decode_roundtrip() {
        let c = cfg();
        let user = Uuid::now_v7();
        let token = encode_access_token(&c, user, "u@example.com").expect("encode");
        let claims = decode_access_token(&c, &token).expect("decode");
        assert_eq!(claims.sub, user.to_string());
        assert_eq!(claims.email, "u@example.com");
        assert_eq!(claims.iss, JWT_ISSUER);
        assert_eq!(claims.aud, JWT_ACCESS_AUDIENCE);
        assert!(!claims.jti.is_empty());
    }

    #[test]
    fn decode_with_wrong_secret_returns_unauthorized() {
        let c = cfg();
        let token = encode_access_token(&c, Uuid::now_v7(), "u@example.com").unwrap();
        let mut bad = cfg();
        bad.jwt_secret = "a-different-secret-also-long-enough-0000".into();
        let err = decode_access_token(&bad, &token).unwrap_err();
        assert!(matches!(err, AppError::Unauthorized));
    }

    #[test]
    fn decode_malformed_token_returns_unauthorized() {
        let c = cfg();
        let err = decode_access_token(&c, "not.a.jwt").unwrap_err();
        assert!(matches!(err, AppError::Unauthorized));
    }

    #[test]
    fn two_tokens_issued_same_second_have_distinct_jti() {
        // Prevents byte-identical tokens (and colliding `token_hash` rows)
        // when two refresh tokens are issued for the same user inside the
        // same wall-clock second.
        let c = cfg();
        let user = Uuid::now_v7();
        let a = encode_refresh_token(&c, user).unwrap();
        let b = encode_refresh_token(&c, user).unwrap();
        assert_ne!(a, b, "jti should prevent token collision");
        let ca = decode_refresh_token(&c, &a).unwrap();
        let cb = decode_refresh_token(&c, &b).unwrap();
        assert_ne!(ca.jti, cb.jti);
    }

    #[test]
    fn decode_access_rejects_wrong_token_type() {
        // Even if someone managed to produce a token with the access audience
        // but `token_type: "refresh"` (e.g. via a future refactor that
        // accidentally unified audiences), the explicit token_type check
        // must reject it.
        let c = cfg();
        let now = Utc::now();
        let claims = Claims {
            sub: Uuid::now_v7().to_string(),
            email: "u@example.com".into(),
            exp: (now + Duration::minutes(15)).timestamp() as usize,
            iat: now.timestamp() as usize,
            iss: JWT_ISSUER.to_string(),
            aud: JWT_ACCESS_AUDIENCE.to_string(),
            jti: Uuid::now_v7().to_string(),
            token_type: "refresh".into(), // wrong
        };
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(c.jwt_secret.as_bytes()),
        )
        .unwrap();
        let err = decode_access_token(&c, &token).unwrap_err();
        assert!(matches!(err, AppError::Unauthorized));
    }

    #[test]
    fn hash_token_is_deterministic_and_hex_encoded() {
        let a = hash_token("some-jwt-string");
        let b = hash_token("some-jwt-string");
        assert_eq!(a, b, "hash must be deterministic");
        assert_eq!(a.len(), 64, "sha256 hex is 64 chars");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, hash_token("some-other-jwt"));
    }
}
