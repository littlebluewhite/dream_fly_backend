use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};

use crate::error::AppError;

/// Construct an Argon2id hasher with parameters pinned to the OWASP 2024
/// "first recommended configuration" (m=19 MiB, t=2, p=1). We pin them
/// explicitly so a future upstream default change cannot silently weaken
/// the hash. Verification uses the parameters embedded in the stored hash
/// (PHC string), so bumping these values here doesn't invalidate old
/// passwords — it just strengthens newly-set ones.
fn argon2() -> Argon2<'static> {
    let params = Params::new(19 * 1024, 2, 1, None)
        .expect("pinned Argon2 params are valid at compile time");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// Hash a password with Argon2id on a blocking thread so the ~50-100ms CPU
/// burst does not park the async runtime's worker threads. Without this wrap,
/// ~40 concurrent logins on a 4-core host can completely stall the runtime.
///
/// # Panics
/// Panics if the blocking task is cancelled or the spawned thread panics,
/// which indicates a bug in Argon2 or the tokio runtime — neither should
/// occur during normal operation.
pub async fn hash_password(password: String) -> Result<String, argon2::password_hash::Error> {
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        let hash = argon2().hash_password(password.as_bytes(), &salt)?;
        Ok(hash.to_string())
    })
    .await
    .expect("argon2 hash_password task panicked")
}

/// Lazily-generated PHC string with no real password behind it. `hash` is
/// `None` when the caller has nothing to check against (no such account, or
/// an account with no password set) — `verify_password` still runs one
/// Argon2 verification against this instead of returning early, so that
/// branch costs the same as a real mismatch (bug #1: account enumeration via
/// response timing). Built once per process from the same private
/// [`argon2()`] hasher, so its params always match a real stored hash's.
static DUMMY_HASH: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();

async fn dummy_hash() -> &'static str {
    DUMMY_HASH
        .get_or_init(|| async {
            hash_password("dummy-password-never-checked-against-a-real-account".into())
                .await
                .expect("dummy hash must succeed")
        })
        .await
}

/// Verify a password against an Argon2 hash on a blocking thread (same
/// rationale as [`hash_password`]). `hash: None` (no account, or an account
/// with no password set) still costs one Argon2 verification — against
/// [`dummy_hash`] — and always resolves to `Ok(false)`; there is no
/// early-return path that skips the CPU cost, so callers can't be timed to
/// tell "no such hash" apart from "wrong password".
///
/// # Panics
/// Panics if the blocking task is cancelled or the spawned thread panics.
pub async fn verify_password(
    password: String,
    hash: Option<String>,
) -> Result<bool, argon2::password_hash::Error> {
    let (hash, force_false) = match hash {
        Some(hash) => (hash, false),
        None => (dummy_hash().await.to_string(), true),
    };

    tokio::task::spawn_blocking(move || {
        let parsed_hash = PasswordHash::new(&hash)?;
        // Verification re-uses the params embedded in the stored PHC
        // string, so the hasher we construct here is only used to carry
        // the algorithm implementation — the parameters come from the hash.
        let matched = argon2()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok();
        Ok(matched && !force_false)
    })
    .await
    .expect("argon2 verify_password task panicked")
}

/// `hash_password` 換成 `AppError` 的薄封裝——三個呼叫端(`auth::service::register`、
/// `auth::service::reset_password`、`users::service::create_user`)雜湊失敗時
/// 映射成同一句 log 字串,原本各自重複這行,收進這裡後只有一處。放在
/// `utils::password` 而非 auth 私有 module,是因為 `users` 不該依賴 auth 的
/// 私有實作。
pub async fn hash_for_storage(password: String) -> Result<String, AppError> {
    hash_password(password)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("password hash error: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_then_verify_roundtrips() {
        let hash = hash_password("correct horse battery staple".into())
            .await
            .expect("hash succeeds");
        let ok = verify_password("correct horse battery staple".into(), Some(hash))
            .await
            .expect("verify succeeds");
        assert!(ok, "password should verify against its own hash");
    }

    #[tokio::test]
    async fn verify_wrong_password_returns_false() {
        let hash = hash_password("the-real-password".into())
            .await
            .expect("hash succeeds");
        let ok = verify_password("not-the-password".into(), Some(hash))
            .await
            .expect("verify does not error on mismatch");
        assert!(!ok, "wrong password must not verify");
    }

    #[tokio::test]
    async fn two_hashes_of_same_password_differ_but_both_verify() {
        // Argon2 uses a fresh random salt each call, so two hashes of the
        // same plaintext should be byte-different but both valid. This
        // catches any accidental switch to a deterministic salt.
        let a = hash_password("same-input".into()).await.unwrap();
        let b = hash_password("same-input".into()).await.unwrap();
        assert_ne!(a, b, "hashes of same password must differ (random salt)");
        assert!(verify_password("same-input".into(), Some(a)).await.unwrap());
        assert!(verify_password("same-input".into(), Some(b)).await.unwrap());
    }

    #[tokio::test]
    async fn verify_with_malformed_hash_returns_error() {
        // A caller that hands us a corrupt DB column should get an Err,
        // not a silent `false` — that distinction matters for logging
        // and triage.
        let result =
            verify_password("anything".into(), Some("not-a-valid-argon2-hash".into())).await;
        assert!(result.is_err(), "malformed hash should surface as error");
    }

    /// `1d` 的決定性單元測試(對照 wall-clock 測試
    /// `tests/service_auth.rs::login_miss_costs_one_argon2_verification`):
    /// `hash: None` 一律驗一次(不管密碼是什麼)並恆回 `Ok(false)`——不是
    /// `matched` 剛好為 false,是 `force_false` 蓋過去,即使假雜湊真的被
    /// 猜中也一樣。
    #[tokio::test]
    async fn absent_hash_never_verifies() {
        let ok = verify_password("anything".into(), None)
            .await
            .expect("verify against the dummy hash does not error");
        assert!(!ok, "no hash to check against must never verify");

        // 就算猜中了假雜湊背後真正用的密碼,還是要回 false——`force_false`
        // 不是碰巧算出來的。
        let ok = verify_password(
            "dummy-password-never-checked-against-a-real-account".into(),
            None,
        )
        .await
        .expect("verify against the dummy hash does not error");
        assert!(
            !ok,
            "guessing the dummy password must still resolve to false"
        );
    }

    /// 假雜湊必須是同一支 `argon2()` 產生的、參數釘住 argon2id / v=0x13 /
    /// 生產同一組 m/t/p——沒有它,`None` 分支的耗時就不會逼近真實雜湊的
    /// 驗證耗時,bug #1 又會露出來。
    #[tokio::test]
    async fn dummy_hash_carries_pinned_params() {
        let hash = dummy_hash().await;
        let parsed = PasswordHash::new(hash).expect("dummy hash parses as a PHC string");

        assert_eq!(parsed.algorithm.as_str(), "argon2id");
        assert_eq!(parsed.version, Some(0x13));

        // `output_len` differs by construction (the parsed hash's is fixed
        // by its actual digest length; `argon2()`'s is left unset) and isn't
        // one of the pinned params — compare m/t/p only.
        let parsed_params =
            Params::try_from(&parsed).expect("dummy hash carries valid Argon2 params");
        let pinned = argon2().params().clone();
        assert_eq!(parsed_params.m_cost(), pinned.m_cost());
        assert_eq!(parsed_params.t_cost(), pinned.t_cost());
        assert_eq!(parsed_params.p_cost(), pinned.p_cost());
    }
}
