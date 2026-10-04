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
use super::idempotency::IdempotencyKey;
use super::service;

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
    let idempotency_key = IdempotencyKey::from_headers(&headers)?;
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let order = service::checkout(
        &state.db,
        auth.user_id,
        idempotency_key,
        req,
        request_id.0,
        state.studio_now(),
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
    let list = service::my_orders(&state.db, auth.user_id, &params).await?;
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
    Path(id): Path<Uuid>,
    request_id: RequestId,
    ValidatedJson(req): ValidatedJson<UpdateOrderStatusRequest>,
) -> Result<Json<OrderResponse>, AppError> {
    let order =
        service::update_order_status(&state.db, id, &req.status, request_id.0, state.clock.now())
            .await?;
    Ok(Json(order))
}

/// Paginated order list across all users (admin only).
#[tracing::instrument(skip_all)]
pub async fn admin_list_orders(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<AdminOrderListResponse>, AppError> {
    let result = service::list_all_orders(&state.db, &params).await?;
    Ok(Json(result))
}
