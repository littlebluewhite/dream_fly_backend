use axum::{
    Json,
    extract::{Path, Query, State},
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;

use super::dto::{
    CourseSessionResponse, MyScheduleEntryResponse, SessionsRangeQuery, TodaySessionResponse,
};
use super::service;

/// `GET /courses/{id}/sessions?from=&to=` — any authenticated user.
#[tracing::instrument(skip_all)]
pub async fn list_course_sessions(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(course_id): Path<Uuid>,
    Query(query): Query<SessionsRangeQuery>,
) -> Result<Json<Vec<CourseSessionResponse>>, AppError> {
    let now = state.clock.now();
    let sessions =
        service::list_course_sessions(&state.db, &state.config.server, now, course_id, query)
            .await?;
    Ok(Json(sessions))
}

/// `GET /sessions/today` — coach or admin only.
#[tracing::instrument(skip_all)]
pub async fn today(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<TodaySessionResponse>>, AppError> {
    let now = state.clock.now();
    let sessions = service::today_sessions(&state.db, &state.config.server, now, &auth).await?;
    Ok(Json(sessions))
}

/// `GET /schedule/me` — any authenticated user; scoped to their own active
/// enrolments.
#[tracing::instrument(skip_all)]
pub async fn my_schedule(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<MyScheduleEntryResponse>>, AppError> {
    let schedule = service::my_weekly_schedule(&state.db, auth.user_id).await?;
    Ok(Json(schedule))
}
