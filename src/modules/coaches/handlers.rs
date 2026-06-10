use axum::{
    Json,
    extract::{Path, Query, State},
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{
    ClockNoteRequest, ClockRecordResponse, CoachDetailResponse, CoachResponse,
    CoachScheduleResponse, UpdateScheduleRequest,
};
use super::service;

#[tracing::instrument(skip_all)]
pub async fn list(
    State(state): State<AppState>,
) -> Result<Json<Vec<CoachResponse>>, AppError> {
    let coaches = service::list_active(&state.db).await?;
    Ok(Json(coaches))
}

#[tracing::instrument(skip_all)]
pub async fn get_by_id(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<CoachDetailResponse>, AppError> {
    let detail = service::get_detail(&state.db, id).await?;
    Ok(Json(detail))
}

#[tracing::instrument(skip_all)]
pub async fn clock_in(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<ClockNoteRequest>,
) -> Result<Json<ClockRecordResponse>, AppError> {
    let record = service::clock_in(&state.db, &auth, id, req.note.as_deref()).await?;
    Ok(Json(record))
}

#[tracing::instrument(skip_all)]
pub async fn clock_out(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ClockRecordResponse>, AppError> {
    let record = service::clock_out(&state.db, &auth, id).await?;
    Ok(Json(record))
}

#[tracing::instrument(skip_all)]
pub async fn get_clock_records(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<Vec<ClockRecordResponse>>, AppError> {
    let records =
        service::get_clock_records(&state.db, &auth, id, params.limit(), params.offset()).await?;
    Ok(Json(records))
}

#[tracing::instrument(skip_all)]
pub async fn get_schedule(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<CoachScheduleResponse>>, AppError> {
    let schedules = service::get_schedules(&state.db, id).await?;
    Ok(Json(schedules))
}

#[tracing::instrument(skip_all)]
pub async fn update_schedule(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<UpdateScheduleRequest>,
) -> Result<Json<Vec<CoachScheduleResponse>>, AppError> {
    let schedules = service::update_schedules(&state.db, &auth, id, &req.schedules).await?;
    Ok(Json(schedules))
}
