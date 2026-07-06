use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::ServerConfig;
use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::modules::coaches::repository as coaches_repository;
use crate::modules::sessions::repository as sessions_repository;

use super::dto::{
    AdminCoachReportRow, AdminCourseReportRow, AdminMembersSection, AdminReportResponse,
    AdminRevenueSection, CoachReportResponse, MemberReportResponse, RevenueMonthPoint,
};
use super::repository;

/// Trailing window for the coach dashboard's rolling attendance rate
/// (`attendance_rate_30d`), per the task brief.
const COACH_ATTENDANCE_WINDOW_DAYS: i64 = 30;

/// Forward window for a member's "upcoming sessions" count. Mirrors
/// `sessions::service`'s `DEFAULT_RANGE_DAYS` "`to = from + N` days" math
/// (an 8-calendar-day inclusive window: today plus 7 more days), rather
/// than a strict "next 7 calendar dates", for consistency with how this
/// codebase already expresses "N-day range from today" elsewhere.
const MEMBER_UPCOMING_WINDOW_DAYS: i64 = 7;

/// `numerator / denominator` as a ratio, or `None` when `denominator` is 0.
/// Guards against emitting `NaN`/`Infinity` (which `serde_json` cannot
/// represent as valid JSON) and, more importantly, expresses "undefined"
/// explicitly rather than relying on that library-specific NaN/Infinity
/// serialization behavior. Shared by `fill_rate` (enrolled/max_students)
/// and both attendance-rate calculations (present/(present+absent)) — all
/// three are "count over count, zero-safe" in the same shape.
fn safe_ratio(numerator: i64, denominator: i64) -> Option<f64> {
    if denominator == 0 {
        None
    } else {
        Some(numerator as f64 / denominator as f64)
    }
}

/// Mirrors `sessions::service::studio_tz` (copied, not shared — the
/// established per-module convention in this codebase for these small
/// timezone helpers; see e.g. `leave::service`'s own copy).
fn studio_tz(server: &ServerConfig) -> Tz {
    server.studio_timezone.parse::<Tz>().unwrap_or(chrono_tz::UTC)
}

/// Mirrors `sessions::service::studio_date_at`.
fn studio_date_at(tz: Tz, now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&tz).date_naive()
}

/// `GET /reports/admin`. Role gating (`admin` only) happens in the
/// handler, not here (mirrors `sessions::today_sessions`'s division of
/// responsibility). Pure aggregation — no writes.
pub async fn admin_report(
    db: &PgPool,
    server: &ServerConfig,
) -> Result<AdminReportResponse, AppError> {
    let now = Utc::now();
    let tz_name = server.studio_timezone.as_str();

    let trend_rows = repository::revenue_trend(db, now, tz_name).await?;
    let (total, new_this_month, active) = repository::member_stats(db, now, tz_name).await?;
    let course_rows = repository::course_reports(db).await?;
    let coach_rows = repository::coach_reports(db).await?;

    let trend: Vec<RevenueMonthPoint> = trend_rows
        .into_iter()
        .map(|(month, revenue_cents)| RevenueMonthPoint { month, revenue_cents })
        .collect();
    // `revenue_trend` always returns exactly 12 rows (oldest..newest,
    // ending at the current studio-local month), so the last entry is
    // "this month" and the second-to-last is "last month". `checked_sub`
    // guards against a hypothetically shorter vec rather than assuming the
    // invariant always holds.
    let this_month_cents = trend.last().map(|p| p.revenue_cents).unwrap_or(0);
    let last_month_cents = trend
        .len()
        .checked_sub(2)
        .and_then(|i| trend.get(i))
        .map(|p| p.revenue_cents)
        .unwrap_or(0);

    let courses: Vec<AdminCourseReportRow> = course_rows
        .into_iter()
        .map(|r| AdminCourseReportRow {
            fill_rate: safe_ratio(r.enrolled, r.max_students as i64),
            course_id: r.course_id,
            name: r.name,
            enrolled: r.enrolled,
            max_students: r.max_students,
            waitlist_count: r.waitlist_count,
        })
        .collect();

    let coaches: Vec<AdminCoachReportRow> = coach_rows
        .into_iter()
        .map(|r| AdminCoachReportRow {
            coach_id: r.coach_id,
            name: r.name,
            course_count: r.course_count,
            student_count: r.student_count,
        })
        .collect();

    Ok(AdminReportResponse {
        revenue: AdminRevenueSection { this_month_cents, last_month_cents, trend },
        members: AdminMembersSection { total, new_this_month, active },
        courses,
        coaches,
    })
}

