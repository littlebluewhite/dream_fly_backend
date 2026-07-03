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

use super::dto::{AddCartItemRequest, CartResponse, UpdateCartItemRequest};
use super::service;

#[tracing::instrument(skip_all)]
pub async fn get_cart(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<CartResponse>, AppError> {
    let cart = service::get_cart(&state.db, auth.user_id).await?;
    Ok(Json(cart))
}

#[tracing::instrument(skip_all)]
pub async fn add_item(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<AddCartItemRequest>,
) -> Result<Json<CartResponse>, AppError> {
    let quantity = req.quantity.unwrap_or(1);
    let cart = service::add_item(&state.db, auth.user_id, &req.item_type, req.item_id, quantity)
        .await?;
    Ok(Json(cart))
}

#[tracing::instrument(skip_all)]
pub async fn update_quantity(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(item_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<UpdateCartItemRequest>,
) -> Result<Json<CartResponse>, AppError> {
    let cart = service::update_quantity(&state.db, auth.user_id, item_id, req.quantity).await?;
    Ok(Json(cart))
}

#[tracing::instrument(skip_all)]
pub async fn remove_item(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(item_id): Path<Uuid>,
) -> Result<Json<CartResponse>, AppError> {
    let cart = service::remove_item(&state.db, auth.user_id, item_id).await?;
    Ok(Json(cart))
}

#[tracing::instrument(skip_all)]
pub async fn clear(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<StatusCode, AppError> {
    service::clear(&state.db, auth.user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
