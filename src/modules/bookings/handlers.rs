use axum::{
    Json,
    extract::{Path, Query, State},
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::extractors::request_id::RequestId;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{BookingResponse, CreateBookingRequest, PaginatedBookingsResponse};
use super::service;

#[tracing::instrument(skip_all)]
pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    request_id: RequestId,
    ValidatedJson(req): ValidatedJson<CreateBookingRequest>,
) -> Result<Json<BookingResponse>, AppError> {
    let booking = service::create_booking(
        &state.db,
        &state.config.server,
        auth.user_id,
        req,
        request_id.0,
    )
    .await?;
    Ok(Json(booking))
}

#[tracing::instrument(skip_all)]
pub async fn my_bookings(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedBookingsResponse>, AppError> {
    let response = service::my_bookings(&state.db, auth.user_id, &params).await?;
    Ok(Json(response))
}

#[tracing::instrument(skip_all)]
pub async fn cancel(
    State(state): State<AppState>,
    auth: AuthUser,
    request_id: RequestId,
    Path(id): Path<Uuid>,
) -> Result<Json<BookingResponse>, AppError> {
    let booking = service::cancel_booking(
        &state.db,
        &state.config.server,
        &auth,
        id,
        request_id.0,
    )
    .await?;
    Ok(Json(booking))
}

#[tracing::instrument(skip_all)]
pub async fn list_all(
    State(state): State<AppState>,
    _auth: AuthUser,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedBookingsResponse>, AppError> {
    let response = service::list_all(&state.db, &params).await?;
    Ok(Json(response))
}
