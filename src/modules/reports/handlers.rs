use axum::{Json, extract::State};

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;

use super::dto::{ActivityResponse, AdminReportResponse, CoachReportResponse, MemberReportResponse};
use super::service;

/// `GET /reports/admin` — admin only.
#[tracing::instrument(skip_all)]
pub async fn admin_report(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<AdminReportResponse>, AppError> {
    let now = state.clock.now();
    let report = service::admin_report(&state.db, &state.config.server, now).await?;
    Ok(Json(report))
}

/// `GET /reports/coach` — coach only (no admin bypass, per task brief).
/// Deliberate carve-out from `staff_router()`'s admin-or-coach gate (same
/// pattern as `attendance::handlers::my_students`) — building a third
/// single-role gate for these two call sites isn't worth it.
#[tracing::instrument(skip_all)]
pub async fn coach_report(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<CoachReportResponse>, AppError> {
    auth.require_role("coach")?;
    let now = state.clock.now();
    let report = service::coach_report(&state.db, &state.config.server, now, &auth).await?;
    Ok(Json(report))
}

/// `GET /reports/me` — any authenticated user.
#[tracing::instrument(skip_all)]
pub async fn member_report(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<MemberReportResponse>, AppError> {
    let now = state.clock.now();
    let report = service::member_report(&state.db, &state.config.server, now, auth.user_id).await?;
    Ok(Json(report))
}

/// `GET /reports/admin/activity` — admin only.
#[tracing::instrument(skip_all)]
pub async fn admin_activity(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<ActivityResponse>, AppError> {
    let report = service::admin_activity(&state.db).await?;
    Ok(Json(report))
}
