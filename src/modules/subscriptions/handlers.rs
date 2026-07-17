use axum::{
    Json,
    extract::{Path, State},
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;

use super::dto::SubscriptionResponse;
use super::service;

/// This user's subscriptions, newest first (not paginated).
#[tracing::instrument(skip_all)]
pub async fn me(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<SubscriptionResponse>>, AppError> {
    let subscriptions = service::list_my_subscriptions(&state.db, auth.user_id).await?;
    Ok(Json(subscriptions))
}

/// Redeem one session from a subscription (admin or coach only). Enforced
/// by the `staff_api` route_layer (see `startup.rs`).
#[tracing::instrument(skip_all)]
pub async fn redeem(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<SubscriptionResponse>, AppError> {
    let id: Uuid = id
        .parse()
        .map_err(|_| AppError::BadRequest("invalid subscription id".into()))?;
    let updated = service::redeem(&state.db, id).await?;
    Ok(Json(updated))
}
