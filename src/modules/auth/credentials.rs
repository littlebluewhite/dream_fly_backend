//! 憑證檢查(Credential Check)——「email + 密碼 → 已驗證 `User` 或 401」的
//! 單一 owner。`login` 之外沒有第二個呼叫點碰 `rate_limit::record_login_failure`
//! /`clear_login_failures`,登入失敗計數與清除只在這裡發生。
//!
//! 流程固定四步:鎖定檢查(`rate_limit::login_locked_out`)→ 查帳號 → Argon2
//! 驗證 → `judge`(純函式,把「查到什麼、密碼對不對」收斂成一個判決)→
//! 收尾(計失敗或清計數)。鎖定分支刻意是快速路徑——已鎖的 email 不查
//! DB、不跑 Argon2,只洩漏「這個 key 被鎖了」,不洩漏「這個帳號存不存
//! 在」(不存在的 email 一樣會被鎖進同一個計數器)。
//!
//! 查無帳號與只綁 Google 的帳號(無 `password_hash`)沿用「略過 Argon2」
//! 的舊行為——1d 前的狀態,之後這裡的行為變成一律驗一次
//! (`utils::password::verify_password`),見該 commit。
//!
//! `is_active` 檢查收在 `judge` 裡:密碼驗證通過之後才判,順序仍先於
//! 呼叫端(`service::login`)清計數與 `update_last_login`。

use sqlx::PgPool;

use crate::error::AppError;
use crate::utils::ephemeral::EphemeralStore;
use crate::utils::password;

use super::model::User;
use super::rate_limit;
use super::repository;

/// `judge` 的判決:`Accept` 帶已驗證且啟用的帳號;`Reject.counts` 是否要
/// 算進登入失敗計數——停用帳號密碼驗證正確時不計(帳號本身沒問題,計了
/// 只會冤枉地把它鎖死)。
enum Verdict {
    Accept(User),
    Reject { counts: bool },
}

/// 純函式:把「查到的帳號」與「密碼是否驗證通過」收斂成一個判決。即使
/// `matched` 傳入 `true`,沒有 `password_hash` 的帳號(查無帳號、只綁
/// Google)仍然 Reject——這條防線不信任呼叫端算出的 `matched`,自己再確
/// 認一次「有沒有東西可以驗」。
fn judge(user: Option<User>, matched: bool) -> Verdict {
    match user {
        None => Verdict::Reject { counts: true },
        Some(u) if u.password_hash.is_none() => Verdict::Reject { counts: true },
        Some(_) if !matched => Verdict::Reject { counts: true },
        Some(u) if !u.is_active => Verdict::Reject { counts: false },
        Some(u) => Verdict::Accept(u),
    }
}

