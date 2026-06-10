use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{OrderListResponse, OrderResponse, UpdateOrderStatusRequest};
use super::service;

/// Read the `Idempotency-Key` header (if present). We bound the length to
/// prevent a 10MB key from blowing up our unique index, and we reject any
/// non-ASCII/non-printable characters.
fn extract_idempotency_key(headers: &HeaderMap) -> Option<String> {
    let value = headers.get("idempotency-key")?.to_str().ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.len() > 128
        || !trimmed
            .chars()
            .all(|c| c.is_ascii_graphic() || c == '-' || c == '_')
    {
        return None;
    }
    Some(trimmed.to_string())
}

#[tracing::instrument(skip_all)]
pub async fn checkout(
    State(state): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
) -> Result<Json<OrderResponse>, AppError> {
    let idempotency_key = extract_idempotency_key(&headers);
    let order = service::checkout(&state.db, auth.user_id, idempotency_key).await?;
    Ok(Json(order))
}

#[tracing::instrument(skip_all)]
pub async fn my_orders(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<PaginationParams>,
) -> Result<Json<OrderListResponse>, AppError> {
    let list = service::my_orders(&state.db, auth.user_id, params.page, params.per_page).await?;
    Ok(Json(list))
}

#[tracing::instrument(skip_all)]
pub async fn get_order(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<OrderResponse>, AppError> {
    let order = service::get_order(&state.db, id, auth.user_id, auth.is_admin()).await?;
    Ok(Json(order))
}

#[tracing::instrument(skip_all)]
pub async fn update_status(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<UpdateOrderStatusRequest>,
) -> Result<Json<OrderResponse>, AppError> {
    auth.require_role("admin")?;
    let order = service::update_order_status(&state.db, id, &req.status).await?;
    Ok(Json(order))
}
