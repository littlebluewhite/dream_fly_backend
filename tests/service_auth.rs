//! Integration tests for `auth::service`.
//!
//! Covered paths:
//! - register creates a row with an Argon2-hashed password
//! - register with a duplicate email returns Conflict with a generic message
//! - login with wrong password returns Unauthorized (no enumeration)
//! - login with nonexistent email also returns Unauthorized
//! - refresh rotates tokens and revokes the old one
//! - refresh token reuse detection revokes the entire token family
//! - forgot_password reissue invalidates the previous outstanding token
//! - reset_password tokens are single-use (GETDEL semantics)
//! - reset_password revokes the entire refresh-token family, not just the
//!   most recently issued token
//! - forgot_password's per-account rate limit silently swallows the 4th
//!   request within the window (still an Ok response, but no email sent)

mod common;

use std::sync::Arc;

use redis::AsyncCommands;
use sqlx::PgPool;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::auth::dto::{
    ForgotPasswordRequest, LoginRequest, RefreshRequest, RegisterRequest, ResetPasswordRequest,
};
use dream_fly_backend::modules::auth::service;
use dream_fly_backend::utils::email::EmailSender;
use dream_fly_backend::utils::jwt;

use common::mocks::MockEmailClient;

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

// ---------------- forgot_password / reset_password token protocol ----------------
//
// These 4 pin the reset-token protocol invariants at the `auth::service`
// boundary — deliberately independent of whether the issue/consume logic
// lives inline in `service.rs` or behind `reset_tokens::{issue,consume}`.

#[sqlx::test]
async fn forgot_password_reissue_invalidates_previous_token(db: PgPool) {
    let mut redis = common::test_redis().await;
    let background = TaskTracker::new();
    let email = format!("reissue-{}@example.com", Uuid::now_v7());
    let user_id = common::seed_member(&db, &email, "Password!234").await;

    let mock = Arc::new(MockEmailClient::new());
    let email_client: Arc<dyn EmailSender> = mock.clone();

    service::forgot_password(
        &db,
        &mut redis,
        email_client.clone(),
        &background,
        ForgotPasswordRequest {
            email: email.clone(),
        },
    )
    .await
    .expect("first forgot_password");

    service::forgot_password(
        &db,
        &mut redis,
        email_client.clone(),
        &background,
        ForgotPasswordRequest {
            email: email.clone(),
        },
    )
    .await
    .expect("second forgot_password (reissue)");

    background.close();
    background.wait().await;

    let sent = mock.sent();
    assert_eq!(sent.len(), 2, "both requests should send an email");
    let first_token = sent[0].token.clone();
    let second_token = sent[1].token.clone();
    assert_ne!(first_token, second_token);

    // Reissuing invalidates the previous token — only the newest one is live.
    let first_exists: bool = redis
        .exists(format!("password_reset:{first_token}"))
        .await
        .expect("check first token key");
    assert!(
        !first_exists,
        "previous token must be invalidated on reissue"
    );

    let second_exists: bool = redis
        .exists(format!("password_reset:{second_token}"))
        .await
        .expect("check second token key");
    assert!(second_exists, "newest token must still be live");

    let index_value: Option<String> = redis
        .get(format!("password_reset_current:{user_id}"))
        .await
        .expect("read index key");
    assert_eq!(index_value.as_deref(), Some(second_token.as_str()));
}

