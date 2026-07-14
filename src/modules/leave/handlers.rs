use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{
    CreateLeaveRequestRequest, DecideLeaveRequestRequest, LeaveRequestListResponse,
    LeaveRequestQuery, LeaveRequestResponse, MakeupRequest,
};
use super::service;

/// `POST /leave-requests` — member (any authenticated user; service resolves
/// their own active enrolment).
#[tracing::instrument(skip_all)]
pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateLeaveRequestRequest>,
) -> Result<Json<LeaveRequestResponse>, AppError> {
    let now = state.clock.now();
    let created =
        service::create_leave_request(&state.db, &state.config.server, now, &auth, req).await?;
    Ok(Json(created))
}

/// `GET /leave-requests/me` — the caller's own leave requests.
#[tracing::instrument(skip_all)]
pub async fn me(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<LeaveRequestResponse>>, AppError> {
    let requests = service::list_my_leave_requests(&state.db, auth.user_id).await?;
    Ok(Json(requests))
}

/// `DELETE /leave-requests/{id}` — owner only, pending only.
#[tracing::instrument(skip_all)]
pub async fn cancel(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    service::cancel_leave_request(&state.db, &auth, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /leave-requests?status=&course_id=` — coach (own courses) or admin.
#[tracing::instrument(skip_all)]
pub async fn list(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(query): Query<LeaveRequestQuery>,
    Query(pagination): Query<PaginationParams>,
) -> Result<Json<LeaveRequestListResponse>, AppError> {
    auth.require_any_role(&["admin", "coach"])?;
    let result = service::list_leave_requests(&state.db, &auth, query, &pagination).await?;
    Ok(Json(result))
}

/// `PATCH /leave-requests/{id}` — that course's coach or admin.
#[tracing::instrument(skip_all)]
pub async fn decide(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<DecideLeaveRequestRequest>,
) -> Result<Json<LeaveRequestResponse>, AppError> {
    auth.require_any_role(&["admin", "coach"])?;
    let updated = service::decide_leave_request(&state.db, &auth, id, &req.status).await?;
    Ok(Json(updated))
}

/// `POST /leave-requests/{id}/makeup` — owner only.
#[tracing::instrument(skip_all)]
pub async fn makeup(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<MakeupRequest>,
) -> Result<Json<LeaveRequestResponse>, AppError> {
    let now = state.clock.now();
    let updated =
        service::book_makeup(&state.db, &state.config.server, now, &auth, id, req).await?;
    Ok(Json(updated))
}
