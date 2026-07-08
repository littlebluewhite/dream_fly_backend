use axum::{Json, extract::State};

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{SettingsResponse, UpdateSettingsRequest};
use super::service;

/// `GET /settings` — admin-only.
#[tracing::instrument(skip_all)]
pub async fn get_settings(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<SettingsResponse>, AppError> {
    auth.require_role("admin")?;
    let settings = service::get_settings(&state.db).await?;
    Ok(Json(settings))
}

/// `PUT /settings` — admin-only.
#[tracing::instrument(skip_all)]
pub async fn update_settings(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<UpdateSettingsRequest>,
) -> Result<Json<SettingsResponse>, AppError> {
    auth.require_role("admin")?;
    let settings = service::update_settings(&state.db, req).await?;
    Ok(Json(settings))
}
