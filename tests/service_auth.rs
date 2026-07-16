//! Integration tests for `auth::service`.
//!
//! Covered paths:
//! - register creates a row with an Argon2-hashed password
//! - register with a duplicate email returns Conflict with a generic message
//! - login with wrong password returns Unauthorized (no enumeration)
//! - login with nonexistent email also returns Unauthorized
//! - refresh rotates tokens and revokes the old one
//! - refresh token reuse detection revokes the entire token family

mod common;

use sqlx::PgPool;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::auth::dto::{LoginRequest, RefreshRequest, RegisterRequest};
use dream_fly_backend::modules::auth::service;
use dream_fly_backend::utils::jwt;

#[sqlx::test]
async fn register_creates_user_with_hashed_password(db: PgPool) {
    let cfg = common::test_auth_config();
    let mut redis = common::test_redis().await;

    let resp = service::register(
        &db,
        &mut redis,
        &cfg,
        RegisterRequest {
            email: "alice@example.com".into(),
            name: "Alice".into(),
            password: "sup3rsecret".into(),
        },
        None,
    )
    .await
    .expect("register");

    // Password is stored as an Argon2 hash, not plaintext.
    let stored_hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
        .bind(resp.user.id)
        .fetch_one(&db)
        .await
        .expect("read user");
    assert!(
        stored_hash.starts_with("$argon2"),
        "password hash must be argon2"
    );
    assert_ne!(stored_hash, "sup3rsecret");

    // Refresh token is stored as a SHA-256 hash (hex, 64 chars), not the raw JWT.
    let stored_token_hash: String =
        sqlx::query_scalar("SELECT token_hash FROM refresh_tokens WHERE user_id = $1")
            .bind(resp.user.id)
            .fetch_one(&db)
            .await
            .expect("read refresh_token row");
    assert_eq!(stored_token_hash.len(), 64);
    assert_ne!(stored_token_hash, resp.refresh_token);
    assert_eq!(stored_token_hash, jwt::hash_token(&resp.refresh_token));

    // Email is normalized to lowercase even if the input was mixed case.
    let stored_email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(resp.user.id)
        .fetch_one(&db)
        .await
        .expect("read email");
    assert_eq!(stored_email, "alice@example.com");

    // A welcome notification is written synchronously post-commit.
    let (notif_type, title): (String, String) =
        sqlx::query_as("SELECT type::text, title FROM notifications WHERE user_id = $1")
            .bind(resp.user.id)
            .fetch_one(&db)
            .await
            .expect("welcome notification row");
    assert_eq!(notif_type, "system");
    assert_eq!(title, "Welcome to Dream Fly");
}

#[sqlx::test]
async fn register_duplicate_email_returns_conflict(db: PgPool) {
    let cfg = common::test_auth_config();
    let mut redis = common::test_redis().await;

    service::register(
        &db,
        &mut redis,
        &cfg,
        RegisterRequest {
            email: "bob@example.com".into(),
            name: "Bob".into(),
            password: "passw0rd!".into(),
        },
        None,
    )
    .await
    .expect("first register");

    let err = service::register(
        &db,
        &mut redis,
        &cfg,
        RegisterRequest {
            // Different case, same email — lowercase normalization must still
            // trigger the unique constraint.
            email: "BOB@example.com".into(),
            name: "Other Bob".into(),
            password: "passw0rd!".into(),
        },
        None,
    )
    .await
    .expect_err("second register should fail");

    assert!(matches!(err, AppError::Conflict(_)), "got: {err:?}");
}

#[sqlx::test]
async fn login_wrong_password_returns_unauthorized(db: PgPool) {
    let cfg = common::test_auth_config();
    let mut redis = common::test_redis().await;
    common::seed_member(&db, "carol@example.com", "correct-password").await;

    let err = service::login(
        &db,
        &mut redis,
        &cfg,
        LoginRequest {
            email: "carol@example.com".into(),
            password: "wrong-password".into(),
        },
    )
    .await
    .expect_err("login with wrong password");

    assert!(matches!(err, AppError::Unauthorized), "got: {err:?}");
}

