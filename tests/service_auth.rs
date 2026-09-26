//! Integration tests for `auth::service`.
//!
//! Covered paths:
//! - register creates a row with an Argon2-hashed password
//! - register with a duplicate email returns Conflict with a generic message
//! - login with wrong password returns Unauthorized (no enumeration)
//! - login with nonexistent email also returns Unauthorized
//! - refresh rotates tokens and revokes the old one
//! - refresh token reuse detection revokes the entire token family
//! - `session::purge_expired` keeps rotated-out (revoked, unexpired) rows,
//!   so reuse detection still fires after a purge
//! - `session::purge_expired` deletes expired rows, revoked or not
//! - forgot_password reissue invalidates the previous outstanding token
//! - reset_password tokens are single-use (GETDEL semantics)
//! - reset_password revokes the entire refresh-token family, not just the
//!   most recently issued token
//! - a stale pre-reset token is a plain 401 and does not kill the session
//!   the user logged in with afterwards
//! - forgot_password's per-account rate limit silently swallows the 4th
//!   request within the window (still an Ok response, but no email sent)

mod common;

use std::sync::Arc;

use redis::AsyncCommands;
use sqlx::PgPool;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::auth::access;
use dream_fly_backend::modules::auth::dto::{
    ForgotPasswordRequest, LoginRequest, RefreshRequest, RegisterRequest, ResetPasswordRequest,
};
use dream_fly_backend::modules::auth::service;
use dream_fly_backend::modules::auth::session;
use dream_fly_backend::utils::email::EmailSender;
use dream_fly_backend::utils::jwt;

use common::mocks::MockEmailClient;

