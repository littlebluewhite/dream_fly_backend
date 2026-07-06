use axum::{Json, extract::State};

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;

use super::dto::{AdminReportResponse, CoachReportResponse, MemberReportResponse};
use super::service;

/// `GET /reports/admin` — admin only.
#[tracing::instrument(skip_all)]
pub async fn admin_report(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<AdminReportResponse>, AppError> {
    auth.require_role("admin")?;
    let report = service::admin_report(&state.db, &state.config.server).await?;
    Ok(Json(report))
}

/// `GET /reports/coach` — coach only (no admin bypass, per task brief).
#[tracing::instrument(skip_all)]
pub async fn coach_report(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<CoachReportResponse>, AppError> {
    auth.require_role("coach")?;
    let report = service::coach_report(&state.db, &state.config.server, &auth).await?;
    Ok(Json(report))
}

/// `GET /reports/me` — any authenticated user.
#[tracing::instrument(skip_all)]
pub async fn member_report(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<MemberReportResponse>, AppError> {
    let report = service::member_report(&state.db, &state.config.server, auth.user_id).await?;
    Ok(Json(report))
}
