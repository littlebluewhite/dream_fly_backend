//! OTP (One-Time Password) send/verify lifecycle — the complete
//! phone-verification flow: per-user request-rate check, 6-digit code
//! generation, Redis-scoped storage, SMS dispatch, and verification
//! (attempt-count bump + code compare + cleanup).
//!
//! The verify-attempt-count-bump and OTP-invalidation-on-too-many-attempts
//! in `verify_otp` below is a single invariant — bumping the attempt
//! counter and (past the threshold) deleting the live OTP are two halves of
//! one "fail closed on brute force" decision, so they are never split
//! across functions or files.
//!
//! `update_phone_verified`'s DB write stays in `service.rs`: this module
//! has no `PgPool` and never touches the database — it only reports
//! whether verification succeeded.

use redis::AsyncCommands;
use uuid::Uuid;

use crate::error::AppError;
use crate::utils::sms::SmsClient;

use super::dto::{MessageResponse, OtpSendRequest, OtpVerifyRequest};
use super::rate_limit;

pub(super) async fn send_otp(
    redis: &mut redis::aio::ConnectionManager,
    sms_client: &SmsClient,
    auth_user_id: Uuid,
    req: OtpSendRequest,
) -> Result<MessageResponse, AppError> {
    use rand::RngExt;

    // 1. Per-user rate limit — costs money if unbounded.
    let rate_key = format!("otp_rate:{}", auth_user_id);
    let count = crate::utils::redis_counter::incr_with_ttl(
        redis,
        &rate_key,
        rate_limit::OTP_RATE_LIMIT_TTL,
    )
    .await?;
    if count > rate_limit::OTP_REQUESTS_PER_HOUR {
        return Err(AppError::BadRequest(
            "too many verification requests, try again later".into(),
        ));
    }

    // 2. Generate 6-digit random code
    let code: u32 = rand::rng().random_range(100000..=999999);
    let code_str = format!("{:06}", code);

    // 3. Store in Redis under a user-scoped key so a user cannot verify a
    //    phone they did not initiate. Store {phone,code} as a JSON payload.
    let payload = serde_json::json!({
        "phone": req.phone,
        "code": code_str,
    })
    .to_string();

    let otp_key = format!("otp:{}", auth_user_id);
    redis::cmd("SET")
        .arg(&otp_key)
        .arg(&payload)
        .arg("EX")
        .arg(rate_limit::OTP_TTL_SECONDS)
        .query_async::<()>(redis)
        .await?;

    // Reset attempt counter whenever a fresh OTP is issued.
    let attempts_key = format!("otp_attempts:{}", auth_user_id);
    let _: () = redis.del(&attempts_key).await?;

    // 4. Send SMS
    sms_client.send_otp(&req.phone, &code_str).await?;

    Ok(MessageResponse {
        message: "verification code sent".into(),
    })
}

/// Verify an OTP for `auth_user_id`. Returns `Ok(())` on success; the
/// caller (`service::verify_otp`) is responsible for the DB write that
/// marks the phone verified.
pub(super) async fn verify_otp(
    redis: &mut redis::aio::ConnectionManager,
    auth_user_id: Uuid,
    req: &OtpVerifyRequest,
) -> Result<(), AppError> {
    use subtle::ConstantTimeEq;

    // 1. Bump the per-user attempt counter first — fail-closed on brute force.
    let attempts_key = format!("otp_attempts:{}", auth_user_id);
    let attempts = crate::utils::redis_counter::incr_with_ttl(
        redis,
        &attempts_key,
        rate_limit::OTP_TTL_SECONDS,
    )
    .await?;
    if attempts > rate_limit::OTP_MAX_ATTEMPTS {
        // Invalidate the live OTP on too many attempts.
        let otp_key = format!("otp:{}", auth_user_id);
        let _: () = redis.del(&otp_key).await?;
        return Err(AppError::BadRequest(
            "too many attempts, request a new code".into(),
        ));
    }

    // 2. Load the OTP payload keyed by the authenticated user.
    let otp_key = format!("otp:{}", auth_user_id);
    let stored: Option<String> = redis.get(&otp_key).await?;
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

    // 5. Success — delete OTP and attempt counter.
    let _: () = redis.del(&otp_key).await?;
    let _: () = redis.del(&attempts_key).await?;

    Ok(())
}
