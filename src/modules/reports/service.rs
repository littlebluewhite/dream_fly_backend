use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::ServerConfig;
use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::modules::attendance::repository as attendance_repository;
use crate::modules::coaches::service as coaches_service;
use crate::modules::messages::repository as messages_repository;
use crate::modules::sessions::repository as sessions_repository;
use crate::utils::studio_clock;

use super::assembly::{self, safe_ratio};
use super::dto::{
    ActivityItem, ActivityResponse, AdminReportResponse, CoachReportResponse, MemberReportResponse,
};
use super::model::ActivityRow;
use super::repository;

/// Trailing window for the coach dashboard's rolling attendance rate
/// (`attendance_rate_30d`), per the task brief.
const COACH_ATTENDANCE_WINDOW_DAYS: i64 = 30;

/// Trailing window (studio-local months) shared by `revenue_trend`,
/// `coach_reports`, and `income_by_source` — single-sourced here instead of
/// each query repeating its own "12"/"11 months" literal. `months` only
/// exists as a SQL bind parameter so the three queries share one window
/// idiom; it is not a tunable/configurable setting — every call site below
/// passes this constant verbatim.
const TRAILING_WINDOW_MONTHS: i32 = 12;

/// Forward window for a member's "upcoming sessions" count. Mirrors
/// `sessions::service`'s `DEFAULT_RANGE_DAYS` "`to = from + N` days" math
/// (an 8-calendar-day inclusive window: today plus 7 more days), rather
/// than a strict "next 7 calendar dates", for consistency with how this
/// codebase already expresses "N-day range from today" elsewhere.
const MEMBER_UPCOMING_WINDOW_DAYS: i64 = 7;

/// `GET /reports/admin`. Role gating (`admin` only) happens in the
/// handler, not here (mirrors `sessions::today_sessions`'s division of
/// responsibility). Pure aggregation — no writes.
pub async fn admin_report(
    db: &PgPool,
    server: &ServerConfig,
    now: DateTime<Utc>,
) -> Result<AdminReportResponse, AppError> {
    let tz_name = server.studio_timezone.as_str();

    let trend_rows = repository::revenue_trend(db, now, tz_name, TRAILING_WINDOW_MONTHS).await?;
    let member_stats = repository::member_stats(db, now, tz_name).await?;
    let course_rows = repository::course_reports(db).await?;
    let coach_rows = repository::coach_reports(db, now, tz_name, TRAILING_WINDOW_MONTHS).await?;
    let kpi = repository::kpis(db, now, tz_name).await?;
    let income_rows =
        repository::income_by_source(db, now, tz_name, TRAILING_WINDOW_MONTHS).await?;
    let payment_rows = repository::payment_split(db, now, tz_name).await?;
    let attendance_dist_rows = repository::attendance_distribution(db).await?;
    let age_dist_rows = repository::age_distribution(db, now, tz_name).await?;
    let tier_dist_rows = repository::tier_distribution(db).await?;
    let retention_rows = repository::retention(db, now, tz_name).await?;
    let funnel_row = repository::funnel(db, now, tz_name).await?;
    let weekday_rows = repository::weekday_load(db, now, tz_name).await?;

    // `venue_usage` is over *this studio month's* sessions, which may not all
    // be materialized yet (future dates in the current month) — so idempotently
    // materialize the whole month for every course first, mirroring how the
    // coach/member reports materialize their own windows before counting.
    let today = studio_clock::today(studio_clock::studio_tz(server), now);
    let (month_start, month_end) = studio_month_bounds(today);
    let all_course_ids = sessions_repository::find_all_course_ids(db).await?;
    let mat =
        sessions_repository::materialize_range(db, &all_course_ids, month_start, month_end).await?;
    let venue_rows = repository::venue_usage(db, &mat).await?;

    let current_month_key = studio_clock::month_key(studio_clock::studio_tz(server), now);
    let inputs = assembly::AdminReportInputs {
        trend_rows,
        member_stats,
        course_rows,
        coach_rows,
        kpi,
        income_rows,
        payment_rows,
        attendance_dist_rows,
        age_dist_rows,
        tier_dist_rows,
        retention_rows,
        funnel_row,
        weekday_rows,
        venue_rows,
    };
    Ok(assembly::assemble_admin_report(inputs, &current_month_key))
}