/// `GET /reports/coach`. 404 if the caller holds the `coach` role but has
/// no `coaches` profile row — this mirrors `coaches::service`'s own
/// `"coach not found"` 404 wording for a missing coach row, rather than
/// `sessions::today_sessions`'/`attendance::my_students`'s "degrade to an
/// empty list" convention: this endpoint returns one dashboard *object*,
/// not a list, so there's no natural "empty" value that wouldn't be
/// misleading (a zeroed/null dashboard looks identical to "you have no
/// students yet" instead of "you aren't a coach"). Role gating (`coach`
/// only, no admin bypass — see task brief) happens in the handler.
pub async fn coach_report(
    db: &PgPool,
    server: &ServerConfig,
    auth: &AuthUser,
) -> Result<CoachReportResponse, AppError> {
    let coach = coaches_repository::find_by_user_id(db, auth.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("coach not found".into()))?;

    let today = studio_date_at(studio_tz(server), Utc::now());
    let course_ids = sessions_repository::find_course_ids_by_coach(db, coach.id).await?;
    sessions_repository::materialize_range(db, &course_ids, today, today).await?;

    let (today_sessions, pending_attendance) =
        repository::coach_today_and_pending(db, coach.id, today).await?;
    let unread_messages = repository::unread_message_count(db, auth.user_id).await?;
    let student_count = repository::coach_student_count(db, coach.id).await?;

    let window_from = today - Duration::days(COACH_ATTENDANCE_WINDOW_DAYS);
    let (present, absent) =
        repository::coach_attendance_in_range(db, coach.id, window_from, today).await?;

    Ok(CoachReportResponse {
        today_sessions,
        pending_attendance,
        unread_messages,
        student_count,
        attendance_rate_30d: safe_ratio(present, present + absent),
    })
}

/// `GET /reports/me`. Any authenticated user (member or coach alike) — no
/// role gate beyond being logged in.
pub async fn member_report(
    db: &PgPool,
    server: &ServerConfig,
    user_id: Uuid,
) -> Result<MemberReportResponse, AppError> {
    let today = studio_date_at(studio_tz(server), Utc::now());

    let (present, absent) = repository::member_attendance(db, user_id).await?;
    let points_balance = repository::points_balance(db, user_id).await?;
    let course_ids = repository::my_active_enrolment_course_ids(db, user_id).await?;
    let active_enrolments = course_ids.len() as i64;

    let window_to = today + Duration::days(MEMBER_UPCOMING_WINDOW_DAYS);
    sessions_repository::materialize_range(db, &course_ids, today, window_to).await?;
    let upcoming_sessions_7d =
        repository::upcoming_session_count(db, &course_ids, today, window_to).await?;

    Ok(MemberReportResponse {
        attended_total: present,
        attendance_rate: safe_ratio(present, present + absent),
        points_balance,
        active_enrolments,
        upcoming_sessions_7d,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // `courses_max_students_pos CHECK (max_students > 0)` (see the init
    // migration) means a real `courses` row can never have `max_students =
    // 0` — this scenario cannot be reproduced through a DB-backed
    // integration test. `safe_ratio` is exercised directly here instead,
    // covering the task brief's "fill_rate 除零" requirement at the level
    // where it's actually reachable: defensive code, not reachable data.
    #[test]
    fn safe_ratio_divide_by_zero_is_none() {
        assert_eq!(safe_ratio(5, 0), None);
        assert_eq!(safe_ratio(0, 0), None);
    }

    #[test]
    fn safe_ratio_normal_case() {
        assert_eq!(safe_ratio(6, 8), Some(0.75));
        assert_eq!(safe_ratio(0, 5), Some(0.0));
    }
}
