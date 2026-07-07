use chrono::{Duration, NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::ServerConfig;
use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::modules::coaches::repository as coaches_repository;
use crate::modules::courses::repository as courses_repository;
use crate::utils::studio_clock;

use super::dto::{
    CourseSessionResponse, MyScheduleEntryResponse, SessionsRangeQuery, TodaySessionResponse,
};
use super::repository;

/// Upper bound on `[from, to]` span for `GET /courses/{id}/sessions` — a
/// service-layer rule (no DB constraint), per the task brief.
const MAX_RANGE_DAYS: i64 = 60;

/// Default span when `to` isn't supplied, applied relative to `from` (which
/// itself defaults to today) — so "no query params at all" yields
/// `[today, today + 28d]`.
const DEFAULT_RANGE_DAYS: i64 = 28;

fn parse_query_date(s: &str) -> Result<NaiveDate, AppError> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| AppError::BadRequest(format!("invalid date format, expected YYYY-MM-DD: {s}")))
}

/// Materialize then list a single course's sessions in `[from, to]`
/// (defaults: from=studio-local today, to=from+28d). 422 if `to < from` or
/// the span exceeds `MAX_RANGE_DAYS`. 404 if the course doesn't exist.
pub async fn list_course_sessions(
    db: &PgPool,
    server: &ServerConfig,
    course_id: Uuid,
    query: SessionsRangeQuery,
) -> Result<Vec<CourseSessionResponse>, AppError> {
    courses_repository::find_by_id(db, course_id)
        .await?
        .ok_or_else(|| AppError::NotFound("course not found".into()))?;

    let today = studio_clock::today(studio_clock::studio_tz(server), Utc::now());
    let from = match query.from {
        Some(s) => parse_query_date(&s)?,
        None => today,
    };
    let to = match query.to {
        Some(s) => parse_query_date(&s)?,
        None => from + Duration::days(DEFAULT_RANGE_DAYS),
    };

    if to < from {
        return Err(AppError::Validation("to must not be before from".into()));
    }
    if (to - from).num_days() > MAX_RANGE_DAYS {
        return Err(AppError::Validation(format!(
            "date range must not exceed {MAX_RANGE_DAYS} days"
        )));
    }

    repository::materialize_range(db, &[course_id], from, to).await?;
    let sessions = repository::find_sessions_by_course_range(db, course_id, from, to).await?;
    Ok(sessions.into_iter().map(CourseSessionResponse::from).collect())
}

/// `GET /sessions/today` — admin sees every course's sessions today; a coach
/// sees only their own courses' sessions today (empty if the caller has no
/// `coaches` row, rather than erroring). "Today" is the studio-local date
/// (see `studio_clock::today`). Materializes today's sessions for the relevant
/// scope first, mirroring `list_course_sessions`. Role gating
/// (`admin`/`coach` only) happens in the handler, not here.
pub async fn today_sessions(
    db: &PgPool,
    server: &ServerConfig,
    auth: &AuthUser,
) -> Result<Vec<TodaySessionResponse>, AppError> {
    let today = studio_clock::today(studio_clock::studio_tz(server), Utc::now());

    let course_ids = if auth.is_admin() {
        repository::find_all_course_ids(db).await?
    } else {
        match coaches_repository::find_by_user_id(db, auth.user_id).await? {
            Some(coach) => repository::find_course_ids_by_coach(db, coach.id).await?,
            None => Vec::new(),
        }
    };

    repository::materialize_range(db, &course_ids, today, today).await?;
    let rows = repository::find_today_by_course_ids(db, &course_ids, today).await?;
    Ok(rows.into_iter().map(TodaySessionResponse::from).collect())
}

/// `GET /schedule/me` — the caller's weekly pattern across their active
/// enrolments. Not materialized: a direct read of `course_schedule_slots`.
pub async fn my_weekly_schedule(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<MyScheduleEntryResponse>, AppError> {
    let rows = repository::find_my_weekly_schedule(db, user_id).await?;
    Ok(rows.into_iter().map(MyScheduleEntryResponse::from).collect())
}