#[sqlx::test]
async fn login_nonexistent_email_returns_unauthorized(db: PgPool) {
    let cfg = common::test_auth_config();
    let mut redis = common::test_redis().await;

    // Exactly the same error shape as wrong-password — prevents enumeration.
    let err = service::login(
        &db,
        &mut redis,
        &cfg,
        LoginRequest {
            email: "nobody@example.com".into(),
            password: "anything".into(),
        },
    )
    .await
    .expect_err("login with unknown email");

    assert!(matches!(err, AppError::Unauthorized), "got: {err:?}");
}

#[sqlx::test]
async fn refresh_token_rotates_and_revokes_old(db: PgPool) {
    let cfg = common::test_auth_config();
    let mut redis = common::test_redis().await;

    let r1 = service::register(
        &db,
        &mut redis,
        &cfg,
        RegisterRequest {
            email: "dave@example.com".into(),
            name: "Dave".into(),
            password: "sup3rsecret".into(),
        },
        None,
    )
    .await
    .expect("register");

    let r2 = service::refresh_token(
        &db,
        &cfg,
        RefreshRequest {
            refresh_token: r1.refresh_token.clone(),
        },
    )
    .await
    .expect("first refresh");

    // New tokens issued
    assert_ne!(r2.refresh_token, r1.refresh_token);
    assert_ne!(r2.access_token, r1.access_token);
    assert_eq!(r2.user.id, r1.user.id);

    // Old token row is now marked revoked
    let old_revoked: bool =
        sqlx::query_scalar("SELECT revoked FROM refresh_tokens WHERE token_hash = $1")
            .bind(jwt::hash_token(&r1.refresh_token))
            .fetch_one(&db)
            .await
            .expect("fetch old token row");
    assert!(old_revoked, "old refresh token should be revoked");

    // New token works
    let _r3 = service::refresh_token(
        &db,
        &cfg,
        RefreshRequest {
            refresh_token: r2.refresh_token.clone(),
        },
    )
    .await
    .expect("second refresh");
}

#[sqlx::test]
async fn refresh_token_reuse_revokes_entire_family(db: PgPool) {
    let cfg = common::test_auth_config();
    let mut redis = common::test_redis().await;

    let r1 = service::register(
        &db,
        &mut redis,
        &cfg,
        RegisterRequest {
            email: "eve@example.com".into(),
            name: "Eve".into(),
            password: "sup3rsecret".into(),
        },
        None,
    )
    .await
    .expect("register");

    let r2 = service::refresh_token(
        &db,
        &cfg,
        RefreshRequest {
            refresh_token: r1.refresh_token.clone(),
        },
    )
    .await
    .expect("first refresh");

    // Replay r1 — it's revoked, so this must fail AND kill the whole family.
    let reuse_err = service::refresh_token(
        &db,
        &cfg,
        RefreshRequest {
            refresh_token: r1.refresh_token.clone(),
        },
    )
    .await
    .expect_err("reuse should fail");
    assert!(matches!(reuse_err, AppError::Unauthorized));

    // Now r2 must also be dead, because reuse detection revoked every token
    // belonging to the user.
    let r2_err = service::refresh_token(
        &db,
        &cfg,
        RefreshRequest {
            refresh_token: r2.refresh_token.clone(),
        },
    )
    .await
    .expect_err("family should be dead");
    assert!(matches!(r2_err, AppError::Unauthorized));

    // Double-check at the DB level: every token row for this user is revoked.
    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM refresh_tokens WHERE user_id = $1 AND revoked = false",
    )
    .bind(r1.user.id)
    .fetch_one(&db)
    .await
    .expect("count active tokens");
    assert_eq!(active_count, 0, "all tokens should be revoked");
}
