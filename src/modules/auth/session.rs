//! Session 生命週期(Session Lifecycle)——`refresh_tokens` 表的單一 owner:
//! 簽發(`start`)、輪替(`rotate`)、單一登出(`end`)、整族撤銷
//! (`end_all`)、過期清理(`purge_expired`)五個動作都在這裡,這張表的 SQL
//! 也只在這裡(私有),對外不可見——比照 [`crate::modules::bookings::occupancy`]
//! 「協定 SQL 與協定邏輯同檔」的先例。`auth::service` 的
//! `refresh_token`/`logout` 只是一行轉呼叫(維持 handler→service 形狀,
//! ADR-0005)。
//!
//! 由本模組獨力維護的 invariant:
//! - refresh token 進庫前必雜湊(`jwt::hash_token`,SHA-256——DB 外洩不直接
//!   洩漏可用憑證);查找也一律以雜湊查,不以原字串查。
//! - access/refresh 恆成對簽發(不存在只發一邊的中間態)。
//! - `expires_at` = 簽發當下 `now + jwt_refresh_expiration_days`(auth token
//!   效期是時鐘 seam 記錄在案的 carve-out,直呼 `Utc::now()`)。
//! - 輪替原子化:舊 token revoke、新 token 簽發在同一 tx 內同進同出。
//! - reuse detection:已 revoke 的 token 再次出現視為竊取重放,整族撤銷。
//!   只有輪替與單一登出(`end`)會留下 revoked 列;整族撤銷(`end_all`)直接
//!   刪列,之後舊 token 只得到一般 401,不會把使用者重新登入後的新 session
//!   誤判為重放而連帶殺掉。
//! - 停用帳號(`!is_active`)在任何簽發路徑都拿不到 session(`start` 首行)。
//! - 輪替先以 `FOR SHARE` 鎖 user 列再鎖 token 列(user-first,與停用、
//!   `reset_password` 同序),進行中的停用會讓輪替等它 commit 再判斷。
//! - retention:清理只刪過期列;輪替/登出留下的已 revoke 未過期列保留到過期,
//!   reuse detection 才認得出重放(過期後由 JWT `exp` 先擋,只差 5 秒 leeway)。

use chrono::{DateTime, Duration, Utc};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::config::AuthConfig;
use crate::error::AppError;
use crate::modules::permissions::repository as permissions_repository;
use crate::utils::jwt;

use super::dto::{AuthResponse, UserResponse};
use super::model::{RefreshToken, User};
use super::repository;

// Single session-issuance owner: encodes the access + refresh JWTs, computes
// the refresh expiry (`now + jwt_refresh_expiration_days` — auth token expiry
// is the documented clock-seam carve-out, so a direct `Utc::now()` is fine
// here), hashes the refresh token with SHA-256 before persisting it (so a
// database compromise does not leak live refresh credentials), loads the
// user's roles, and assembles the `AuthResponse`. Callers own the
// transaction/connection boundary and pass it in as `&mut PgConnection`
// (`&mut tx` deref-coerces for the three transactional callers; `login`
// acquires a plain pooled connection instead). Every internal query reborrows
// `&mut *conn` — passing `conn` straight through would move it out on the
// first use.
pub(super) async fn start(
    conn: &mut PgConnection,
    config: &AuthConfig,
    user: &User,
) -> Result<AuthResponse, AppError> {
    // Every issuance path crosses this gate, so no caller can hand a session
    // to a deactivated account (Google login used to skip the check).
    if !user.is_active {
        return Err(AppError::Unauthorized);
    }

    let access_token = jwt::encode_access_token(config, user.id, &user.email)?;
    let refresh_token = jwt::encode_refresh_token(config, user.id)?;

    let expires_at = Utc::now() + Duration::days(config.jwt_refresh_expiration_days as i64);
    let token_hash = jwt::hash_token(&refresh_token);

    save_refresh_token(&mut *conn, user.id, &token_hash, expires_at).await?;

    let roles = permissions_repository::find_role_names_by_user(&mut *conn, user.id).await?;

    Ok(AuthResponse {
        access_token,
        refresh_token,
        user: UserResponse::new(user.clone(), roles),
    })
}

