use sqlx::PgPool;
use uuid::Uuid;

use crate::config::AuthConfig;
use crate::error::AppError;
use crate::kafka::events::UserRegisteredPayload;
use crate::kafka::outbox;
use crate::modules::auth::access::AccessCache;
use crate::modules::notifications::service as notify;
use crate::modules::permissions::repository as permissions_repository;
use crate::utils::email::EmailSender;
use crate::utils::ephemeral::EphemeralStore;
use crate::utils::google_oauth::GoogleIdentityProvider;
use crate::utils::password;
use crate::utils::sms::SmsClient;

use super::credentials;
use super::dto::{
    AuthResponse, ForgotPasswordRequest, GoogleAuthRequest, LoginRequest, MessageResponse,
    OtpSendRequest, OtpVerifyRequest, RefreshRequest, RegisterRequest, ResetPasswordRequest,
};
use super::linking;
use super::model::normalize_email;
use super::otp;
use super::provisioning;
use super::rate_limit;
use super::repository;
use super::reset_tokens;
use super::session;

pub async fn register(
    db: &PgPool,
    config: &AuthConfig,
    req: RegisterRequest,
    correlation_id: Option<String>,
) -> Result<AuthResponse, AppError> {
    // Hash password (on a blocking thread so the Argon2 CPU burst doesn't
    // stall async workers).
    let hashed = password::hash_for_storage(req.password.clone()).await?;

    // Wrap user creation + role assignment + outbox event + token
    // persistence in a single transaction so partial failures never leave
    // orphaned rows.
    let mut tx = db.begin().await?;

    // Insert user + assign "member" role + queue the user_registered event —
    // see `provisioning::create_account` for why these three are one atomic
    // step (including the email normalization). Rely on the DB unique
    // constraint for the duplicate check so existence enumeration is not
    // possible via race condition probing.
    let user = provisioning::create_account(
        &mut tx,
        provisioning::NewAccount {
            email: &req.email,
            name: &req.name,
            phone: None,
            birth_date: None,
            password_hash: &hashed,
        },
        correlation_id,
    )
    .await
    .map_err(|e| AppError::conflict_on_unique(e, "registration failed"))?;

    // If token generation fails here, the entire transaction — including the
    // event row `create_account` already queued — rolls back: no phantom
    // user row.
    let response = session::start(&mut tx, config, &user).await?;

    tx.commit().await?;

    // Welcome notification is written synchronously after commit.
    notify::user_welcomed(user.id).deliver(db).await;

    Ok(response)
}

pub async fn login(
    db: &PgPool,
    store: &dyn EphemeralStore,
    config: &AuthConfig,
    req: LoginRequest,
) -> Result<AuthResponse, AppError> {
    let email = normalize_email(&req.email);

    // Lockout check, account lookup, Argon2 verification, disabled-account
    // rejection, and failure counting/clearing all live in `credentials` —
    // the single owner of "email + password → verified User or 401" (see
    // that module's doc for the exact branch table).
    let user = credentials::verify(db, store, &email, &req.password).await?;

    let mut conn = db.acquire().await?;
    repository::update_last_login(&mut *conn, user.id).await?;

    session::start(&mut conn, config, &user).await
}

pub async fn google_auth(
    db: &PgPool,
    cache: &dyn AccessCache,
    config: &AuthConfig,
    google: &dyn GoogleIdentityProvider,
    req: GoogleAuthRequest,
    correlation_id: Option<String>,
) -> Result<AuthResponse, AppError> {
    // 1-2. Exchange the authorization code and verify the returned id_token
    //      (signature, iss/aud/exp, email_verified) — see
    //      `utils::google_oauth`. Nothing is written before this succeeds.
    let identity = google.verify_code(&req.code).await?;

    let name = identity
        .name
        .clone()
        .unwrap_or_else(|| identity.email.clone());
    let email = normalize_email(&identity.email);

    // 3. Wrap all mutations in a single transaction so partial failures
    //    never leave orphaned/inconsistent rows.
    let mut tx = db.begin().await?;

    // Look up the existing user by google_id, then — lazily, only on a
    // google_id miss — by email, so `linking::plan` can decide between
    // create, link, and refresh. See `linking`'s module doc for the full
    // decision and its truth table. Both lookups run on the tx's own
    // connection rather than borrowing a second one from the pool.
    let existing_by_google = repository::find_user_by_google_id(&mut *tx, &identity.sub).await?;
    let existing_by_email = if existing_by_google.is_none() {
        repository::find_user_by_email(&mut *tx, &email).await?
    } else {
        None
    };
    let plan = linking::plan(existing_by_google.as_ref(), existing_by_email.as_ref())?;

    let user = match plan.action {
        linking::LinkAction::Create | linking::LinkAction::Refresh => {
            repository::create_or_update_google_user_tx(
                &mut tx,
                &email,
                &name,
                &identity.sub,
                identity.picture.as_deref(),
            )
            .await?
        }
        linking::LinkAction::Link { user_id } => {
            repository::link_google_account_tx(
                &mut tx,
                user_id,
                &identity.sub,
                identity.picture.as_deref(),
            )
            .await?
        }
    };

    // 4. Assign "member" role — account birth (`Create`) only; Link/Refresh
    //    leave an existing account's roles alone (see `linking`'s module doc).
    //    Unlike `provisioning::create_account`, the witness is really flushed:
    //    the `ON CONFLICT (google_id)` upsert may have landed on a row a
    //    concurrent first login already committed, whose access may be cached.
    let dirty = if plan.grant_member {
        Some(permissions_repository::assign_role_by_name(&mut tx, user.id, "member").await?)
    } else {
        None
    };

    // 5. Update last_login
    repository::update_last_login(&mut *tx, user.id).await?;

    // 6. Generate tokens (inside the same tx)
    let response = session::start(&mut tx, config, &user).await?;

    // 7. Queue user_registered event atomically with the user row — see
    //    `linking`'s module doc for why Create and Link both emit it.
    if plan.emit_registered_event {
        outbox::insert_domain_event_tx(
            &mut tx,
            UserRegisteredPayload {
                user_id: user.id,
                email: user.email.clone(),
                name: user.name.clone(),
            },
            correlation_id,
        )
        .await?;
    }

    tx.commit().await?;

    if let Some(dirty) = dirty {
        dirty.flush(cache).await;
    }

    if plan.send_welcome {
        notify::user_welcomed(user.id).deliver(db).await;
    }

    Ok(response)
}

