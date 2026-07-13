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
    _auth: AuthUser,
) -> Result<Json<SettingsResponse>, AppError> {
    let settings = service::get_settings(&state.db).await?;
    Ok(Json(settings))
}

/// `PUT /settings` — admin-only.
#[tracing::instrument(skip_all)]
pub async fn update_settings(
    State(state): State<AppState>,
    _auth: AuthUser,
    ValidatedJson(req): ValidatedJson<UpdateSettingsRequest>,
) -> Result<Json<SettingsResponse>, AppError> {
    let settings = service::update_settings(&state.db, req).await?;
    Ok(Json(settings))
}
