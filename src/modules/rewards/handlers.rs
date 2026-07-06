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

use super::dto::{
    CreateRewardRequest, RedeemResponse, RedemptionListResponse, RewardListQuery,
    RewardListResponse, RewardResponse, UpdateRewardRequest,
};
use super::service;

/// `GET /rewards?all=` — member sees only `is_active`; `all=true` additionally
/// requires admin (see `service::list`).
#[tracing::instrument(skip_all)]
pub async fn list(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<RewardListQuery>,
) -> Result<Json<RewardListResponse>, AppError> {
    let result = service::list(&state.db, &auth, params.all.unwrap_or(false)).await?;
    Ok(Json(result))
}

/// `POST /rewards/{id}/redeem` — any authenticated member.
#[tracing::instrument(skip_all)]
pub async fn redeem(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<RedeemResponse>, AppError> {
    let result = service::redeem(&state.db, auth.user_id, id).await?;
    Ok(Json(result))
}

/// `GET /rewards/redemptions/me` — any authenticated member.
#[tracing::instrument(skip_all)]
pub async fn my_redemptions(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<PaginationParams>,
) -> Result<Json<RedemptionListResponse>, AppError> {
    let result = service::my_redemptions(&state.db, auth.user_id, &params).await?;
    Ok(Json(result))
}

/// `POST /rewards` — admin only.
#[tracing::instrument(skip_all)]
pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateRewardRequest>,
) -> Result<Json<RewardResponse>, AppError> {
    auth.require_role("admin")?;
    let result = service::create(&state.db, req).await?;
    Ok(Json(result))
}

/// `PATCH /rewards/{id}` — admin only.
#[tracing::instrument(skip_all)]
pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<UpdateRewardRequest>,
) -> Result<Json<RewardResponse>, AppError> {
    auth.require_role("admin")?;
    let result = service::update(&state.db, id, req).await?;
    Ok(Json(result))
}
