//! OTP (One-Time Password) send/verify lifecycle — the complete
//! phone-verification flow: per-user request-rate check, 6-digit code
//! generation, user-scoped short-lived storage (`EphemeralStore`), SMS
//! dispatch, and verification (`check`: attempt-count bump + code compare;
//! `consume`: cleanup, called only after the caller's DB write succeeds).
//! Key formats and limits are private to this module.
//!
//! Every store call is **fail-closed**: a store error (INCR, SET, GET or
//! DEL alike) fails the request with a 500 rather than skipping the rate
//! limit or the attempt count — unbounded OTP sends cost money, and an
//! unbounded attempt count would allow brute force.
//!
//! The verify-attempt-count-bump and OTP-invalidation-on-too-many-attempts
//! in `check` below is a single invariant — bumping the attempt
//! counter and (past the threshold) deleting the live OTP are two halves of
//! one "fail closed on brute force" decision, so they are never split
//! across functions or files.
//!
//! `update_phone_verified`'s DB write stays in `service.rs`: this module
//! has no `PgPool` and never touches the database — it only reports
//! whether verification succeeded.

use uuid::Uuid;

use crate::error::AppError;
use crate::utils::ephemeral::EphemeralStore;
use crate::utils::sms::SmsClient;

use super::dto::{MessageResponse, OtpSendRequest, OtpVerifyRequest};

/// Maximum OTP requests a single authenticated user may trigger per hour.
const OTP_REQUESTS_PER_HOUR: i64 = 3;
/// Maximum failed verification attempts before the OTP is invalidated.
const OTP_MAX_ATTEMPTS: i64 = 5;
/// OTP lifetime in seconds.
const OTP_TTL_SECONDS: u64 = 300;
/// OTP rate-limit window in seconds.
const OTP_RATE_LIMIT_TTL: u64 = 3600;

fn rate_key(auth_user_id: Uuid) -> String {
    format!("otp_rate:{auth_user_id}")
}

fn otp_key(auth_user_id: Uuid) -> String {
    format!("otp:{auth_user_id}")
}

fn attempts_key(auth_user_id: Uuid) -> String {
    format!("otp_attempts:{auth_user_id}")
}

pub(super) async fn send_otp(
    store: &dyn EphemeralStore,
    sms_client: &SmsClient,
    auth_user_id: Uuid,
    req: OtpSendRequest,
) -> Result<MessageResponse, AppError> {
    use rand::RngExt;

    // 1. Per-user rate limit — costs money if unbounded.
    let count = store
        .incr_with_ttl(&rate_key(auth_user_id), OTP_RATE_LIMIT_TTL)
        .await?;
    if count > OTP_REQUESTS_PER_HOUR {
        return Err(AppError::BadRequest(
            "too many verification requests, try again later".into(),
        ));
    }

    // 2. Generate 6-digit random code
    let code: u32 = rand::rng().random_range(100000..=999999);
    let code_str = format!("{:06}", code);

    // 3. Store under a user-scoped key so a user cannot verify a
    //    phone they did not initiate. Store {phone,code} as a JSON payload.
    let payload = serde_json::json!({
        "phone": req.phone,
        "code": code_str,
    })
    .to_string();

    store
        .set_ex(&otp_key(auth_user_id), &payload, OTP_TTL_SECONDS)
        .await?;

    // Reset attempt counter whenever a fresh OTP is issued.
    store.del(&attempts_key(auth_user_id)).await?;

    // 4. Send SMS
    sms_client.send_otp(&req.phone, &code_str).await?;

    Ok(MessageResponse {
        message: "verification code sent".into(),
    })
}

/// Evidence that [`check`] accepted the code. The code is still live in the
/// store until [`consume`] is called with this proof, so a caller whose
/// follow-up DB write fails leaves the code usable for a retry.
#[must_use = "an accepted code stays live until consume() is called"]
pub(super) struct OtpProof {
    auth_user_id: Uuid,
}

