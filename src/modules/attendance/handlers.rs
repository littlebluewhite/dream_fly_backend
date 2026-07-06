use axum::{
    Json,
    extract::{Path, State},
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{BulkUpsertAttendanceRequest, MyStudentResponse, RosterEntryResponse};
use super::service;

/// `GET /sessions/{id}/roster` — that course's coach, or admin.
#[tracing::instrument(skip_all)]
pub async fn get_roster(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(session_id): Path<Uuid>,
) -> Result<Json<Vec<RosterEntryResponse>>, AppError> {
    auth.require_any_role(&["admin", "coach"])?;
    let roster = service::get_roster(&state.db, &auth, session_id).await?;
    Ok(Json(roster))
}

/// `PUT /sessions/{id}/attendance` — that course's coach, or admin.
#[tracing::instrument(skip_all)]
pub async fn bulk_upsert_attendance(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(session_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<BulkUpsertAttendanceRequest>,
) -> Result<Json<Vec<RosterEntryResponse>>, AppError> {
    auth.require_any_role(&["admin", "coach"])?;
    let roster =
        service::bulk_upsert_attendance(&state.db, &auth, session_id, req.records).await?;
    Ok(Json(roster))
}

/// `GET /coaches/me/students` — coach only.
#[tracing::instrument(skip_all)]
pub async fn my_students(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<MyStudentResponse>>, AppError> {
    auth.require_role("coach")?;
    let students = service::my_students(&state.db, &auth).await?;
    Ok(Json(students))
}