/// `(first_day, last_day)` of `today`'s calendar month. Used to bound the
/// idempotent session materialization the `venue_usage` aggregate needs —
/// `venue_usage` receives it only via the `MaterializedRange` witness's
/// date bounds, not by re-deriving it in SQL (see
/// `repository::venue_usage`). Thin shell over `studio_clock::month_bounds`
/// (the year-rollover + `pred_opt` dance's single owner now); `today`'s own
/// year/month always yields valid bounds, so the `expect` is total.
fn studio_month_bounds(today: NaiveDate) -> (NaiveDate, NaiveDate) {
    studio_clock::month_bounds(today.year(), today.month())
        .expect("today's own year/month always yields valid month bounds")
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
    now: DateTime<Utc>,
    auth: &AuthUser,
) -> Result<CoachReportResponse, AppError> {
    let coach = coaches_service::resolve(db, auth)
        .await?
        .ok_or_else(|| AppError::NotFound("coach not found".into()))?;

    let today = studio_clock::today(studio_clock::studio_tz(server), now);
    let course_ids = sessions_repository::find_course_ids_by_coach(db, coach.id).await?;
    let day = sessions_repository::materialize_day(db, &course_ids, today).await?;

    let (today_sessions, pending_attendance) =
        repository::coach_today_and_pending(db, coach.id, &day).await?;
    let unread_messages = messages_repository::count_unread_for_user(db, auth.user_id).await?;
    let student_count = attendance_repository::count_my_students(db, coach.id).await?;

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
    now: DateTime<Utc>,
    user_id: Uuid,
) -> Result<MemberReportResponse, AppError> {
    let today = studio_clock::today(studio_clock::studio_tz(server), now);

    let (present, absent) = repository::member_attendance(db, user_id).await?;
    let points_balance = repository::points_balance(db, user_id).await?;
    let course_ids = repository::my_active_enrolment_course_ids(db, user_id).await?;
    let active_enrolments = course_ids.len() as i64;

    let window_to = today + Duration::days(MEMBER_UPCOMING_WINDOW_DAYS);
    let mat = sessions_repository::materialize_range(db, &course_ids, today, window_to).await?;
    let upcoming_sessions_7d = repository::upcoming_session_count(db, &mat).await?;

    Ok(MemberReportResponse {
        attended_total: present,
        attendance_rate: safe_ratio(present, present + absent),
        points_balance,
        active_enrolments,
        upcoming_sessions_7d,
    })
}

/// `GET /reports/admin/activity`. Role gating (`admin` only) happens in the
/// handler. Merges the 20 most recent rows from four operational-event
/// sources (see `repository::recent_activity`) and formats each into a
/// backend-composed label string via `activity_label`.
pub async fn admin_activity(db: &PgPool) -> Result<ActivityResponse, AppError> {
    let rows = repository::recent_activity(db).await?;
    let items = rows.into_iter().map(activity_label).collect();
    Ok(ActivityResponse { items })
}

/// Formats one merged UNION row into its response shape — the one place
/// that knows each `kind`'s (Traditional Chinese, task-brief-verbatim)
/// label template. `amount_cents`/`inquiry_type` are `None` for every kind
/// except `order`/`inquiry` respectively (see `model::ActivityRow`'s doc
/// comment), so `unwrap_or` defaults there are unreachable in practice, not
/// a masked error case. The `order` amount is rendered as whole NT dollars
/// (`cents / 100`, no decimals) embedded directly in the label — this
/// module's response shape has no separate amount field, so an
/// amount-bearing label has nowhere else to put it (see task report for the
/// brief's internally-inconsistent wording on this point).
fn activity_label(row: ActivityRow) -> ActivityItem {
    let label = match row.kind.as_str() {
        "user" => format!("新會員註冊:{}", row.detail),
        "order" => {
            let dollars = row.amount_cents.unwrap_or(0) / 100;
            format!("訂單 {} 已付款:NT${dollars}", row.detail)
        }
        "enrolment" => format!("新報名:{}", row.detail),
        "inquiry" => {
            let inquiry_type = row.inquiry_type.as_deref().unwrap_or("general");
            format!("新洽詢({inquiry_type}):{}", row.detail)
        }
        // Unreachable given `repository::recent_activity`'s fixed 4-branch
        // UNION always tags one of the four kinds above.
        other => format!("{}:{}", other, row.detail),
    };
    ActivityItem { kind: row.kind, label, occurred_at: row.occurred_at }
}