/// Check an OTP for `auth_user_id` without consuming it. The caller
/// (`service::verify_otp`) does the DB write that marks the phone verified,
/// then calls [`consume`].
pub(super) async fn check(
    store: &dyn EphemeralStore,
    auth_user_id: Uuid,
    req: &OtpVerifyRequest,
) -> Result<OtpProof, AppError> {
    use subtle::ConstantTimeEq;

    // 1. Bump the per-user attempt counter first — fail-closed on brute force.
    let attempts_key = attempts_key(auth_user_id);
    let attempts = store.incr_with_ttl(&attempts_key, OTP_TTL_SECONDS).await?;
    if attempts > OTP_MAX_ATTEMPTS {
        // Invalidate the live OTP on too many attempts.
        store.del(&otp_key(auth_user_id)).await?;
        return Err(AppError::BadRequest(
            "too many attempts, request a new code".into(),
        ));
    }

    // 2. Load the OTP payload keyed by the authenticated user.
    let otp_key = otp_key(auth_user_id);
    let stored = store.get(&otp_key).await?;
    let stored = stored.ok_or_else(|| AppError::BadRequest("verification code expired".into()))?;

    let payload: serde_json::Value = serde_json::from_str(&stored)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("otp payload corrupted: {e}")))?;

    let stored_phone = payload["phone"].as_str().unwrap_or_default();
    let stored_code = payload["code"].as_str().unwrap_or_default();

    // 3. The phone being verified must match the phone the OTP was issued to.
    if stored_phone != req.phone {
        return Err(AppError::BadRequest("invalid verification code".into()));
    }

    // 4. Constant-time code comparison.
    let codes_equal: bool = stored_code.as_bytes().ct_eq(req.code.as_bytes()).into();
    if !codes_equal {
        return Err(AppError::BadRequest("invalid verification code".into()));
    }

    Ok(OtpProof { auth_user_id })
}

/// Delete the accepted OTP and its attempt counter. Call only after the
/// follow-up DB write succeeded.
pub(super) async fn consume(store: &dyn EphemeralStore, proof: OtpProof) -> Result<(), AppError> {
    store.del(&otp_key(proof.auth_user_id)).await?;
    store.del(&attempts_key(proof.auth_user_id)).await?;
    Ok(())
}

/// Pins the key formats and TTLs against the pre-`EphemeralStore` code
/// (`eafe837`): a renamed key or changed TTL would silently reset every live
/// Redis counter/OTP on deploy.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SmsConfig;
    use crate::utils::ephemeral::recording::{Call, RecordingStore};

    #[tokio::test]
    async fn send_otp_keys_and_ttls() {
        let user = Uuid::now_v7();
        let store = RecordingStore::default();
        // Unroutable SMS endpoint: the send fails after every store call.
        let sms = SmsClient::new(
            &SmsConfig {
                twilio_account_sid: "AC-test".into(),
                twilio_auth_token: "token".into(),
                twilio_from_number: "+10000000000".into(),
                twilio_base_url: "http://127.0.0.1:1".into(),
            },
            reqwest::Client::new(),
        );
        let _ = send_otp(
            &store,
            &sms,
            user,
            OtpSendRequest {
                phone: "0912345678".into(),
            },
        )
        .await;
        assert_eq!(
            store.take_calls(),
            vec![
                Call::Incr(format!("otp_rate:{user}"), 3600),
                Call::SetEx(format!("otp:{user}"), 300),
                Call::Del(format!("otp_attempts:{user}")),
            ]
        );
    }

    #[tokio::test]
    async fn verify_otp_keys_and_ttls() {
        let user = Uuid::now_v7();
        let store = RecordingStore::default().with_value(
            &format!("otp:{user}"),
            r#"{"phone":"0912345678","code":"123456"}"#,
        );
        let proof = check(
            &store,
            user,
            &OtpVerifyRequest {
                phone: "0912345678".into(),
                code: "123456".into(),
            },
        )
        .await
        .expect("verify");
        consume(&store, proof).await.expect("consume");
        assert_eq!(
            store.take_calls(),
            vec![
                Call::Incr(format!("otp_attempts:{user}"), 300),
                Call::Get(format!("otp:{user}")),
                Call::Del(format!("otp:{user}")),
                Call::Del(format!("otp_attempts:{user}")),
            ]
        );
    }
}