#[sqlx::test]
async fn reset_password_token_is_single_use(db: PgPool) {
    let mut redis = common::test_redis().await;
    let background = TaskTracker::new();
    let email = format!("singleuse-{}@example.com", Uuid::now_v7());
    common::seed_member(&db, &email, "Password!234").await;

    let mock = Arc::new(MockEmailClient::new());
    let email_client: Arc<dyn EmailSender> = mock.clone();

    service::forgot_password(
        &db,
        &mut redis,
        email_client,
        &background,
        ForgotPasswordRequest {
            email: email.clone(),
        },
    )
    .await
    .expect("forgot_password");

    background.close();
    background.wait().await;

    let token = mock.sent()[0].token.clone();

    service::reset_password(
        &db,
        &mut redis,
        ResetPasswordRequest {
            token: token.clone(),
            new_password: "NewPassword!234".into(),
        },
    )
    .await
    .expect("first reset_password consumes the token");

    // GETDEL already deleted the key — a second attempt with the same token
    // must fail, not silently succeed again.
    let err = service::reset_password(
        &db,
        &mut redis,
        ResetPasswordRequest {
            token,
            new_password: "AnotherPassword!234".into(),
        },
    )
    .await
    .expect_err("token must be single-use");
    assert!(matches!(err, AppError::BadRequest(_)), "got: {err:?}");
}

#[sqlx::test]
async fn reset_password_revokes_entire_refresh_family(db: PgPool) {
    let cfg = common::test_auth_config();
    let mut redis = common::test_redis().await;
    let background = TaskTracker::new();
    let email = format!("family-{}@example.com", Uuid::now_v7());

    let r1 = service::register(
        &db,
        &mut redis,
        &cfg,
        RegisterRequest {
            email: email.clone(),
            name: "Family Test".into(),
            password: "Password!234".into(),
        },
        None,
    )
    .await
    .expect("register");

    // Rotate once so the family has more than one member — proves the
    // whole family is revoked, not just the most recently issued token.
    let r2 = service::refresh_token(
        &db,
        &cfg,
        RefreshRequest {
            refresh_token: r1.refresh_token.clone(),
        },
    )
    .await
    .expect("rotate refresh token");

    let mock = Arc::new(MockEmailClient::new());
    let email_client: Arc<dyn EmailSender> = mock.clone();
    service::forgot_password(
        &db,
        &mut redis,
        email_client,
        &background,
        ForgotPasswordRequest {
            email: email.clone(),
        },
    )
    .await
    .expect("forgot_password");

    background.close();
    background.wait().await;
    let token = mock.sent()[0].token.clone();

    service::reset_password(
        &db,
        &mut redis,
        ResetPasswordRequest {
            token,
            new_password: "BrandNewPassword!234".into(),
        },
    )
    .await
    .expect("reset_password");

    // r2 — the live token at the moment of reset — must now be dead too, not
    // just whichever token happened to be current when the password changed.
    // (Reusing r1 directly would itself trip reuse-detection family-wipe, so
    // it is deliberately not replayed here — the DB-level count below is the
    // unconfounded proof that r1's row is also revoked.)
    let err = service::refresh_token(
        &db,
        &cfg,
        RefreshRequest {
            refresh_token: r2.refresh_token.clone(),
        },
    )
    .await
    .expect_err("entire family should be revoked by reset_password");
    assert!(matches!(err, AppError::Unauthorized));

    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM refresh_tokens WHERE user_id = $1 AND revoked = false",
    )
    .bind(r1.user.id)
    .fetch_one(&db)
    .await
    .expect("count active tokens");
    assert_eq!(
        active_count, 0,
        "all tokens (including r1's already-rotated-away row) should be revoked"
    );
}

#[sqlx::test]
async fn forgot_password_rate_limit_swallows_fourth_request_silently(db: PgPool) {
    let mut redis = common::test_redis().await;
    let background = TaskTracker::new();
    let email = format!("ratelimit-{}@example.com", Uuid::now_v7());
    common::seed_member(&db, &email, "Password!234").await;

    let mock = Arc::new(MockEmailClient::new());
    let email_client: Arc<dyn EmailSender> = mock.clone();

    for i in 1..=4 {
        service::forgot_password(
            &db,
            &mut redis,
            email_client.clone(),
            &background,
            ForgotPasswordRequest {
                email: email.clone(),
            },
        )
        .await
        .unwrap_or_else(|e| {
            panic!("request {i} should still return an Ok (200-equivalent): {e:?}")
        });
    }

    background.close();
    background.wait().await;

    assert_eq!(
        mock.sent().len(),
        3,
        "the 4th request must be swallowed silently — no 4th email sent"
    );
}
