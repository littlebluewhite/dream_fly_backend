use axum::{
    Json,
    extract::{Path, Query, State},
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::state::AppState;

use super::dto::{NotificationResponse, UnreadCountResponse};
use super::service;

/// List notifications for the authenticated user
#[tracing::instrument(skip_all)]
pub async fn list(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<PaginationParams>,
) -> Result<Json<Vec<NotificationResponse>>, AppError> {
    let notifications =
        service::list_notifications(&state.db, auth.user_id, &params).await?;
    Ok(Json(notifications))
}

/// Get unread notification count for the authenticated user
#[tracing::instrument(skip_all)]
pub async fn unread_count(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<UnreadCountResponse>, AppError> {
    let result = service::get_unread_count(&state.db, auth.user_id).await?;
    Ok(Json(result))
}

/// Mark a notification as read
#[tracing::instrument(skip_all)]
pub async fn mark_read(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<NotificationResponse>, AppError> {
    let notification =
        service::mark_as_read(&state.db, id, auth.user_id).await?;
    Ok(Json(notification))
}
