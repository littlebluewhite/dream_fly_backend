use axum::{
    Json,
    extract::{Query, State},
};

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{
    AvailabilityQuery, CreateSlotsRequest, DaySchedule, ScheduleQuery, TimeSlotResponse,
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
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateSlotsRequest>,
) -> Result<Json<Vec<TimeSlotResponse>>, AppError> {
    auth.require_role("admin")?;
    let slots = service::create_slots(&state.db, &state.config.server, req).await?;
    Ok(Json(slots))
}