#[sqlx::test]
async fn register_creates_user_with_hashed_password(db: PgPool) {
    let cfg = common::test_auth_config();

    let resp = service::register(
        &db,
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

    service::register(
        &db,
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

    let r1 = service::register(
        &db,
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

    let r1 = service::register(
        &db,
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

/// Hole 3: the hourly purge used to delete revoked rows too, so once it ran,
/// replaying a rotated-out token found no row at all — a plain 401 with no
/// family revoke, silently disabling reuse detection. Purge now deletes only
/// expired rows; a revoked-but-unexpired row must survive so the replay is
/// still recognised.
#[sqlx::test]
async fn purge_expired_keeps_rotated_tokens_so_reuse_still_revokes_family(db: PgPool) {
    let cfg = common::test_auth_config();

    let r1 = service::register(
        &db,
        &cfg,
        RegisterRequest {
            email: "purge@example.com".into(),
            name: "Purge".into(),
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

    session::purge_expired(&db).await.expect("purge");

    // Replay the rotated-out r1: still recognised as reuse …
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

    // … so the whole family, including the live r2, is revoked.
    let r2_err = service::refresh_token(
        &db,
        &cfg,
        RefreshRequest {
            refresh_token: r2.refresh_token.clone(),
        },
    )
    .await
    .expect_err("family should be dead after reuse");
    assert!(matches!(r2_err, AppError::Unauthorized));
}

/// The other half of the retention rule: once a row is past `expires_at`,
/// `purge_expired` deletes it whether or not it was revoked, and leaves
/// unexpired rows alone.
#[sqlx::test]
async fn purge_expired_deletes_expired_rows_revoked_or_not(db: PgPool) {
    let cfg = common::test_auth_config();

    let r1 = service::register(
        &db,
        &cfg,
        RegisterRequest {
            email: "purge-expired@example.com".into(),
            name: "Purge Expired".into(),
            password: "sup3rsecret".into(),
        },
        None,
    )
    .await
    .expect("register");

    // Rotate once (r1 becomes revoked) and once more (r2 revoked, r3 live).
    let r2 = service::refresh_token(
        &db,
        &cfg,
        RefreshRequest {
            refresh_token: r1.refresh_token.clone(),
        },
    )
    .await
    .expect("first refresh");
    let r3 = service::refresh_token(
        &db,
        &cfg,
        RefreshRequest {
            refresh_token: r2.refresh_token.clone(),
        },
    )
    .await
    .expect("second refresh");

    // Backdate r1 (revoked) and r3 (live); r2 (revoked) stays unexpired.
    for token in [&r1.refresh_token, &r3.refresh_token] {
        sqlx::query(
            "UPDATE refresh_tokens SET expires_at = NOW() - INTERVAL '1 minute' WHERE token_hash = $1",
        )
        .bind(jwt::hash_token(token))
        .execute(&db)
        .await
        .expect("backdate expires_at");
    }

    let purged = session::purge_expired(&db).await.expect("purge");
    assert_eq!(purged, 2, "both expired rows, revoked and live, are deleted");

    let remaining: Vec<String> =
        sqlx::query_scalar("SELECT token_hash FROM refresh_tokens WHERE user_id = $1")
            .bind(r1.user.id)
            .fetch_all(&db)
            .await
            .expect("read remaining rows");
    assert_eq!(remaining, vec![jwt::hash_token(&r2.refresh_token)]);
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
    // (r1 is deliberately not replayed here — the DB-level count below is
    // the direct proof that r1's already-rotated row is gone too.)
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

/// `end_all` used to mark rows revoked, and revoked rows are retained until
/// expiry for reuse detection — so a stale device refreshing with a
/// pre-reset token after the user logged in again looked like a replay:
/// `rotate` ran `end_all` again, killing the brand-new session and logging a
/// false `refresh_token_reuse`. `end_all` now deletes the rows, so the stale
/// token is a plain 401 and the new session survives.
#[sqlx::test]
async fn stale_token_after_reset_password_does_not_kill_new_session(db: PgPool) {
    let cfg = common::test_auth_config();
    let mut redis = common::test_redis().await;
    let background = TaskTracker::new();
    let email = format!("stale-{}@example.com", Uuid::now_v7());

    let old = service::register(
        &db,
        &cfg,
        RegisterRequest {
            email: email.clone(),
            name: "Stale Device".into(),
            password: "Password!234".into(),
        },
        None,
    )
    .await
    .expect("register");

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

    let fresh = service::login(
        &db,
        &mut redis,
        &cfg,
        LoginRequest {
            email: email.clone(),
            password: "BrandNewPassword!234".into(),
        },
    )
    .await
    .expect("login with the new password");

    // The stale device refreshes with its pre-reset token: rejected …
    let err = service::refresh_token(
        &db,
        &cfg,
        RefreshRequest {
            refresh_token: old.refresh_token.clone(),
        },
    )
    .await
    .expect_err("pre-reset token must be dead");
    assert!(matches!(err, AppError::Unauthorized), "got: {err:?}");

    // … without taking the new session down with it.
    service::refresh_token(
        &db,
        &cfg,
        RefreshRequest {
            refresh_token: fresh.refresh_token.clone(),
        },
    )
    .await
    .expect("new session's refresh token must still work");
}

/// A refresh racing an admin deactivation must not mint a session that
/// outlives it. `rotate` takes the user row `FOR SHARE` before touching the
/// token (user-first, the same order as deactivation and `reset_password`),
/// so it queues behind the deactivation's `UPDATE users` and then sees
/// `is_active = false`. It used to read the user unlocked: it could issue a
/// new pair from the pre-deactivation snapshot, and the new row could slip
/// past the deactivation's `DELETE` snapshot and stay live.
#[sqlx::test]
async fn refresh_waits_for_in_flight_deactivation(db: PgPool) {
    let cfg = common::test_auth_config();
    let cache = access::RedisAccessCache::new(common::test_redis().await);

    let r1 = service::register(
        &db,
        &cfg,
        RegisterRequest {
            email: "deactivate-race@example.com".into(),
            name: "Deactivate Race".into(),
            password: "sup3rsecret".into(),
        },
        None,
    )
    .await
    .expect("register");

    // The admin's deactivation, held open: user row updated and sessions
    // ended, not committed.
    let mut admin_tx = db.begin().await.expect("begin admin tx");
    let dirty = access::deactivate_tx(&mut admin_tx, r1.user.id)
        .await
        .expect("deactivate");

    let refresh = tokio::spawn({
        let db = db.clone();
        let cfg = cfg.clone();
        let refresh_token = r1.refresh_token.clone();
        async move { service::refresh_token(&db, &cfg, RefreshRequest { refresh_token }).await }
    });

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        !refresh.is_finished(),
        "refresh must wait for the in-flight deactivation"
    );

    admin_tx.commit().await.expect("commit deactivation");
    dirty.flush(&cache).await;

    let err = refresh
        .await
        .expect("join refresh")
        .expect_err("refresh must fail once the deactivation lands");
    assert!(matches!(err, AppError::Unauthorized), "got: {err:?}");

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM refresh_tokens WHERE user_id = $1")
        .bind(r1.user.id)
        .fetch_one(&db)
        .await
        .expect("count refresh_tokens");
    assert_eq!(rows, 0, "no refresh token may outlive the deactivation");
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
