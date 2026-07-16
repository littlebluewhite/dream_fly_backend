use axum::{
    Json,
    extract::{Path, Query, State},
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{
    AvailabilityQuery, CreateSlotsRequest, DaySchedule, ScheduleQuery, TimeSlotResponse,
    UpdateSlotRequest,
};
use super::service;

#[tracing::instrument(skip_all)]
pub async fn get_monthly(
    State(state): State<AppState>,
    Query(params): Query<ScheduleQuery>,
) -> Result<Json<Vec<DaySchedule>>, AppError> {
    let schedule = service::get_monthly_schedule(&state.db, params).await?;
    Ok(Json(schedule))
}

#[tracing::instrument(skip_all)]
pub async fn get_availability(
    State(state): State<AppState>,
    Query(params): Query<AvailabilityQuery>,
) -> Result<Json<Vec<TimeSlotResponse>>, AppError> {
    let slots = service::get_availability(&state.db, params).await?;
    Ok(Json(slots))
}

#[tracing::instrument(skip_all)]
pub async fn create_slots(
    State(state): State<AppState>,
    _auth: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateSlotsRequest>,
) -> Result<Json<Vec<TimeSlotResponse>>, AppError> {
    let now = state.clock.now();
    let slots = service::create_slots(&state.db, &state.config.server, now, req).await?;
    Ok(Json(slots))
}

#[tracing::instrument(skip_all)]
pub async fn update_slot(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<UpdateSlotRequest>,
) -> Result<Json<TimeSlotResponse>, AppError> {
    let slot = service::update_slot(&state.db, id, &req).await?;
    Ok(Json(slot))
}
