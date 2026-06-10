use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};

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

/// Verify a password against an Argon2 hash on a blocking thread (same
/// rationale as [`hash_password`]).
///
/// # Panics
/// Panics if the blocking task is cancelled or the spawned thread panics.
pub async fn verify_password(
    password: String,
    hash: String,
) -> Result<bool, argon2::password_hash::Error> {
    tokio::task::spawn_blocking(move || {
        let parsed_hash = PasswordHash::new(&hash)?;
        // Verification re-uses the params embedded in the stored PHC
        // string, so the hasher we construct here is only used to carry
        // the algorithm implementation — the parameters come from the hash.
        Ok(argon2()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok())
    })
    .await
    .expect("argon2 verify_password task panicked")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_then_verify_roundtrips() {
        let hash = hash_password("correct horse battery staple".into())
            .await
            .expect("hash succeeds");
        let ok = verify_password("correct horse battery staple".into(), hash)
            .await
            .expect("verify succeeds");
        assert!(ok, "password should verify against its own hash");
    }

    #[tokio::test]
    async fn verify_wrong_password_returns_false() {
        let hash = hash_password("the-real-password".into())
            .await
            .expect("hash succeeds");
        let ok = verify_password("not-the-password".into(), hash)
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
        assert!(verify_password("same-input".into(), a).await.unwrap());
        assert!(verify_password("same-input".into(), b).await.unwrap());
    }

    #[tokio::test]
    async fn verify_with_malformed_hash_returns_error() {
        // A caller that hands us a corrupt DB column should get an Err,
        // not a silent `false` — that distinction matters for logging
        // and triage.
        let result = verify_password("anything".into(), "not-a-valid-argon2-hash".into()).await;
        assert!(result.is_err(), "malformed hash should surface as error");
    }
}
