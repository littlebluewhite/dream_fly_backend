use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{
    CreatePostRequest, PostDetailResponse, PostListResponse, UpdatePostRequest,
};
use super::service;

/// List published posts (public)
#[tracing::instrument(skip_all)]
pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PostListResponse>, AppError> {
    let result = service::list_published(&state.db, &params).await?;
    Ok(Json(result))
}

/// Get a published post by slug or UUID (public).
/// Draft and archived posts are not visible through this endpoint.
#[tracing::instrument(skip_all)]
pub async fn get_by_slug_or_id(
    State(state): State<AppState>,
    Path(param): Path<String>,
) -> Result<Json<PostDetailResponse>, AppError> {
    let post = service::get_published_by_slug_or_id(&state.db, &param).await?;
    Ok(Json(post))
}

/// Create a new post (admin or coach)
#[tracing::instrument(skip_all)]
pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<CreatePostRequest>,
) -> Result<Json<PostDetailResponse>, AppError> {
    if !auth.is_admin() && !auth.roles.contains(&"coach".to_string()) {
        return Err(AppError::Forbidden("insufficient permissions".into()));
    }
    let post = service::create_post(&state.db, auth.user_id, req).await?;
    Ok(Json(post))
}

/// Update a post (admin or author)
#[tracing::instrument(skip_all)]
pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id_str): Path<String>,
    ValidatedJson(req): ValidatedJson<UpdatePostRequest>,
) -> Result<Json<PostDetailResponse>, AppError> {
    let id: Uuid = id_str
        .parse()
        .map_err(|_| AppError::BadRequest("invalid post id".into()))?;
    let post = service::update_post(&state.db, id, auth.user_id, auth.is_admin(), req).await?;
    Ok(Json(post))
}

/// Delete a post (admin only)
#[tracing::instrument(skip_all)]
pub async fn delete(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id_str): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require_role("admin")?;
    let id: Uuid = id_str
        .parse()
        .map_err(|_| AppError::BadRequest("invalid post id".into()))?;
    service::delete_post(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
