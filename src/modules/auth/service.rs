use chrono::{Duration, Utc};
use redis::AsyncCommands;
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::{AppConfig, AuthConfig};
use crate::error::AppError;
use crate::kafka::events::{UserRegisteredPayload, event_types, topics};
use crate::kafka::outbox;
use crate::modules::notifications::service as notify;
use crate::modules::permissions::repository as permissions_repository;
use crate::utils::email::EmailSender;
use crate::utils::google_oauth;
use crate::utils::jwt;
use crate::utils::password;
use crate::utils::sms::SmsSender;

use super::dto::{
    AuthResponse, ForgotPasswordRequest, GoogleAuthRequest, LoginRequest, MessageResponse,
    OtpSendRequest, OtpVerifyRequest, RefreshRequest, RegisterRequest, ResetPasswordRequest,
    UserResponse,
};
use super::otp;
use super::rate_limit;
use super::repository;

// Helper: build AuthResponse from a user. The refresh token is hashed with
// SHA-256 before being persisted so a database compromise does not leak live
// refresh credentials.
async fn build_auth_response(
    db: &PgPool,
    config: &AuthConfig,
    user: &super::model::User,
) -> Result<AuthResponse, AppError> {
    let access_token = jwt::encode_access_token(config, user.id, &user.email)?;
    let refresh_token = jwt::encode_refresh_token(config, user.id)?;

    let expires_at = Utc::now() + Duration::days(config.jwt_refresh_expiration_days as i64);
    let token_hash = jwt::hash_token(&refresh_token);

    repository::save_refresh_token(db, user.id, &token_hash, expires_at).await?;

    let roles = permissions_repository::find_role_names_by_user(db, user.id).await?;

    Ok(AuthResponse {
        access_token,
        refresh_token,
        user: UserResponse {
            roles,
            ..UserResponse::from(user.clone())
        },
    })
}

/// Transactional variant — persists the refresh token inside an existing tx.
async fn build_auth_response_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    config: &AuthConfig,
    user: &super::model::User,
) -> Result<AuthResponse, AppError> {
    let access_token = jwt::encode_access_token(config, user.id, &user.email)?;
    let refresh_token = jwt::encode_refresh_token(config, user.id)?;

    let expires_at = Utc::now() + Duration::days(config.jwt_refresh_expiration_days as i64);
    let token_hash = jwt::hash_token(&refresh_token);

    repository::save_refresh_token_tx(tx, user.id, &token_hash, expires_at).await?;

    let roles = permissions_repository::find_role_names_by_user_tx(tx, user.id).await?;

    Ok(AuthResponse {
        access_token,
        refresh_token,
        user: UserResponse {
            roles,
            ..UserResponse::from(user.clone())
        },
    })
}

pub async fn register(
    db: &PgPool,
    config: &AuthConfig,
    req: RegisterRequest,
) -> Result<AuthResponse, AppError> {
    // Normalize email to lowercase to avoid IDOR via case-insensitive
    // duplicates later on.
    let email = req.email.to_lowercase();

    // Hash password (on a blocking thread so the Argon2 CPU burst doesn't
    // stall async workers).
    let hashed = password::hash_password(req.password.clone())
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("password hash error: {e}")))?;

    // Wrap user creation + role assignment + token persistence in a single
    // transaction so partial failures never leave orphaned rows.
    let mut tx = db.begin().await?;

    // Insert user. Rely on the DB unique constraint for the duplicate check
    // so existence enumeration is not possible via race condition probing.
    let user = repository::create_user_tx(&mut tx, &email, &req.name, &hashed)
        .await
        .map_err(|e| AppError::conflict_on_unique(e, "registration failed"))?;

    // Assign "member" role
    repository::assign_role_tx(&mut tx, user.id, "member").await?;

    // Build tokens before publishing: if token generation fails the entire
    // transaction rolls back — no phantom user row.
    let response = build_auth_response_tx(&mut tx, config, &user).await?;

    // Queue the user_registered event inside the same tx. The event is for
    // audit / external integration only — the welcome notification is now
    // written synchronously post-commit via `notify::user_welcomed` below,
    // not derived from this event.
    outbox::insert_event_tx(
        &mut tx,
        topics::USERS_REGISTERED,
        event_types::USER_REGISTERED,
        &user.id.to_string(),
        UserRegisteredPayload {
            user_id: user.id,
            email: user.email.clone(),
            name: user.name.clone(),
        },
        None,
    )
    .await?;

    tx.commit().await?;

    // Welcome notification is written synchronously after commit.
    notify::user_welcomed(db, user.id).await;

    Ok(response)
}

