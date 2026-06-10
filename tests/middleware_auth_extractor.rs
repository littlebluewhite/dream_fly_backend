//! Integration tests for the `AuthUser` extractor in `src/extractors/auth.rs`.
//!
//! The extractor sits between every protected handler and the router, so
//! driving it through `/users/me` exercises the full chain (JWT parsing +
//! signature check + is_active cache + role cache + DB fallback).

mod common;

use chrono::{Duration, Utc};
use common::http::spawn_test_app;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::utils::jwt::{Claims, JWT_ACCESS_AUDIENCE, JWT_ISSUER};

#[sqlx::test]
async fn no_authorization_header_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/users/me").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn malformed_authorization_header_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .get("/api/v1/users/me")
        .add_header("authorization", "NotBearer foo")
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn garbage_token_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .get("/api/v1/users/me")
        .authorization_bearer("totally.not.a.jwt")
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn token_signed_with_wrong_secret_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    // Hand-craft a JWT signed with a DIFFERENT secret.
    let now = Utc::now();
    let claims = Claims {
        sub: Uuid::now_v7().to_string(),
        email: "x@y.com".into(),
        exp: (now + Duration::minutes(15)).timestamp() as usize,
        iat: now.timestamp() as usize,
        iss: JWT_ISSUER.into(),
        aud: JWT_ACCESS_AUDIENCE.into(),
        jti: Uuid::now_v7().to_string(),
        token_type: "access".to_string(),
    };
    let wrong = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(b"this-is-not-the-test-secret-32ch"),
    )
    .unwrap();

    let resp = app
        .get("/api/v1/users/me")
        .authorization_bearer(&wrong)
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn expired_token_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let now = Utc::now();
    let claims = Claims {
        sub: Uuid::now_v7().to_string(),
        email: "x@y.com".into(),
        // Expired 1 hour ago — well outside the 5s leeway.
        exp: (now - Duration::hours(1)).timestamp() as usize,
        iat: (now - Duration::hours(2)).timestamp() as usize,
        iss: JWT_ISSUER.into(),
        aud: JWT_ACCESS_AUDIENCE.into(),
        jti: Uuid::now_v7().to_string(),
        token_type: "access".to_string(),
    };
    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(app.config.auth.jwt_secret.as_bytes()),
    )
    .unwrap();

    let resp = app
        .get("/api/v1/users/me")
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn valid_token_for_unknown_user_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    // Sign a token for a user that does not exist in the users table.
    // The extractor's is_active check should fail and return 401.
    let now = Utc::now();
    let ghost = Uuid::now_v7();
    let claims = Claims {
        sub: ghost.to_string(),
        email: "ghost@example.com".into(),
        exp: (now + Duration::minutes(15)).timestamp() as usize,
        iat: now.timestamp() as usize,
        iss: JWT_ISSUER.into(),
        aud: JWT_ACCESS_AUDIENCE.into(),
        jti: Uuid::now_v7().to_string(),
        token_type: "access".to_string(),
    };
    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(app.config.auth.jwt_secret.as_bytes()),
    )
    .unwrap();

    let resp = app
        .get("/api/v1/users/me")
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn valid_token_for_deactivated_user_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("deact@example.com", "Password!234").await;

    // Flip is_active to false and clear the Redis active cache.
    sqlx::query("UPDATE users SET is_active = false WHERE id = $1")
        .bind(user.user_id)
        .execute(&app.db)
        .await
        .unwrap();
    let mut r = app.redis_conn().await;
    let _: Result<(), _> = redis::AsyncCommands::del::<_, ()>(
        &mut r,
        format!("user_active:{}", user.user_id),
    )
    .await;

    let resp = app
        .get("/api/v1/users/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn member_token_against_admin_endpoint_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("m@example.com", "Password!234").await;
    let resp = app
        .get("/api/v1/users")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403);
}