pub(super) async fn rotate(
    db: &PgPool,
    config: &AuthConfig,
    refresh_token: &str,
) -> Result<AuthResponse, AppError> {
    // 1. Decode refresh token JWT (verifies signature + claims)
    let claims = jwt::decode_refresh_token(config, refresh_token)?;

    // 2. Look up by SHA-256 hash, never by raw token
    let token_hash = jwt::hash_token(refresh_token);
    let user_id: Uuid = claims.sub.parse().map_err(|_| AppError::Unauthorized)?;

    // 3. Atomically: find + revoke old, create new — everything in one tx
    let mut tx = db.begin().await?;

    // User row first, `FOR SHARE` — the same user-first order as deactivation
    // and `reset_password`. An in-flight deactivation makes this wait, so the
    // `is_active` check below sees it; read unlocked, a racing refresh could
    // issue a new token that the deactivation's `end_all` never saw.
    let user = repository::find_user_by_id_for_share_tx(&mut tx, user_id)
        .await?
        .ok_or(AppError::Unauthorized)?;

    let stored = find_refresh_token_tx(&mut tx, &token_hash)
        .await?
        .ok_or(AppError::Unauthorized)?;

    if stored.user_id != user_id {
        // JWT sub does not match stored record — refuse.
        return Err(AppError::Unauthorized);
    }

    if stored.revoked {
        // Reuse detection: if a revoked token is seen again, treat it as a
        // stolen-token replay and invalidate the user's entire token family.
        // Only rotation and logout (`end`) leave revoked rows behind —
        // `end_all` deletes rows —
        // so a token killed by a password reset / deactivation / earlier
        // reuse wipe finds no row above (plain 401) and cannot trip this
        // branch against the user's newer sessions.
        //
        // Log at ERROR with a distinct `security_event` field so SIEM rules
        // can alert on this specifically — token reuse is a strong
        // indicator of credential theft rather than a benign retry.
        end_all(&mut tx, stored.user_id).await?;
        tx.commit().await?;
        tracing::error!(
            security_event = "refresh_token_reuse",
            user_id = %stored.user_id,
            "refresh token reuse detected; all sessions revoked"
        );
        return Err(AppError::Unauthorized);
    }

    if stored.expires_at < Utc::now() {
        return Err(AppError::Unauthorized);
    }

    revoke_refresh_token(&mut *tx, &token_hash).await?;

    // 4. Deactivated users cannot mint new access tokens, even with a valid
    // refresh token. This closes the window where a disabled user could
    // keep refreshing until the cached is_active flag expires.
    if !user.is_active {
        end_all(&mut tx, user.id).await?;
        tx.commit().await?;
        return Err(AppError::Unauthorized);
    }

    // 5. Issue + persist the new refresh token, still inside the same tx.
    // Behavior fix: roles used to be read from the pool *after* commit — if
    // that query had failed, the old token was already revoked and the new
    // one never reached the client, so the client's next request with the
    // now-dead old token would trip reuse detection and revoke the whole
    // family. Routing this through `start` (same tx, pre-commit) makes
    // issuance atomic with the rest of the rotation.
    let response = start(&mut tx, config, &user).await?;

    tx.commit().await?;

    Ok(response)
}

pub(super) async fn end(
    db: &PgPool,
    config: &AuthConfig,
    refresh_token: &str,
) -> Result<(), AppError> {
    // Verify the JWT first so random strings cannot be used to revoke tokens.
    // If it doesn't parse, treat as success so logout is idempotent client-side.
    if jwt::decode_refresh_token(config, refresh_token).is_err() {
        return Ok(());
    }

    let token_hash = jwt::hash_token(refresh_token);
    revoke_refresh_token(db, &token_hash).await?;
    Ok(())
}

/// Ends every session of `user_id` by deleting all of its refresh-token rows
/// (the whole token family, revoked rows included). Deleting rather than
/// marking revoked is deliberate: revoked rows are retained for reuse
/// detection, so a revoked-by-`end_all` row would make a stale device's old
/// token look like a replay once the user logs in again, and `rotate` would
/// then wipe the new sessions too. With the rows gone, an old token is a
/// plain 401. DB only — the caller owns the tx boundary and, after commit,
/// any access-cache invalidation (`auth::access::AccessDirty::flush`).
pub(super) async fn end_all(conn: &mut PgConnection, user_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM refresh_tokens WHERE user_id = $1")
        .bind(user_id)
        .execute(conn)
        .await?;
    Ok(())
}

/// Deletes expired refresh-token rows only. Revoked-but-unexpired rows (only
/// rotation and logout produce them; `end_all` deletes outright) are kept on
/// purpose: reuse detection in `rotate` needs the revoked row to
/// recognise a replayed rotated-out token (a missing row is a plain 401 with
/// no family revoke). Once a row has expired, the JWT's own `exp` (same
/// `jwt_refresh_expiration_days` horizon) rejects the token in
/// `jwt::decode_refresh_token` before any lookup, so the row is no longer
/// needed — except for the validator's 5-second leeway, in which a replay of
/// a just-purged token gets a plain 401 instead of a family revoke. Backed by the full `idx_refresh_tokens_expires_at` index
/// (migration `20260926000002`).
pub async fn purge_expired(db: &PgPool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM refresh_tokens WHERE expires_at < NOW()")
        .execute(db)
        .await?;
    Ok(result.rows_affected())
}

async fn save_refresh_token(
    executor: impl sqlx::PgExecutor<'_>,
    user_id: Uuid,
    token_hash: &str,
    expires_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO refresh_tokens (id, user_id, token_hash, expires_at, revoked, created_at)
        VALUES ($1, $2, $3, $4, false, NOW())
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(token_hash)
    .bind(expires_at)
    .execute(executor)
    .await?;
    Ok(())
}