pub async fn login(
    db: &PgPool,
    redis: &mut redis::aio::ConnectionManager,
    config: &AuthConfig,
    req: LoginRequest,
) -> Result<AuthResponse, AppError> {
    let email = req.email.to_lowercase();

    // 1. Per-email failed-attempt check. A residential-proxy attacker with
    //    thousands of IPs would sail past the per-IP rate limit, so we
    //    additionally throttle on the target account regardless of source.
    let fail_key = format!("login_fail:{email}");
    let failures = rate_limit::read_count(redis, &fail_key).await;
    if failures >= rate_limit::LOGIN_MAX_ATTEMPTS {
        // Same error code as bad credentials to avoid confirming the lockout
        // to an attacker. A defender inspecting logs will see the counter.
        tracing::warn!(%email, failures, "login blocked: lockout threshold reached");
        return Err(AppError::Unauthorized);
    }

    // 2. Always return the same Unauthorized message so we don't leak:
    //    whether the email exists, whether the account is Google-linked,
    //    whether the password is correct.
    let user_opt = repository::find_user_by_email(db, &email).await?;

    let user = match user_opt {
        Some(u) => u,
        None => {
            rate_limit::bump_login_failure(redis, &fail_key).await;
            return Err(AppError::Unauthorized);
        }
    };

    let hash = match user.password_hash.as_deref() {
        Some(h) => h,
        None => {
            rate_limit::bump_login_failure(redis, &fail_key).await;
            return Err(AppError::Unauthorized);
        }
    };

    let valid = password::verify_password(req.password.clone(), hash.to_string())
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("password verify error: {e}")))?;
    if !valid {
        rate_limit::bump_login_failure(redis, &fail_key).await;
        return Err(AppError::Unauthorized);
    }

    // Reject disabled accounts at login time as well — otherwise a
    // previously-issued refresh token would still work until the is_active
    // cache expires.
    if !user.is_active {
        return Err(AppError::Unauthorized);
    }

    // 3. Success — clear the failure counter and log last_login.
    rate_limit::clear_count(redis, &fail_key).await;

    repository::update_last_login(db, user.id).await?;

    build_auth_response(db, config, &user).await
}

#[derive(serde::Deserialize)]
struct GoogleTokenResponse {
    id_token: String,
}

