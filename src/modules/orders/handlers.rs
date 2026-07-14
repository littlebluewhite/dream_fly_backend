use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::extractors::request_id::RequestId;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{
    AdminOrderListResponse, CheckoutRequest, OrderListResponse, OrderResponse,
    UpdateOrderStatusRequest,
};
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
    request_id: RequestId,
    // `Option<Json<T>>` rather than `ValidatedJson<T>`: axum's built-in
    // `OptionalFromRequest` impl for `Json` yields `None` when the request
    // has no `Content-Type` header at all (the existing no-body `POST
    // /orders` calls), instead of failing extraction the way a bare
    // `ValidatedJson<CheckoutRequest>` would. A present-but-non-JSON
    // content type still errors; a present JSON body (including `{}`, since
    // every `CheckoutRequest` field is `Option`) is parsed normally. This
    // must be the last handler argument (only one extractor per handler may
    // consume the body).
    body: Option<Json<CheckoutRequest>>,
) -> Result<Json<OrderResponse>, AppError> {
    let idempotency_key = extract_idempotency_key(&headers);
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let order = service::checkout(
        &state.db,
        auth.user_id,
        idempotency_key,
        req,
        request_id.0,
        &state.config.server,
        state.clock.now(),
    )
    .await?;
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
    let order = service::get_order(&state.db, id, &auth).await?;
    Ok(Json(order))
}

#[tracing::instrument(skip_all)]
pub async fn update_status(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
    request_id: RequestId,
    ValidatedJson(req): ValidatedJson<UpdateOrderStatusRequest>,
) -> Result<Json<OrderResponse>, AppError> {
    let order = service::update_order_status(&state.db, id, &req.status, request_id.0).await?;
    Ok(Json(order))
}

/// Paginated order list across all users (admin only).
#[tracing::instrument(skip_all)]
pub async fn admin_list_orders(
    State(state): State<AppState>,
    _auth: AuthUser,
    Query(params): Query<PaginationParams>,
) -> Result<Json<AdminOrderListResponse>, AppError> {
    let result = service::list_all_orders(&state.db, &params).await?;
    Ok(Json(result))
}