async fn find_refresh_token_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    token_hash: &str,
) -> Result<Option<RefreshToken>, sqlx::Error> {
    sqlx::query_as::<_, RefreshToken>(
        "SELECT * FROM refresh_tokens WHERE token_hash = $1 FOR UPDATE",
    )
    .bind(token_hash)
    .fetch_optional(&mut **tx)
    .await
}

async fn revoke_refresh_token(
    executor: impl sqlx::PgExecutor<'_>,
    token_hash: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE refresh_tokens SET revoked = true WHERE token_hash = $1")
        .bind(token_hash)
        .execute(executor)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deliberately not the `30` used by `common::test_auth_config` elsewhere
    /// in the suite, so the expiry assertion below can't pass by coincidence
    /// against some other hardcoded constant.
    fn test_config() -> AuthConfig {
        AuthConfig {
            jwt_secret: "owner-test-secret-at-least-32-chars-0000".into(),
            jwt_access_expiration_minutes: 15,
            jwt_refresh_expiration_days: 14,
            google_client_id: "test-client".into(),
            google_client_secret: "test-secret".into(),
            google_redirect_url: "http://localhost/oauth/callback".into(),
            google_token_url: "http://127.0.0.1:1/oauth/token".into(),
            google_jwks_url: "http://127.0.0.1:1/certs".into(),
        }
    }

    /// `refresh_tokens.user_id` carries a `REFERENCES users(id)` FK, so
    /// `start` needs a real `users` row to attach to. Reuses
    /// `repository::create_user_tx` rather than hand-rolling a parallel
    /// INSERT.
    async fn insert_bare_user(db: &PgPool, email: &str) -> User {
        let mut tx = db.begin().await.expect("begin tx");
        let user = repository::create_user_tx(
            &mut tx,
            email,
            "Owner Test User",
            None,
            "owner-test-hash",
            None,
            Utc::now(),
        )
        .await
        .expect("insert bare user");
        tx.commit().await.expect("commit user insert");
        user
    }

    /// The owner invariant this module exists to guarantee, asserted
    /// directly against `start`'s observable effects rather than through
    /// `register`/`login`/etc (which only exercise it indirectly):
    /// - the persisted refresh token is a SHA-256 hash, never the raw JWT
    /// - the access and refresh tokens are issued as a matched pair (same
    ///   subject, distinct strings)
    /// - the persisted expiry is `now + jwt_refresh_expiration_days`
    #[sqlx::test]
    async fn start_hashes_pairs_and_expires_at_now_plus_n_days(db: PgPool) {
        let config = test_config();
        let user = insert_bare_user(&db, "owner-invariant@example.com").await;

        let mut conn = db.acquire().await.expect("acquire conn");
        let before = Utc::now();
        let response = start(&mut conn, &config, &user).await.expect("start");
        let after = Utc::now();

        // Paired issuance: both halves decode and agree on the same subject.
        let access_claims =
            jwt::decode_access_token(&config, &response.access_token).expect("decode access token");
        let refresh_claims = jwt::decode_refresh_token(&config, &response.refresh_token)
            .expect("decode refresh token");
        assert_eq!(access_claims.sub, user.id.to_string());
        assert_eq!(refresh_claims.sub, user.id.to_string());
        assert_ne!(response.access_token, response.refresh_token);

        // Must-hash-before-store: the persisted row never holds the raw JWT.
        let (stored_hash, stored_expires_at): (String, chrono::DateTime<Utc>) =
            sqlx::query_as("SELECT token_hash, expires_at FROM refresh_tokens WHERE user_id = $1")
                .bind(user.id)
                .fetch_one(&db)
                .await
                .expect("read refresh_token row");
        assert_eq!(stored_hash, jwt::hash_token(&response.refresh_token));
        assert_ne!(stored_hash, response.refresh_token);

        // Expiry = now + N days, bracketed by the wall-clock window around
        // the call so this can't be flaky.
        let expected_min = before + Duration::days(config.jwt_refresh_expiration_days as i64);
        let expected_max = after + Duration::days(config.jwt_refresh_expiration_days as i64);
        assert!(
            stored_expires_at >= expected_min && stored_expires_at <= expected_max,
            "expires_at {stored_expires_at:?} not within [{expected_min:?}, {expected_max:?}]"
        );
    }

    /// `start` is the one gate every issuance path crosses, so an inactive
    /// account is refused here — not only by `login`'s early check (Google
    /// login used to issue a session to a deactivated account). Nothing may
    /// be persisted on the refusal.
    #[sqlx::test]
    async fn start_refuses_inactive_account(db: PgPool) {
        let config = test_config();
        let mut user = insert_bare_user(&db, "inactive-start@example.com").await;
        user.is_active = false;

        let mut conn = db.acquire().await.expect("acquire conn");
        let err = start(&mut conn, &config, &user)
            .await
            .expect_err("inactive account must be refused");
        assert!(matches!(err, AppError::Unauthorized), "got: {err:?}");

        let rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM refresh_tokens WHERE user_id = $1")
                .bind(user.id)
                .fetch_one(&db)
                .await
                .expect("count refresh_tokens");
        assert_eq!(rows, 0, "a refused start must not persist a refresh token");
    }
}
