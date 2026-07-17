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

/// `GET /sessions/{id}/roster` — that course's coach, or admin. Enforced by
/// the `staff_api` route_layer (see `startup.rs`).
#[tracing::instrument(skip_all)]
pub async fn get_roster(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(session_id): Path<Uuid>,
) -> Result<Json<Vec<RosterEntryResponse>>, AppError> {
    let roster = service::get_roster(&state.db, &auth, session_id).await?;
    Ok(Json(roster))
}

/// `PUT /sessions/{id}/attendance` — that course's coach, or admin.
/// Enforced by the `staff_api` route_layer (see `startup.rs`).
#[tracing::instrument(skip_all)]
pub async fn bulk_upsert_attendance(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(session_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<BulkUpsertAttendanceRequest>,
) -> Result<Json<Vec<RosterEntryResponse>>, AppError> {
    let now = state.clock.now();
    let roster = service::bulk_upsert_attendance(
        &state.db,
        &state.config.server,
        now,
        &auth,
        session_id,
        req.records,
    )
    .await?;
    Ok(Json(roster))
}

/// `GET /coaches/me/students` — coach only; admin is deliberately excluded
/// (an admin has no "own" students to look up). Stays a handler-level
/// `require_role("coach")` check instead of joining `staff_router()`'s
/// admin-or-coach gate — building a third single-role gate for this one call
/// site (plus `reports::handlers::coach_report`) isn't worth it.
#[tracing::instrument(skip_all)]
pub async fn my_students(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<MyStudentResponse>>, AppError> {
    auth.require_role("coach")?;
    let students = service::my_students(&state.db, &auth).await?;
    Ok(Json(students))
}