/// email 須已由呼叫端 `normalize_email` 正規化。
pub(super) async fn verify(
    db: &PgPool,
    store: &dyn EphemeralStore,
    email: &str,
    password: &str,
) -> Result<User, AppError> {
    // 1. 每 email 失敗次數鎖定——同一錯誤碼(呼叫端統一回 401),不洩漏鎖定
    //    本身。
    if rate_limit::login_locked_out(store, email).await {
        return Err(AppError::Unauthorized);
    }

    // 2. 查帳號。
    let user = repository::find_user_by_email(db, email).await?;

    // 3. 只在帳號存在且有密碼雜湊時才跑 Argon2——查無帳號、只綁 Google 的
    //    帳號沿用舊行為略過驗證(1d 改)。
    let matched = match user.as_ref().and_then(|u| u.password_hash.as_deref()) {
        Some(hash) => self::password::verify_password(password.to_string(), hash.to_string())
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("password verify error: {e}")))?,
        None => false,
    };

    // 4. 判決收尾:唯一呼叫 rate_limit 計數/清除的地方。
    match judge(user, matched) {
        Verdict::Reject { counts: true } => {
            rate_limit::record_login_failure(store, email).await;
            Err(AppError::Unauthorized)
        }
        Verdict::Reject { counts: false } => Err(AppError::Unauthorized),
        Verdict::Accept(user) => {
            rate_limit::clear_login_failures(store, email).await;
            Ok(user)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::ephemeral::recording::{Call, RecordingStore};

    fn user_fixture(password_hash: Option<&str>, is_active: bool) -> User {
        use chrono::Utc;
        use uuid::Uuid;

        User {
            id: Uuid::now_v7(),
            email: "judge-fixture@example.com".into(),
            name: "Judge Fixture".into(),
            phone: None,
            phone_verified: false,
            avatar_url: None,
            password_hash: password_hash.map(str::to_string),
            google_id: None,
            is_active,
            last_login: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            points_balance: 0,
            preferences: None,
            birth_date: None,
        }
    }

    #[test]
    fn judge_table() {
        // (None, _) → Reject { counts: true }
        assert!(matches!(
            judge(None, true),
            Verdict::Reject { counts: true }
        ));
        assert!(matches!(
            judge(None, false),
            Verdict::Reject { counts: true }
        ));

        // (無 hash, true) → Reject { counts: true } — 不信任 matched。
        assert!(matches!(
            judge(Some(user_fixture(None, true)), true),
            Verdict::Reject { counts: true }
        ));

        // (有 hash, false) → Reject { counts: true }
        assert!(matches!(
            judge(Some(user_fixture(Some("hash"), true)), false),
            Verdict::Reject { counts: true }
        ));

        // (有 hash, true, 停用) → Reject { counts: false }
        assert!(matches!(
            judge(Some(user_fixture(Some("hash"), false)), true),
            Verdict::Reject { counts: false }
        ));

        // (有 hash, true, 啟用) → Accept
        assert!(matches!(
            judge(Some(user_fixture(Some("hash"), true)), true),
            Verdict::Accept(_)
        ));
    }

    fn login_fail_key(email: &str) -> String {
        format!("login_fail:{email}")
    }

    /// Bare `INSERT` rather than `repository::create_user_tx`: that helper
    /// always sets a password hash and `is_active = true`, but the table
    /// below needs a NULL hash (Google-only) and a disabled account too. A
    /// NULL `password_hash` needs a `google_id` to satisfy
    /// `users_has_auth_method` — a fixture value, never a real one.
    async fn insert_login_test_user(
        db: &PgPool,
        email: &str,
        password_hash: Option<&str>,
        is_active: bool,
    ) {
        let google_id = password_hash
            .is_none()
            .then(|| format!("google-fixture-{email}"));
        sqlx::query(
            r#"
            INSERT INTO users (id, email, name, password_hash, google_id, phone_verified, is_active, created_at, updated_at)
            VALUES ($1, $2, 'Credential Check Test User', $3, $4, false, $5, NOW(), NOW())
            "#,
        )
        .bind(uuid::Uuid::now_v7())
        .bind(email)
        .bind(password_hash)
        .bind(google_id)
        .bind(is_active)
        .execute(db)
        .await
        .expect("insert test user");
    }

    /// Pins that every 401 branch of `verify` counts exactly one failure —
    /// except the two branches that must not (a disabled account with the
    /// right password, and an already-locked email, neither of which may
    /// touch the counter or clear it). See the phase-1 brief's table.
    /// Table-driven against one db: each scenario uses its own email so the
    /// failure counters can't bleed into each other. (Moved here from
    /// `service::tests` in the credentials extraction — same assertions,
    /// now driving `credentials::verify` directly instead of `login`.)
    #[sqlx::test]
    async fn verify_counts_each_401_branch_exactly_once(db: PgPool) {
        let correct_password = "Password!234";
        let correct_hash = password::hash_password(correct_password.into())
            .await
            .expect("hash correct password");
        let wrong_hash = password::hash_password("some-other-password".into())
            .await
            .expect("hash wrong-password fixture");

        // 查無 email — never in the DB.
        {
            let email = "missing@example.com";
            let store = RecordingStore::default();
            let err = verify(&db, &store, email, correct_password)
                .await
                .expect_err("missing email must be 401");
            assert!(matches!(err, AppError::Unauthorized), "got: {err:?}");
            assert_eq!(
                store.take_calls(),
                vec![
                    Call::Get(login_fail_key(email)),
                    Call::Incr(login_fail_key(email), 900),
                ]
            );
        }

        // 只綁 Google — user exists, password_hash NULL.
        {
            let email = "google-only@example.com";
            insert_login_test_user(&db, email, None, true).await;
            let store = RecordingStore::default();
            let err = verify(&db, &store, email, correct_password)
                .await
                .expect_err("google-only account must be 401");
            assert!(matches!(err, AppError::Unauthorized), "got: {err:?}");
            assert_eq!(
                store.take_calls(),
                vec![
                    Call::Get(login_fail_key(email)),
                    Call::Incr(login_fail_key(email), 900),
                ]
            );
        }

        // 密碼錯
        {
            let email = "wrong-password@example.com";
            insert_login_test_user(&db, email, Some(&wrong_hash), true).await;
            let store = RecordingStore::default();
            let err = verify(&db, &store, email, correct_password)
                .await
                .expect_err("wrong password must be 401");
            assert!(matches!(err, AppError::Unauthorized), "got: {err:?}");
            assert_eq!(
                store.take_calls(),
                vec![
                    Call::Get(login_fail_key(email)),
                    Call::Incr(login_fail_key(email), 900),
                ]
            );
        }

        // 停用＋密碼錯
        {
            let email = "disabled-wrong@example.com";
            insert_login_test_user(&db, email, Some(&wrong_hash), false).await;
            let store = RecordingStore::default();
            let err = verify(&db, &store, email, correct_password)
                .await
                .expect_err("disabled + wrong password must be 401");
            assert!(matches!(err, AppError::Unauthorized), "got: {err:?}");
            assert_eq!(
                store.take_calls(),
                vec![
                    Call::Get(login_fail_key(email)),
                    Call::Incr(login_fail_key(email), 900),
                ]
            );
        }

        // 停用＋密碼對 — is_active is checked after the password verifies;
        // no count, no clear.
        {
            let email = "disabled-correct@example.com";
            insert_login_test_user(&db, email, Some(&correct_hash), false).await;
            let store = RecordingStore::default();
            let err = verify(&db, &store, email, correct_password)
                .await
                .expect_err("disabled account must be 401 even with the right password");
            assert!(matches!(err, AppError::Unauthorized), "got: {err:?}");
            assert_eq!(store.take_calls(), vec![Call::Get(login_fail_key(email))]);
        }

        // 已鎖 — the lockout check alone must short-circuit before ever
        // querying the DB or running Argon2.
        {
            let email = "locked-out@example.com";
            let store = RecordingStore::default().with_value(&login_fail_key(email), "10");
            let err = verify(&db, &store, email, correct_password)
                .await
                .expect_err("locked-out email must be 401 without querying the DB");
            assert!(matches!(err, AppError::Unauthorized), "got: {err:?}");
            assert_eq!(store.take_calls(), vec![Call::Get(login_fail_key(email))]);
        }

        // 成功
        {
            let email = "success@example.com";
            insert_login_test_user(&db, email, Some(&correct_hash), true).await;
            let store = RecordingStore::default();
            let user = verify(&db, &store, email, correct_password)
                .await
                .expect("correct credentials on an active account must succeed");
            assert_eq!(user.email, email);
            assert_eq!(
                store.take_calls(),
                vec![
                    Call::Get(login_fail_key(email)),
                    Call::Del(login_fail_key(email)),
                ]
            );
        }
    }
}
