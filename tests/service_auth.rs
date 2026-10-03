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
//! - login locks an email out after 10 failures (even the right password is
//!   then refused), a success clears the count, and the lockout is per email
//! - OTP: the 4th send within the hour is refused (3 SMS total), and the 6th
//!   verify attempt invalidates the code until a fresh one is sent
//! - short-lived state store down (`FailingEphemeralStore`): login and the
//!   forgot-password counter fail open; OTP and reset tokens fail closed
//!   (so forgot-password for a known email is a 500, an unknown one a 200)
//! - google_auth login rules, with `FakeGoogleIdentity` standing in for
//!   Google (the real adapter is covered in `tests/google_identity.rs`):
//!   welcome on Create only, Link keeps roles and doesn't resend welcome,
//!   inactive accounts refused with zero writes, Refresh doesn't regrant
//!   `member`, an email bound to another Google account is a Conflict, and a
//!   provider rejection writes nothing

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::PgPool;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::auth::access;
use dream_fly_backend::modules::auth::dto::{
    AuthResponse, ForgotPasswordRequest, GoogleAuthRequest, LoginRequest, OtpSendRequest,
    OtpVerifyRequest, RefreshRequest, RegisterRequest, ResetPasswordRequest,
};
use dream_fly_backend::modules::auth::repository;
use dream_fly_backend::modules::auth::service;
use dream_fly_backend::modules::auth::session;
use dream_fly_backend::modules::permissions::model::Role;
use dream_fly_backend::modules::permissions::repository as permissions_repository;
use dream_fly_backend::utils::email::EmailSender;
use dream_fly_backend::utils::ephemeral::EphemeralStore;
use dream_fly_backend::utils::jwt;
use dream_fly_backend::utils::sms::SmsClient;
use wiremock::MockServer;

