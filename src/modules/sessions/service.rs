use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::ServerConfig;
use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::modules::coaches::repository as coaches_repository;
use crate::modules::courses::repository as courses_repository;

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

/// Resolve the studio timezone. Mirrors `schedule::service::studio_tz` —
/// startup validation (`AppConfig::load`) already rejects invalid timezone
/// names, so the UTC fallback only fires if a future refactor bypasses that
/// check.
fn studio_tz(server: &ServerConfig) -> Tz {
    server.studio_timezone.parse::<Tz>().unwrap_or(chrono_tz::UTC)
}

/// The studio-local calendar date of a UTC instant — this module's "today"
/// (contract §3.18 裁決 2). Kept separate from `Utc::now()` so the day
/// boundary is unit-testable with fixed inputs: at 23:00 UTC the studio
/// (Asia/Taipei, UTC+8) is already 07:00 the *next* day, and a coach
/// checking their morning `GET /sessions/today` must get that next day,
/// not UTC's date.
fn studio_date_at(tz: Tz, now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&tz).date_naive()
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

    let today = studio_date_at(studio_tz(server), Utc::now());
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
/// (see `studio_date_at`). Materializes today's sessions for the relevant
/// scope first, mirroring `list_course_sessions`. Role gating
/// (`admin`/`coach` only) happens in the handler, not here.
pub async fn today_sessions(
    db: &PgPool,
    server: &ServerConfig,
    auth: &AuthUser,
) -> Result<Vec<TodaySessionResponse>, AppError> {
    let today = studio_date_at(studio_tz(server), Utc::now());

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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn taipei() -> Tz {
        "Asia/Taipei".parse::<Tz>().expect("valid IANA name")
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn studio_date_before_taipei_midnight_matches_utc_date() {
        // 15:59:59Z = 23:59:59 Taipei — still the same calendar day in
        // both zones.
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 15, 59, 59).unwrap();
        assert_eq!(studio_date_at(taipei(), now), d(2026, 7, 5));
    }

    #[test]
    fn studio_date_at_taipei_midnight_rolls_to_next_day() {
        // 16:00:00Z = 00:00:00 Taipei of the NEXT day — Taipei's date must
        // win over UTC's (which is still July 5).
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 16, 0, 0).unwrap();
        assert_eq!(studio_date_at(taipei(), now), d(2026, 7, 6));
    }

    #[test]
    fn studio_date_taipei_early_morning_is_next_utc_day() {
        // 22:00:00Z = 06:00 Taipei next day — the "coach checks morning
        // sessions at 6-7am Taipei" scenario this helper exists for.
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 22, 0, 0).unwrap();
        assert_eq!(studio_date_at(taipei(), now), d(2026, 7, 6));
    }

    #[test]
    fn studio_date_under_utc_config_is_plain_utc_date() {
        // The integration-test harness pins studio_timezone to UTC — under
        // that config the helper must degrade to the plain UTC date.
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 22, 0, 0).unwrap();
        assert_eq!(studio_date_at(chrono_tz::UTC, now), d(2026, 7, 5));
    }
}
