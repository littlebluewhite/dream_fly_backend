use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{AssignRoleRequest, CreateRoleRequest, MessageResponse, RoleResponse};
use super::service;

#[tracing::instrument(skip_all)]
pub async fn list_roles(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<RoleResponse>>, AppError> {
    auth.require_role("admin")?;
    let roles = service::list_roles(&state.db).await?;
    Ok(Json(roles))
}

#[tracing::instrument(skip_all)]
pub async fn create_role(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateRoleRequest>,
) -> Result<Json<RoleResponse>, AppError> {
    auth.require_role("admin")?;
    let role = service::create_role(&state.db, &req.name, req.description.as_deref()).await?;
    Ok(Json(role))
}

#[tracing::instrument(skip_all)]
pub async fn assign_role(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(role_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<AssignRoleRequest>,
) -> Result<Json<MessageResponse>, AppError> {
    auth.require_role("admin")?;
    let mut redis = state.redis.clone();
    service::assign_role_to_user(&state.db, &mut redis, req.user_id, role_id).await?;
    Ok(Json(MessageResponse {
        message: "role assigned successfully".into(),
    }))
}

#[tracing::instrument(skip_all)]
pub async fn remove_role(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((role_id, user_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    auth.require_role("admin")?;
    let mut redis = state.redis.clone();
    service::remove_role_from_user(&state.db, &mut redis, user_id, role_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