pub async fn google_auth(
    db: &PgPool,
    config: &AppConfig,
    http_client: &reqwest::Client,
    req: GoogleAuthRequest,
) -> Result<AuthResponse, AppError> {
    // 1. Exchange authorization code for tokens (including id_token).
    //    The endpoint is configurable so integration tests can redirect to
    //    a `wiremock` server instead of reaching real Google.
    let token_response = http_client
        .post(config.auth.google_token_url.as_str())
        .form(&[
            ("code", req.code.as_str()),
            ("client_id", config.auth.google_client_id.as_str()),
            ("client_secret", config.auth.google_client_secret.as_str()),
            ("redirect_uri", config.auth.google_redirect_url.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Google token exchange failed: {e}")))?;

    if !token_response.status().is_success() {
        // Log the upstream detail server-side, but only return a generic message.
        let body = token_response.text().await.unwrap_or_default();
        tracing::warn!(body = %body, "Google token exchange returned non-success");
        return Err(AppError::BadRequest("Google authentication failed".into()));
    }

    let token_data: GoogleTokenResponse = token_response.json().await.map_err(|e| {
        AppError::Internal(anyhow::anyhow!(
            "failed to parse Google token response: {e}"
        ))
    })?;

    // 2. Verify id_token signature against Google's published JWKS and
    //    enforce iss/aud/exp/email_verified. This is defense-in-depth over
    //    the TLS channel check: if this helper is ever reused in a flow
    //    where the token is not fetched directly from Google, signature
    //    verification is the only thing standing between us and forgery.
    let claims = google_oauth::verify_google_id_token(
        http_client,
        &token_data.id_token,
        &config.auth.google_client_id,
    )
    .await?;

    let name = claims.name.clone().unwrap_or_else(|| claims.email.clone());
    let email = claims.email.to_lowercase();

    // 3. Wrap all mutations in a single transaction so partial failures
    //    never leave orphaned/inconsistent rows.
    let mut tx = db.begin().await?;

    // Check whether the user already exists (by google_id or by email) so
    // we can decide between create, link, or update — and know whether to
    // publish a user_registered event afterwards.
    let existing_by_google = repository::find_user_by_google_id(db, &claims.sub).await?;
    let existed = existing_by_google.is_some();

    // Set ONLY in the genuinely brand-new branch below. `!existed` is not a
    // proxy for "new user": it also covers linking Google to a pre-existing
    // password account (which already got a welcome at register time).
    let mut created_new_user = false;

    let user = if existing_by_google.is_some() {
        // Returning Google user — update their profile inside the tx.
        repository::create_or_update_google_user_tx(
            &mut tx,
            &email,
            &name,
            &claims.sub,
            claims.picture.as_deref(),
        )
        .await?
    } else {
        // First-time Google login. Check if a password-registered user
        // already owns this email. If so, link the Google account rather
        // than hitting the email UNIQUE constraint with an unhandled 500.
        if let Some(existing_by_email) = repository::find_user_by_email_tx(&mut tx, &email).await? {
            if existing_by_email.google_id.is_some() {
                // Different google_id but same email — conflict.
                return Err(AppError::Conflict(
                    "email already associated with another account".into(),
                ));
            }
            // Link Google account to the existing password user — not a new
            // user, so no welcome.
            repository::link_google_account_tx(
                &mut tx,
                existing_by_email.id,
                &claims.sub,
                claims.picture.as_deref(),
            )
            .await?
        } else {
            // Brand-new user via Google — the only place the flag is set.
            created_new_user = true;
            repository::create_or_update_google_user_tx(
                &mut tx,
                &email,
                &name,
                &claims.sub,
                claims.picture.as_deref(),
            )
            .await?
        }
    };

    // 4. Assign "member" role (idempotent)
    repository::assign_role_tx(&mut tx, user.id, "member").await?;

    // 5. Update last_login
    repository::update_last_login_tx(&mut tx, user.id).await?;

    // 6. Generate tokens (inside the same tx)
    let response = build_auth_response_tx(&mut tx, &config.auth, &user).await?;

    // 7. First-time login only: queue user_registered event atomically
    //    with the newly-created (or newly-linked) user row.
    if !existed {
        outbox::insert_event_tx(
            &mut tx,
            topics::USERS_REGISTERED,
            event_types::USER_REGISTERED,
            &user.id.to_string(),
            UserRegisteredPayload {
                user_id: user.id,
                email: user.email.clone(),
                name: user.name.clone(),
            },
            None,
        )
        .await?;
    }

    tx.commit().await?;

    // Deliberate asymmetry: the user_registered event above fires for BOTH a
    // freshly-created user and a Google-link of an existing account (the
    // pre-existing wire contract, out of scope to change), but the welcome
    // notification fires only for genuinely new users — a linked account was
    // already welcomed at register time.
    if created_new_user {
        notify::user_welcomed(db, user.id).await;
    }

    Ok(response)
}

pub async fn refresh_token(
    db: &PgPool,
    config: &AuthConfig,
    req: RefreshRequest,
) -> Result<AuthResponse, AppError> {
    // 1. Decode refresh token JWT (verifies signature + claims)
    let claims = jwt::decode_refresh_token(config, &req.refresh_token)?;

    // 2. Look up by SHA-256 hash, never by raw token
    let token_hash = jwt::hash_token(&req.refresh_token);

    // 3. Atomically: find + revoke old, create new — everything in one tx
    let mut tx = db.begin().await?;

    let stored = repository::find_refresh_token_tx(&mut tx, &token_hash)
        .await?
        .ok_or(AppError::Unauthorized)?;

    if stored.revoked {
        // Reuse detection: if a revoked token is seen again, treat it as a
        // stolen-token replay and invalidate the user's entire token family.
        //
        // Log at ERROR with a distinct `security_event` field so SIEM rules
        // can alert on this specifically — token reuse is a strong
        // indicator of credential theft rather than a benign retry.
        let _ = repository::revoke_all_user_tokens_tx(&mut tx, stored.user_id).await;
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

    repository::revoke_refresh_token_tx(&mut tx, &token_hash).await?;

    // 4. Load user from JWT sub
    let user_id: Uuid = claims.sub.parse().map_err(|_| AppError::Unauthorized)?;
    if user_id != stored.user_id {
        // JWT sub does not match stored record — refuse.
        return Err(AppError::Unauthorized);
    }

    let user = repository::find_user_by_id_tx(&mut tx, user_id)
        .await?
        .ok_or(AppError::Unauthorized)?;

    // Deactivated users cannot mint new access tokens, even with a valid
    // refresh token. This closes the window where a disabled user could
    // keep refreshing until the cached is_active flag expires.
    if !user.is_active {
        repository::revoke_all_user_tokens_tx(&mut tx, user.id).await?;
        tx.commit().await?;
        return Err(AppError::Unauthorized);
    }

    // 5. Issue + persist the new refresh token, still inside the same tx
    let access_token = jwt::encode_access_token(config, user.id, &user.email)?;
    let new_refresh = jwt::encode_refresh_token(config, user.id)?;
    let new_hash = jwt::hash_token(&new_refresh);
    let new_expires = Utc::now() + Duration::days(config.jwt_refresh_expiration_days as i64);

    repository::save_refresh_token_tx(&mut tx, user.id, &new_hash, new_expires).await?;

    tx.commit().await?;

    let roles = permissions_repository::find_role_names_by_user(db, user.id).await?;

    Ok(AuthResponse {
        access_token,
        refresh_token: new_refresh,
        user: UserResponse {
            roles,
            ..UserResponse::from(user)
        },
    })
}

pub async fn logout(db: &PgPool, config: &AuthConfig, req: RefreshRequest) -> Result<(), AppError> {
    // Verify the JWT first so random strings cannot be used to revoke tokens.
    // If it doesn't parse, treat as success so logout is idempotent client-side.
    if jwt::decode_refresh_token(config, &req.refresh_token).is_err() {
        return Ok(());
    }

    let token_hash = jwt::hash_token(&req.refresh_token);
    repository::revoke_refresh_token(db, &token_hash).await?;
    Ok(())
}

pub async fn send_otp(
    redis: &mut redis::aio::ConnectionManager,
    sms_client: &dyn SmsSender,
    auth_user_id: Uuid,
    req: OtpSendRequest,
) -> Result<MessageResponse, AppError> {
    otp::send_otp(redis, sms_client, auth_user_id, req).await
}

pub async fn verify_otp(
    db: &PgPool,
    redis: &mut redis::aio::ConnectionManager,
    auth_user_id: Uuid,
    req: OtpVerifyRequest,
) -> Result<MessageResponse, AppError> {
    otp::verify_otp(redis, auth_user_id, &req).await?;

    // Update phone_verified now that the code has been confirmed.
    repository::update_phone_verified(db, auth_user_id, &req.phone).await?;

    Ok(MessageResponse {
        message: "phone verified successfully".into(),
    })
}

pub async fn forgot_password(
    db: &PgPool,
    redis: &mut redis::aio::ConnectionManager,
    email_client: std::sync::Arc<dyn EmailSender>,
    req: ForgotPasswordRequest,
) -> Result<MessageResponse, AppError> {
    let success_msg = MessageResponse {
        message: "if that email exists, a password reset link has been sent".into(),
    };

    // 1. Find user by email (return success even if not found - prevents enumeration)
    let email_lower = req.email.to_lowercase();
    let user = repository::find_user_by_email(db, &email_lower).await?;

    let user = match user {
        Some(u) => u,
        None => return Ok(success_msg),
    };

    // 2. Per-account request rate limit so the password-reset email is not
    //    weaponized as an email-flooding vector against a known victim.
    let forgot_rate_key = format!("forgot_rate:{email_lower}");
    let count = rate_limit::bump_count_best_effort(redis, &forgot_rate_key, 3600i64).await;
    if count > 3 {
        // Swallow silently — do NOT leak to the attacker that they tripped
        // a per-account limit. Same response shape as the success branch.
        return Ok(success_msg);
    }

    // 3. Generate a URL-safe random token. URL_SAFE_NO_PAD avoids `=`/`+`/`/`
    //    which get URL-encoded inconsistently by email clients.
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use rand::Rng;

    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);

    // 4. Invalidate any previous outstanding reset token for this user so
    //    only the most recent link works.
    let index_key = format!("password_reset_current:{}", user.id);
    let previous: Option<String> = redis.get(&index_key).await?;
    if let Some(prev_token) = previous {
        let _: () = redis.del(format!("password_reset:{prev_token}")).await?;
    }

    // 5. Store token -> user_id with 15-minute TTL.
    let key = format!("password_reset:{token}");
    redis::cmd("SET")
        .arg(&key)
        .arg(user.id.to_string())
        .arg("EX")
        .arg(rate_limit::PASSWORD_RESET_TTL_SECONDS)
        .query_async::<()>(redis)
        .await?;

    // 6. Track the current token for this user so the next request can
    //    invalidate it.
    redis::cmd("SET")
        .arg(&index_key)
        .arg(&token)
        .arg("EX")
        .arg(rate_limit::PASSWORD_RESET_TTL_SECONDS)
        .query_async::<()>(redis)
        .await?;

    // 7. Spawn the SMTP send so the handler returns at a roughly-constant
    //    latency regardless of whether the email exists. This flattens a
    //    subtle timing oracle: the "user not found" branch returns instantly
    //    while the "user found" branch would otherwise block on SMTP.
    let user_email = user.email.clone();
    let client = email_client;
    tokio::spawn(async move {
        if let Err(e) = client.send_password_reset(&user_email, &token).await {
            tracing::error!(error = ?e, "password-reset email failed to send");
        }
    });

    Ok(success_msg)
}

pub async fn reset_password(
    db: &PgPool,
    redis: &mut redis::aio::ConnectionManager,
    req: ResetPasswordRequest,
) -> Result<MessageResponse, AppError> {
    // 1. Atomically consume the token: GETDEL returns the old value and
    //    deletes the key in one round-trip, preventing double-use races.
    let key = format!("password_reset:{}", req.token);
    let user_id_str: Option<String> = redis::cmd("GETDEL").arg(&key).query_async(redis).await?;

    let user_id_str =
        user_id_str.ok_or_else(|| AppError::BadRequest("invalid or expired token".into()))?;

    let user_id: Uuid = user_id_str
        .parse()
        .map_err(|_| AppError::Internal(anyhow::anyhow!("invalid user_id in reset token")))?;

    // 2. Also clear the "current token" index for this user.
    let _: () = redis
        .del::<_, ()>(format!("password_reset_current:{}", user_id))
        .await?;

    // 3. Hash new password
    let hashed = password::hash_password(req.new_password.clone())
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("password hash error: {e}")))?;

    // 4. Update password + revoke all tokens atomically so a partial failure
    //    cannot leave old sessions valid after a password change.
    let mut tx = db.begin().await?;
    repository::update_password_tx(&mut tx, user_id, &hashed).await?;
    repository::revoke_all_user_tokens_tx(&mut tx, user_id).await?;
    tx.commit().await?;

    Ok(MessageResponse {
        message: "password reset successfully".into(),
    })
}
