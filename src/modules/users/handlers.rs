use axum::{
    Json,
    extract::{Path, Query, State},
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{CreateUserRequest, UpdateProfileRequest, UpdateUserRequest, UserListResponse, UserResponse};
use super::service;

#[tracing::instrument(skip_all)]
pub async fn me(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<UserResponse>, AppError> {
    let response = service::get_me(&state.db, auth.user_id).await?;
    Ok(Json(response))
}

#[tracing::instrument(skip_all)]
pub async fn update_me(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<UpdateProfileRequest>,
) -> Result<Json<UserResponse>, AppError> {
    let response = service::update_me(&state.db, auth.user_id, req).await?;
    Ok(Json(response))
}

#[tracing::instrument(skip_all)]
pub async fn list(
    State(state): State<AppState>,
    _auth: AuthUser,
    Query(params): Query<PaginationParams>,
) -> Result<Json<UserListResponse>, AppError> {
    let response = service::list_users(&state.db, &params).await?;
    Ok(Json(response))
}

#[tracing::instrument(skip_all)]
pub async fn get_user(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<UserResponse>, AppError> {
    let response = service::get_user(&state.db, id).await?;
    Ok(Json(response))
}

#[tracing::instrument(skip_all)]
pub async fn create(
    State(state): State<AppState>,
    _auth: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateUserRequest>,
) -> Result<Json<UserResponse>, AppError> {
    let response = service::create_user(&state.db, req).await?;
    Ok(Json(response))
}

#[tracing::instrument(skip_all)]
pub async fn admin_update(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<UpdateUserRequest>,
) -> Result<Json<UserResponse>, AppError> {
    let mut redis = state.redis.clone();
    let response = service::admin_update_user(&state.db, &mut redis, id, req).await?;
    Ok(Json(response))
}
