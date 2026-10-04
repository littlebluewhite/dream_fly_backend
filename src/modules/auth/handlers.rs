use axum::{Json, extract::State};

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::request_id::RequestId;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{
    AuthResponse, ForgotPasswordRequest, GoogleAuthRequest, LoginRequest, MessageResponse,
    OtpSendRequest, OtpVerifyRequest, RefreshRequest, RegisterRequest, ResetPasswordRequest,
};
use super::service;

#[tracing::instrument(skip_all)]
pub async fn register(
    State(state): State<AppState>,
    request_id: RequestId,
    ValidatedJson(req): ValidatedJson<RegisterRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    let response = service::register(
        &state.db,
        &state.config.auth,
        req,
        request_id.0,
        state.clock.now(),
    )
    .await?;
    Ok(Json(response))
}

#[tracing::instrument(skip_all)]
pub async fn login(
    State(state): State<AppState>,
    ValidatedJson(req): ValidatedJson<LoginRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    let response =
        service::login(&state.db, state.ephemeral.as_ref(), &state.config.auth, req).await?;
    Ok(Json(response))
}

#[tracing::instrument(skip_all)]
pub async fn google_auth(
    State(state): State<AppState>,
    request_id: RequestId,
    ValidatedJson(req): ValidatedJson<GoogleAuthRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    let response = service::google_auth(
        &state.db,
        state.access_cache.as_ref(),
        &state.config.auth,
        state.google_identity.as_ref(),
        req,
        request_id.0,
        state.clock.now(),
    )
    .await?;
    Ok(Json(response))
}

#[tracing::instrument(skip_all)]
pub async fn refresh(
    State(state): State<AppState>,
    ValidatedJson(req): ValidatedJson<RefreshRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    let response = service::refresh_token(&state.db, &state.config.auth, req).await?;
    Ok(Json(response))
}

#[tracing::instrument(skip_all)]
pub async fn logout(
    State(state): State<AppState>,
    ValidatedJson(req): ValidatedJson<RefreshRequest>,
) -> Result<Json<MessageResponse>, AppError> {
    service::logout(&state.db, &state.config.auth, req).await?;
    Ok(Json(MessageResponse {
        message: "logged out successfully".into(),
    }))
}

#[tracing::instrument(skip_all)]
pub async fn send_otp(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<OtpSendRequest>,
) -> Result<Json<MessageResponse>, AppError> {
    let response = service::send_otp(
        state.ephemeral.as_ref(),
        state.sms_client.as_ref(),
        auth.user_id,
        req,
    )
    .await?;
    Ok(Json(response))
}

#[tracing::instrument(skip_all)]
pub async fn verify_otp(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<OtpVerifyRequest>,
) -> Result<Json<MessageResponse>, AppError> {
    let response =
        service::verify_otp(&state.db, state.ephemeral.as_ref(), auth.user_id, req).await?;
    Ok(Json(response))
}

#[tracing::instrument(skip_all)]
pub async fn forgot_password(
    State(state): State<AppState>,
    ValidatedJson(req): ValidatedJson<ForgotPasswordRequest>,
) -> Result<Json<MessageResponse>, AppError> {
    let response = service::forgot_password(
        &state.db,
        state.ephemeral.as_ref(),
        state.email_client.clone(),
        &state.background_tasks,
        req,
    )
    .await?;
    Ok(Json(response))
}

#[tracing::instrument(skip_all)]
pub async fn reset_password(
    State(state): State<AppState>,
    ValidatedJson(req): ValidatedJson<ResetPasswordRequest>,
) -> Result<Json<MessageResponse>, AppError> {
    let response = service::reset_password(&state.db, state.ephemeral.as_ref(), req).await?;
    Ok(Json(response))
}