use common::mocks::{
    FailingEphemeralStore, FakeGoogleIdentity, InMemoryAccessCache, InMemoryEphemeralStore,
    MockEmailClient,
};
use common::twilio::{extract_otp_code, mount_twilio, twilio_sent};

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
    let store = InMemoryEphemeralStore::new();
    common::seed_member(&db, "carol@example.com", "correct-password").await;

    let err = service::login(
        &db,
        &store,
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
    let store = InMemoryEphemeralStore::new();

    // Exactly the same error shape as wrong-password — prevents enumeration.
    let err = service::login(
        &db,
        &store,
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
    let store = InMemoryEphemeralStore::new();
    let background = TaskTracker::new();
    let email = format!("reissue-{}@example.com", Uuid::now_v7());
    common::seed_member(&db, &email, "Password!234").await;

    let mock = Arc::new(MockEmailClient::new());
    let email_client: Arc<dyn EmailSender> = mock.clone();

    service::forgot_password(
        &db,
        &store,
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
        &store,
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
    let err = service::reset_password(
        &db,
        &store,
        ResetPasswordRequest {
            token: first_token,
            new_password: "NewPassword!234".into(),
        },
    )
    .await
    .expect_err("previous token must be invalidated on reissue");
    assert!(
        matches!(&err, AppError::BadRequest(m) if m == "invalid or expired token"),
        "got: {err:?}"
    );

    service::reset_password(
        &db,
        &store,
        ResetPasswordRequest {
            token: second_token,
            new_password: "NewPassword!234".into(),
        },
    )
    .await
    .expect("newest token must still be live");
}

#[sqlx::test]
async fn reset_password_token_is_single_use(db: PgPool) {
    let store = InMemoryEphemeralStore::new();
    let background = TaskTracker::new();
    let email = format!("singleuse-{}@example.com", Uuid::now_v7());
    common::seed_member(&db, &email, "Password!234").await;

    let mock = Arc::new(MockEmailClient::new());
    let email_client: Arc<dyn EmailSender> = mock.clone();

    service::forgot_password(
        &db,
        &store,
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
        &store,
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
        &store,
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
    let store = InMemoryEphemeralStore::new();
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
        &store,
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
        &store,
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
    let store = InMemoryEphemeralStore::new();
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
        &store,
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
        &store,
        ResetPasswordRequest {
            token,
            new_password: "BrandNewPassword!234".into(),
        },
    )
    .await
    .expect("reset_password");

    let fresh = service::login(
        &db,
        &store,
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
    let cache = InMemoryAccessCache::new();

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
    let store = InMemoryEphemeralStore::new();
    let background = TaskTracker::new();
    let email = format!("ratelimit-{}@example.com", Uuid::now_v7());
    common::seed_member(&db, &email, "Password!234").await;

    let mock = Arc::new(MockEmailClient::new());
    let email_client: Arc<dyn EmailSender> = mock.clone();

    for i in 1..=4 {
        service::forgot_password(
            &db,
            &store,
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

// ---------------- login lockout (per email) ----------------

async fn login_as(
    db: &PgPool,
    store: &dyn EphemeralStore,
    email: &str,
    password: &str,
) -> Result<AuthResponse, AppError> {
    service::login(
        db,
        store,
        &common::test_auth_config(),
        LoginRequest {
            email: email.into(),
            password: password.into(),
        },
    )
    .await
}

/// `n` wrong-password logins, each refused with the plain 401.
async fn fail_logins(db: &PgPool, store: &dyn EphemeralStore, email: &str, n: u32) {
    for i in 1..=n {
        let err = login_as(db, store, email, "wrong-password")
            .await
            .expect_err("wrong password must fail");
        assert!(
            matches!(err, AppError::Unauthorized),
            "failure {i}: {err:?}"
        );
    }
}

#[sqlx::test]
async fn login_locks_out_after_ten_failures_even_with_correct_password(db: PgPool) {
    let store = InMemoryEphemeralStore::new();
    let email = format!("lockout-{}@example.com", Uuid::now_v7());
    common::seed_member(&db, &email, "Password!234").await;

    fail_logins(&db, &store, &email, 10).await;

    // Same 401 as bad credentials — the lockout is not revealed.
    let err = login_as(&db, &store, &email, "Password!234")
        .await
        .expect_err("locked-out email must be refused even with the right password");
    assert!(matches!(err, AppError::Unauthorized), "got: {err:?}");
}

#[sqlx::test]
async fn login_success_clears_failure_count(db: PgPool) {
    let store = InMemoryEphemeralStore::new();
    let email = format!("clear-{}@example.com", Uuid::now_v7());
    common::seed_member(&db, &email, "Password!234").await;

    fail_logins(&db, &store, &email, 9).await;
    login_as(&db, &store, &email, "Password!234")
        .await
        .expect("9 failures do not lock out");

    // Without the clear these 9 would make 18 and lock the account.
    fail_logins(&db, &store, &email, 9).await;
    login_as(&db, &store, &email, "Password!234")
        .await
        .expect("the earlier success reset the count");
}

#[sqlx::test]
async fn login_lockout_is_per_email(db: PgPool) {
    let store = InMemoryEphemeralStore::new();
    let locked = format!("locked-{}@example.com", Uuid::now_v7());
    let other = format!("other-{}@example.com", Uuid::now_v7());
    common::seed_member(&db, &locked, "Password!234").await;
    common::seed_member(&db, &other, "Password!234").await;

    fail_logins(&db, &store, &locked, 10).await;

    login_as(&db, &store, &other, "Password!234")
        .await
        .expect("another email is not affected by the lockout");
}

// ---------------- OTP send/verify limits (Twilio via wiremock) ----------------

const OTP_PHONE: &str = "+15551234567";

/// A real `SmsClient` pointed at a `wiremock` Twilio stub.
async fn twilio_sms() -> (MockServer, SmsClient) {
    let server = MockServer::start().await;
    mount_twilio(&server).await;
    let config = common::http::test_app_config(|cfg| cfg.sms.twilio_base_url = server.uri());
    let sms = SmsClient::new(&config.sms, reqwest::Client::new());
    (server, sms)
}

async fn send_otp(
    store: &dyn EphemeralStore,
    sms: &SmsClient,
    user_id: Uuid,
) -> Result<(), AppError> {
    service::send_otp(
        store,
        sms,
        user_id,
        OtpSendRequest {
            phone: OTP_PHONE.into(),
        },
    )
    .await
    .map(|_| ())
}

async fn verify_otp(
    db: &PgPool,
    store: &dyn EphemeralStore,
    user_id: Uuid,
    code: &str,
) -> Result<(), AppError> {
    service::verify_otp(
        db,
        store,
        user_id,
        OtpVerifyRequest {
            phone: OTP_PHONE.into(),
            code: code.into(),
        },
    )
    .await
    .map(|_| ())
}

/// The code in the most recent SMS the stub received.
async fn last_otp_code(server: &MockServer) -> String {
    let sent = twilio_sent(server).await;
    extract_otp_code(&sent.last().expect("otp sms sent").body).expect("otp code")
}

fn assert_bad_request(err: &AppError, message: &str) {
    assert!(
        matches!(err, AppError::BadRequest(m) if m == message),
        "expected 400 {message:?}, got: {err:?}"
    );
}

#[sqlx::test]
async fn otp_send_fourth_request_within_hour_is_rejected(db: PgPool) {
    let store = InMemoryEphemeralStore::new();
    let (server, sms) = twilio_sms().await;
    let email = format!("otp-rate-{}@example.com", Uuid::now_v7());
    let user_id = common::seed_member(&db, &email, "Password!234").await;

    for i in 1..=3 {
        send_otp(&store, &sms, user_id)
            .await
            .unwrap_or_else(|e| panic!("send {i} is within the hourly limit: {e:?}"));
    }

    let err = send_otp(&store, &sms, user_id)
        .await
        .expect_err("4th send within the hour must be refused");
    assert_bad_request(&err, "too many verification requests, try again later");

    assert_eq!(
        twilio_sent(&server).await.len(),
        3,
        "no SMS for the 4th request"
    );
}

#[sqlx::test]
async fn otp_verify_sixth_attempt_invalidates_code_until_resend(db: PgPool) {
    let store = InMemoryEphemeralStore::new();
    let (server, sms) = twilio_sms().await;
    let email = format!("otp-attempts-{}@example.com", Uuid::now_v7());
    let user_id = common::seed_member(&db, &email, "Password!234").await;

    send_otp(&store, &sms, user_id).await.expect("send");
    let code = last_otp_code(&server).await;

    // Codes are 100000..=999999, so "000000" is always wrong.
    for _ in 1..=5 {
        let err = verify_otp(&db, &store, user_id, "000000")
            .await
            .expect_err("wrong code");
        assert_bad_request(&err, "invalid verification code");
    }

    // The 6th attempt is refused even with the right code, and kills it.
    let err = verify_otp(&db, &store, user_id, &code)
        .await
        .expect_err("6th attempt must be refused");
    assert_bad_request(&err, "too many attempts, request a new code");

    // A fresh send resets the attempt count; its new code verifies.
    send_otp(&store, &sms, user_id).await.expect("resend");
    let new_code = last_otp_code(&server).await;
    verify_otp(&db, &store, user_id, &new_code)
        .await
        .expect("fresh code verifies after resend");

    let verified: bool = sqlx::query_scalar("SELECT phone_verified FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&db)
        .await
        .expect("read phone_verified");
    assert!(verified);
}

#[sqlx::test]
async fn verify_otp_conflict_keeps_code_usable(db: PgPool) {
    let store = InMemoryEphemeralStore::new();
    let (server, sms) = twilio_sms().await;
    let owner = common::seed_member(&db, &format!("otp-owner-{}@example.com", Uuid::now_v7()), "Password!234").await;
    let user_id = common::seed_member(&db, &format!("otp-conflict-{}@example.com", Uuid::now_v7()), "Password!234").await;

    // Another user already holds the verified phone.
    sqlx::query("UPDATE users SET phone = $2, phone_verified = true WHERE id = $1")
        .bind(owner)
        .bind(OTP_PHONE)
        .execute(&db)
        .await
        .expect("seed verified phone");

    send_otp(&store, &sms, user_id).await.expect("send");
    let code = last_otp_code(&server).await;

    let err = verify_otp(&db, &store, user_id, &code)
        .await
        .expect_err("phone already verified by another user");
    // The unique-violation maps to 409 at the HTTP boundary.
    assert!(
        matches!(&err, AppError::Database(e) if e.as_database_error().is_some_and(|d| d.is_unique_violation())),
        "expected unique violation, got: {err:?}"
    );

    // The DB write failed, so the code was not burned — and the failed attempt
    // stays counted (retained, not reset): free the phone, retry.
    let attempts_key = format!("otp_attempts:{user_id}");
    assert_eq!(
        store.get(&attempts_key).await.expect("read attempts").as_deref(),
        Some("1"),
        "the 409 must keep the attempt count, not reset it"
    );
    sqlx::query("UPDATE users SET phone_verified = false WHERE id = $1")
        .bind(owner)
        .execute(&db)
        .await
        .expect("free phone");
    verify_otp(&db, &store, user_id, &code)
        .await
        .expect("same code still verifies after the conflict is gone");
}

// ------- short-lived state store down (`FailingEphemeralStore`) -------

fn assert_internal(err: &AppError) {
    assert!(
        matches!(err, AppError::Internal(_)),
        "expected 500, got: {err:?}"
    );
}

/// Login fails open: the lockout check reads as "not locked" and failure
/// counting/clearing are skipped, so a store outage locks nobody out.
#[sqlx::test]
async fn login_fails_open_when_store_is_down(db: PgPool) {
    let email = format!("down-login-{}@example.com", Uuid::now_v7());
    common::seed_member(&db, &email, "Password!234").await;

    fail_logins(&db, &FailingEphemeralStore, &email, 1).await;
    login_as(&db, &FailingEphemeralStore, &email, "Password!234")
        .await
        .expect("login still works with the store down");
}

/// The forgot-password counter fails open, but issuing the reset token fails
/// closed: a known email is a 500 with no email sent. An unknown email
/// returns before touching the store, so it is still the plain 200.
#[sqlx::test]
async fn forgot_password_when_store_is_down(db: PgPool) {
    let background = TaskTracker::new();
    let email = format!("down-forgot-{}@example.com", Uuid::now_v7());
    common::seed_member(&db, &email, "Password!234").await;
    let mock = Arc::new(MockEmailClient::new());
    let email_client: Arc<dyn EmailSender> = mock.clone();

    let err = service::forgot_password(
        &db,
        &FailingEphemeralStore,
        email_client.clone(),
        &background,
        ForgotPasswordRequest {
            email: email.clone(),
        },
    )
    .await
    .expect_err("known email: token issue fails closed");
    assert_internal(&err);

    service::forgot_password(
        &db,
        &FailingEphemeralStore,
        email_client,
        &background,
        ForgotPasswordRequest {
            email: format!("down-ghost-{}@example.com", Uuid::now_v7()),
        },
    )
    .await
    .expect("unknown email never reaches the store");

    background.close();
    background.wait().await;
    assert!(mock.sent().is_empty(), "no email with the store down");
}

/// Consuming a reset token fails closed.
#[sqlx::test]
async fn reset_password_fails_closed_when_store_is_down(db: PgPool) {
    let err = service::reset_password(
        &db,
        &FailingEphemeralStore,
        ResetPasswordRequest {
            token: "any-token".into(),
            new_password: "NewPassword!234".into(),
        },
    )
    .await
    .expect_err("reset must fail closed");
    assert_internal(&err);
}

/// OTP send and verify fail closed — no SMS goes out unmetered.
#[sqlx::test]
async fn otp_fails_closed_when_store_is_down(db: PgPool) {
    let (server, sms) = twilio_sms().await;
    let email = format!("down-otp-{}@example.com", Uuid::now_v7());
    let user_id = common::seed_member(&db, &email, "Password!234").await;

    let err = send_otp(&FailingEphemeralStore, &sms, user_id)
        .await
        .expect_err("send must fail closed");
    assert_internal(&err);
    assert!(twilio_sent(&server).await.is_empty(), "no SMS sent");

    let err = verify_otp(&db, &FailingEphemeralStore, user_id, "123456")
        .await
        .expect_err("verify must fail closed");
    assert_internal(&err);
}

// ------- google_auth login rules (fake `GoogleIdentityProvider`) -------

/// Run `service::google_auth` with `google` standing in for Google.
async fn google_login(db: &PgPool, google: &FakeGoogleIdentity) -> Result<AuthResponse, AppError> {
    service::google_auth(
        db,
        &InMemoryAccessCache::new(),
        &common::test_auth_config(),
        google,
        GoogleAuthRequest {
            code: "fake-authorization-code".into(),
        },
        None,
    )
    .await
}

async fn welcome_count(db: &PgPool, user_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications \
         WHERE user_id = $1 AND type = 'system'::notification_type \
         AND title = 'Welcome to Dream Fly'",
    )
    .bind(user_id)
    .fetch_one(db)
    .await
    .expect("count welcome notifications")
}

/// Role names currently granted to `user_id`, straight from `user_roles`.
async fn role_names(db: &PgPool, user_id: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT r.name FROM roles r JOIN user_roles ur ON ur.role_id = r.id \
         WHERE ur.user_id = $1 ORDER BY r.name",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
    .expect("read role names")
}

/// `linking::plan`'s `Create` case (`send_welcome: true`): a brand-new Google
/// user is born as a `member` and gets a welcome notification.
#[sqlx::test]
async fn google_auth_new_user_gets_welcome_notification(db: PgPool) {
    let google = FakeGoogleIdentity::verified("google-sub-new-user", "NewGoogle@example.com");

    let resp = google_login(&db, &google).await.expect("google login");

    assert_eq!(resp.user.email, "newgoogle@example.com");
    assert_eq!(resp.user.roles, vec!["member"]);
    let welcome = common::latest_notification(&db, resp.user.id, "system")
        .await
        .expect("welcome notification row");
    assert_eq!(welcome.0, "Welcome to Dream Fly");
}

/// Deliberate asymmetry (see `auth::linking`'s module doc): linking Google to
/// an existing password account resolves to that account and does not resend
/// the welcome it already got at registration.
#[sqlx::test]
async fn google_auth_linking_existing_account_does_not_resend_welcome(db: PgPool) {
    let registered = service::register(
        &db,
        &common::test_auth_config(),
        RegisterRequest {
            email: "linkme@example.com".into(),
            name: "Link Me".into(),
            password: "Password!234".into(),
        },
        None,
    )
    .await
    .expect("register");
    assert_eq!(welcome_count(&db, registered.user.id).await, 1);

    let google = FakeGoogleIdentity::verified("google-sub-link-user", "linkme@example.com");
    let resp = google_login(&db, &google).await.expect("google link");

    assert_eq!(
        resp.user.id, registered.user.id,
        "google link must resolve to the same existing user"
    );
    assert_eq!(
        welcome_count(&db, registered.user.id).await,
        1,
        "linking Google to an existing password account must not resend the welcome notification"
    );
}

/// Google login used to skip the `is_active` gate that password login has.
/// `session::start` refuses it, and the refusal rolls back the whole
/// google_auth tx — no refresh token row, no `last_login` bump.
#[sqlx::test]
async fn google_auth_rejects_inactive_account(db: PgPool) {
    let google = FakeGoogleIdentity::verified("google-sub-inactive", "inactive-google@example.com");
    let user_id = google_login(&db, &google)
        .await
        .expect("first login")
        .user
        .id;

    sqlx::query("UPDATE users SET is_active = false WHERE id = $1")
        .bind(user_id)
        .execute(&db)
        .await
        .expect("deactivate user");

    let snapshot = |db: &PgPool| {
        let db = db.clone();
        async move {
            let rows: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM refresh_tokens WHERE user_id = $1")
                    .bind(user_id)
                    .fetch_one(&db)
                    .await
                    .expect("count refresh tokens");
            let last_login: Option<chrono::DateTime<chrono::Utc>> =
                sqlx::query_scalar("SELECT last_login FROM users WHERE id = $1")
                    .bind(user_id)
                    .fetch_one(&db)
                    .await
                    .expect("read last_login");
            (rows, last_login)
        }
    };
    let before = snapshot(&db).await;

    let err = google_login(&db, &google)
        .await
        .expect_err("inactive account must be refused");
    assert!(matches!(err, AppError::Unauthorized), "got: {err:?}");
    assert_eq!(
        snapshot(&db).await,
        before,
        "a refused Google login must not persist a refresh token or bump last_login"
    );
}

/// Only account birth (`LinkPlan::grant_member`, `Create` only) grants
/// `member`: a returning Google user (`Refresh`) whose `member` role an admin
/// removed does not get it back.
#[sqlx::test]
async fn google_auth_refresh_does_not_regrant_removed_member(db: PgPool) {
    let google = FakeGoogleIdentity::verified("google-sub-regrant", "regrant@example.com");
    let user_id = google_login(&db, &google)
        .await
        .expect("first login")
        .user
        .id;
    assert_eq!(role_names(&db, user_id).await, vec!["member"]);

    sqlx::query(
        "DELETE FROM user_roles WHERE user_id = $1 \
         AND role_id = (SELECT id FROM roles WHERE name = 'member')",
    )
    .bind(user_id)
    .execute(&db)
    .await
    .expect("remove member role");

    let resp = google_login(&db, &google).await.expect("returning login");
    assert!(resp.user.roles.is_empty(), "got: {:?}", resp.user.roles);
    assert!(role_names(&db, user_id).await.is_empty());
}

/// `Link` branch: linking Google to an existing account (here a seeded admin
/// with no `member` role) keeps exactly the roles it had.
#[sqlx::test]
async fn google_auth_link_does_not_grant_member_to_seeded_admin(db: PgPool) {
    let hash = common::hashed("Password!234").await;
    let mut tx = db.begin().await.expect("begin tx");
    let admin = repository::create_user_tx(
        &mut tx,
        "link-admin@example.com",
        "Admin",
        None,
        &hash,
        None,
    )
    .await
    .expect("insert admin");
    // The user row was created in this very tx, so no access-cache entry can
    // exist for it yet.
    permissions_repository::assign_role(&mut tx, admin.id, Role::Admin)
        .await
        .expect("assign admin")
        .assume_uncached();
    tx.commit().await.expect("commit admin seed");

    let google = FakeGoogleIdentity::verified("google-sub-link-admin", "link-admin@example.com");
    let resp = google_login(&db, &google).await.expect("google link");

    assert_eq!(resp.user.id, admin.id);
    assert_eq!(resp.user.roles, vec!["admin"]);
    assert_eq!(role_names(&db, admin.id).await, vec!["admin"]);
}

/// An email already bound to a different Google account is a 409 — the new
/// Google identity is neither linked nor created.
#[sqlx::test]
async fn google_auth_email_bound_to_another_google_account_conflicts(db: PgPool) {
    let first = FakeGoogleIdentity::verified("google-sub-first", "shared@example.com");
    google_login(&db, &first).await.expect("first login");

    let second = FakeGoogleIdentity::verified("google-sub-second", "shared@example.com");
    let err = google_login(&db, &second)
        .await
        .expect_err("a second Google account must not take the email");
    match err {
        AppError::Conflict(msg) => {
            assert_eq!(msg, "email already associated with another account")
        }
        other => panic!("expected Conflict, got {other:?}"),
    }

    let google_ids: Vec<Option<String>> = sqlx::query_scalar("SELECT google_id FROM users")
        .fetch_all(&db)
        .await
        .expect("read google_ids");
    assert_eq!(google_ids, vec![Some("google-sub-first".to_string())]);
}

/// A code the provider refuses fails before anything is written: no user,
/// no session, no outbox event.
#[sqlx::test]
async fn google_auth_provider_rejection_writes_nothing(db: PgPool) {
    let err = google_login(&db, &FakeGoogleIdentity::rejecting())
        .await
        .expect_err("a refused code must fail");
    match err {
        AppError::BadRequest(msg) => assert_eq!(msg, "Google authentication failed"),
        other => panic!("expected BadRequest, got {other:?}"),
    }

    for count_sql in [
        "SELECT COUNT(*) FROM users",
        "SELECT COUNT(*) FROM refresh_tokens",
        "SELECT COUNT(*) FROM events_outbox",
    ] {
        let rows: i64 = sqlx::query_scalar(count_sql)
            .fetch_one(&db)
            .await
            .expect("count rows");
        assert_eq!(rows, 0, "`{count_sql}` must be 0");
    }
}

// ---------------- login timing (bug #1: 帳號列舉時間差) ----------------

/// 三條 401 路徑各跑 1 次熱身(不計時)、再量 3 次取最小值:查無 email、
/// 只綁 Google 的帳號、已知帳號但密碼錯。修正前,前兩條完全跳過 Argon2
/// (只碰記憶體 store,耗時趨近 0);已知帳號密碼錯那條一定跑一次 Argon2
/// (~50-100ms)。4 倍餘裕只會被「miss 路徑跳過 Argon2」弄紅,commit 前照
/// brief 連跑 20 次觀察 wall-clock 餘裕是否夠(機器負載可能影響)。
#[sqlx::test]
async fn login_miss_costs_one_argon2_verification(db: PgPool) {
    let store = InMemoryEphemeralStore::new();
    let cfg = common::test_auth_config();

    let known_email = format!("timing-known-{}@example.com", Uuid::now_v7());
    common::seed_member(&db, &known_email, "Password!234").await;

    let missing_email = format!("timing-missing-{}@example.com", Uuid::now_v7());

    let google_email = format!("timing-google-{}@example.com", Uuid::now_v7());
    sqlx::query(
        r#"
        INSERT INTO users (id, email, name, password_hash, google_id, phone_verified, is_active, created_at, updated_at)
        VALUES ($1, $2, 'Timing Google User', NULL, $3, false, true, NOW(), NOW())
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(&google_email)
    .bind(format!("google-fixture-{google_email}"))
    .execute(&db)
    .await
    .expect("insert google-only user");

    async fn timed_login(
        db: &PgPool,
        store: &InMemoryEphemeralStore,
        cfg: &dream_fly_backend::config::AuthConfig,
        email: &str,
        password: &str,
    ) -> Duration {
        let start = Instant::now();
        let err = service::login(
            db,
            store,
            cfg,
            LoginRequest {
                email: email.into(),
                password: password.into(),
            },
        )
        .await
        .expect_err("failure count stays well under the lockout threshold in this test");
        assert!(matches!(err, AppError::Unauthorized), "got: {err:?}");
        start.elapsed()
    }

    /// 1 次熱身(不計入)、再量 3 次取最小值。
    async fn min_of_three(
        db: &PgPool,
        store: &InMemoryEphemeralStore,
        cfg: &dream_fly_backend::config::AuthConfig,
        email: &str,
        password: &str,
    ) -> Duration {
        timed_login(db, store, cfg, email, password).await;
        let mut min = Duration::MAX;
        for _ in 0..3 {
            min = min.min(timed_login(db, store, cfg, email, password).await);
        }
        min
    }

    let min_missing = min_of_three(&db, &store, &cfg, &missing_email, "anything").await;
    let min_google = min_of_three(&db, &store, &cfg, &google_email, "anything").await;
    let min_known = min_of_three(&db, &store, &cfg, &known_email, "wrong-password").await;

    let min_miss = min_missing.min(min_google);
    assert!(
        min_miss * 4 >= min_known,
        "a miss path (no account / no password hash) must cost at least ~1/4 of a \
         real Argon2 verification, not skip it: min_miss={min_miss:?} \
         (missing={min_missing:?}, google-only={min_google:?}), min_known={min_known:?}"
    );
}
