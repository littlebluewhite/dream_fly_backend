use axum::{
    Json,
    extract::{Query, State},
};

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::state::AppState;

use super::dto::PointsMeResponse;
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