pub async fn refresh_token(
    db: &PgPool,
    config: &AuthConfig,
    req: RefreshRequest,
) -> Result<AuthResponse, AppError> {
    session::rotate(db, config, &req.refresh_token).await
}

pub async fn logout(db: &PgPool, config: &AuthConfig, req: RefreshRequest) -> Result<(), AppError> {
    session::end(db, config, &req.refresh_token).await
}

pub async fn send_otp(
    store: &dyn EphemeralStore,
    sms_client: &SmsClient,
    auth_user_id: Uuid,
    req: OtpSendRequest,
) -> Result<MessageResponse, AppError> {
    otp::send_otp(store, sms_client, auth_user_id, req).await
}

pub async fn verify_otp(
    db: &PgPool,
    store: &dyn EphemeralStore,
    auth_user_id: Uuid,
    req: OtpVerifyRequest,
) -> Result<MessageResponse, AppError> {
    let proof = otp::check(store, auth_user_id, &req).await?;

    // Update phone_verified now that the code has been confirmed; the code is
    // consumed only after this write succeeds (a 409 leaves it usable).
    repository::update_phone_verified(db, auth_user_id, &req.phone).await?;
    otp::consume(store, proof).await?;

    Ok(MessageResponse {
        message: "phone verified successfully".into(),
    })
}

pub async fn forgot_password(
    db: &PgPool,
    store: &dyn EphemeralStore,
    email_client: std::sync::Arc<dyn EmailSender>,
    background: &tokio_util::task::TaskTracker,
    req: ForgotPasswordRequest,
) -> Result<MessageResponse, AppError> {
    let success_msg = MessageResponse {
        message: "if that email exists, a password reset link has been sent".into(),
    };

    // 1. Find user by email (return success even if not found - prevents enumeration)
    let email_lower = normalize_email(&req.email);
    let user = repository::find_user_by_email(db, &email_lower).await?;

    let user = match user {
        Some(u) => u,
        None => return Ok(success_msg),
    };

    // 2. Per-account request rate limit so the password-reset email is not
    //    weaponized as an email-flooding vector against a known victim.
    if !rate_limit::forgot_allowed(store, &email_lower).await {
        // Swallow silently — do NOT leak to the attacker that they tripped
        // a per-account limit. Same response shape as the success branch.
        return Ok(success_msg);
    }

    // 3. Issue a fresh reset token, invalidating any previous outstanding
    //    one for this user so only the newest link works.
    let token = reset_tokens::issue(store, user.id).await?;

    // 4. Spawn the SMTP send so the handler returns at a roughly-constant
    //    latency regardless of whether the email exists. This flattens a
    //    subtle timing oracle: the "user not found" branch returns instantly
    //    while the "user found" branch would otherwise block on SMTP.
    //    `background` (an `AppState::background_tasks` clone) tracks the
    //    spawn for shutdown drain and test quiescence; the spawn must stay
    //    at this layer, synchronously registered before the handler
    //    returns — moving it into a deeper `async` call would break that
    //    guarantee.
    let user_email = user.email.clone();
    let client = email_client;
    background.spawn(async move {
        if let Err(e) = client.send_password_reset(&user_email, &token).await {
            tracing::error!(error = ?e, "password-reset email failed to send");
        }
    });

    Ok(success_msg)
}

pub async fn reset_password(
    db: &PgPool,
    store: &dyn EphemeralStore,
    req: ResetPasswordRequest,
) -> Result<MessageResponse, AppError> {
    // 1. Atomically consume the token (single-use) and clear the "current
    //    token" index for this user.
    let user_id = reset_tokens::consume(store, &req.token).await?;

    // 2. Hash new password
    let hashed = password::hash_for_storage(req.new_password.clone()).await?;

    // 3. Update password + revoke all tokens atomically so a partial failure
    //    cannot leave old sessions valid after a password change.
    let mut tx = db.begin().await?;
    repository::update_password_tx(&mut tx, user_id, &hashed).await?;
    session::end_all(&mut tx, user_id).await?;
    tx.commit().await?;

    Ok(MessageResponse {
        message: "password reset successfully".into(),
    })
}
