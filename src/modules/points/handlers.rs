use axum::{
    Json,
    extract::{Query, State},
};

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{AdjustPointsRequest, PointsAdjustmentResponse, PointsMeResponse};
use super::service;

/// Current points balance + paginated ledger history (newest first).
#[tracing::instrument(skip_all)]
pub async fn me(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PointsMeResponse>, AppError> {
    let result = service::get_my_points(&state.db, auth.user_id, &params).await?;
    Ok(Json(result))
}

/// `POST /points/adjustments` — admin-only (gated by `admin_router()`, see
/// `routes.rs`). See `service::adjust_points` for the CAS semantics
/// `expected_balance` gives this endpoint.
#[tracing::instrument(skip_all)]
pub async fn adjust(
    State(state): State<AppState>,
    _auth: AuthUser,
    ValidatedJson(req): ValidatedJson<AdjustPointsRequest>,
) -> Result<Json<PointsAdjustmentResponse>, AppError> {
    let result = service::adjust_points(&state.db, &req).await?;
    Ok(Json(result))
}
