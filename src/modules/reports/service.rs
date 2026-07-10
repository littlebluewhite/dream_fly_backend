use chrono::{Datelike, Duration, NaiveDate, Utc};
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

use super::dto::{
    ActivityItem, ActivityResponse, AdminCoachReportRow, AdminCourseReportRow, AdminMembersSection,
    AdminReportResponse, AdminRevenueSection, BucketCountEntry, CategorySplitEntry,
    CoachReportResponse, FunnelSection, IncomeSourceEntry, IncomeSourceMonthEntry, KpisSection,
    MemberReportResponse, MonthPair, PaymentSplitEntry, RateMonthPair, RetentionMonthRow,
    RevenueMonthPoint, VenueUsageEntry, WeekdayLoadEntry,
};
use super::model::ActivityRow;
use super::repository;

/// Trailing window for the coach dashboard's rolling attendance rate
/// (`attendance_rate_30d`), per the task brief.
const COACH_ATTENDANCE_WINDOW_DAYS: i64 = 30;

/// Months covered by `income_sources_12m` — the same trailing window as
/// `revenue.trend`'s hardcoded 12 (see `repository::revenue_trend`).
const INCOME_SOURCE_MONTHS: i32 = 12;

/// The one source of `repository::income_by_source` that is *not* an order
/// line (bookings, not `order_items`) — `category_split` is defined over
/// order-line 毛額 only, so this source is filtered out of it (and out of
/// its ratio denominator) while still appearing in `revenue_breakdown`.
const VENUE_RENTAL_SOURCE: &str = "venue_rental";

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
/// serialization behavior. Shared by `fill_rate` (enrolled/max_students),
/// every attendance-rate calculation (present/(present+absent)), and
/// `category_split`'s gross-over-total ratios — all are "count over count,
/// zero-safe" in the same shape.
fn safe_ratio(numerator: i64, denominator: i64) -> Option<f64> {
    if denominator == 0 {
        None
    } else {
        Some(numerator as f64 / denominator as f64)
    }
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
    let coach_rows = repository::coach_reports(db, now, tz_name).await?;
    let kpi = repository::kpis(db, now, tz_name).await?;
    let income_rows = repository::income_by_source(db, now, tz_name, INCOME_SOURCE_MONTHS).await?;
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
    sessions_repository::materialize_range(db, &all_course_ids, month_start, month_end).await?;
    let venue_rows = repository::venue_usage(db, now, tz_name).await?;

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
            revenue_cents_12m: r.revenue_cents_12m,
            attendance_rate: safe_ratio(r.att_present, r.att_present + r.att_absent),
        })
        .collect();

    let kpis = KpisSection {
        new_members: MonthPair {
            this_month: kpi.new_members_this,
            last_month: kpi.new_members_last,
        },
        new_enrolments: MonthPair {
            this_month: kpi.new_enrolments_this,
            last_month: kpi.new_enrolments_last,
        },
        paid_orders_count: MonthPair {
            this_month: kpi.paid_orders_this,
            last_month: kpi.paid_orders_last,
        },
        attendance_rate: RateMonthPair {
            this_month: safe_ratio(kpi.present_this, kpi.present_this + kpi.absent_this),
            last_month: safe_ratio(kpi.present_last, kpi.present_last + kpi.absent_last),
        },
    };

    // `income_by_source` zero-fills every (month, source) cell, so the
    // current studio month's rows are always exactly the 6 sources —
    // `revenue_breakdown` is that slice, `income_sources_12m` the whole
    // series, and `category_split` the order-line subset of the slice with
    // ratios over the order-line total (venue rental is not an order line
    // — see `dto::CategorySplitEntry`).
    let current_month_key =
        now.with_timezone(&studio_clock::studio_tz(server)).format("%Y-%m").to_string();
    let revenue_breakdown: Vec<IncomeSourceEntry> = income_rows
        .iter()
        .filter(|r| r.month == current_month_key)
        .map(|r| IncomeSourceEntry {
            source: r.source.clone(),
            gross_cents: r.gross_cents,
            orders_count: r.orders_count,
            units: r.units,
        })
        .collect();

    let order_line_total: i64 = revenue_breakdown
        .iter()
        .filter(|r| r.source != VENUE_RENTAL_SOURCE)
        .map(|r| r.gross_cents)
        .sum();
    let category_split: Vec<CategorySplitEntry> = revenue_breakdown
        .iter()
        .filter(|r| r.source != VENUE_RENTAL_SOURCE)
        .map(|r| CategorySplitEntry {
            source: r.source.clone(),
            gross_cents: r.gross_cents,
            ratio: safe_ratio(r.gross_cents, order_line_total),
        })
        .collect();

    let income_sources_12m: Vec<IncomeSourceMonthEntry> = income_rows
        .into_iter()
        .map(|r| IncomeSourceMonthEntry {
            month: r.month,
            source: r.source,
            gross_cents: r.gross_cents,
            orders_count: r.orders_count,
            units: r.units,
        })
        .collect();

    let payment_split: Vec<PaymentSplitEntry> = payment_rows
        .into_iter()
        .map(|(method, count)| PaymentSplitEntry { method, count })
        .collect();

    // The three fixed-bucket distributions share `(bucket, count)` — each
    // repository query already zero-fills its own fixed bucket set, so the
    // service just renames the row into its DTO.
    let attendance_distribution: Vec<BucketCountEntry> = attendance_dist_rows
        .into_iter()
        .map(|r| BucketCountEntry { bucket: r.bucket, count: r.count })
        .collect();
    let age_distribution: Vec<BucketCountEntry> = age_dist_rows
        .into_iter()
        .map(|r| BucketCountEntry { bucket: r.bucket, count: r.count })
        .collect();
    let tier_distribution: Vec<BucketCountEntry> = tier_dist_rows
        .into_iter()
        .map(|r| BucketCountEntry { bucket: r.bucket, count: r.count })
        .collect();

    // `rate` = |上月活躍 ∩ 本月活躍| / |上月活躍| — `safe_ratio` renders the
    // empty-previous-month case as null (undefined), not 0.
    let retention: Vec<RetentionMonthRow> = retention_rows
        .into_iter()
        .map(|r| RetentionMonthRow {
            month: r.month,
            new_count: r.new_count,
            returning_count: r.returning_count,
            rate: safe_ratio(r.retained_count, r.prev_active_count),
        })
        .collect();

    let funnel = FunnelSection {
        trial_inquiries: funnel_row.trial_inquiries,
        new_enrolments: funnel_row.new_enrolments,
    };

    let weekday_load: Vec<WeekdayLoadEntry> = weekday_rows
        .into_iter()
        .map(|r| WeekdayLoadEntry { weekday: r.weekday, present_count: r.present_count })
        .collect();

    let venue_usage: Vec<VenueUsageEntry> = venue_rows
        .into_iter()
        .map(|r| VenueUsageEntry { venue: r.venue, minutes: r.minutes })
        .collect();

    Ok(AdminReportResponse {
        revenue: AdminRevenueSection { this_month_cents, last_month_cents, trend },
        kpis,
        revenue_breakdown,
        income_sources_12m,
        category_split,
        payment_split,
        attendance_distribution,
        age_distribution,
        tier_distribution,
        retention,
        funnel,
        weekday_load,
        venue_usage,
        members: AdminMembersSection { total, new_this_month, active },
        courses,
        coaches,
    })
}

/// `(first_day, last_day)` of `today`'s calendar month. Used to bound the
/// idempotent session materialization the `venue_usage` aggregate needs (the
/// SQL itself re-derives the same month window from `now`/`tz` — see
/// `repository::venue_usage`). `unwrap`s are total: day 1 always exists, and
/// stepping to the first of next month then back one day always lands on a
/// real last-of-month.
fn studio_month_bounds(today: NaiveDate) -> (NaiveDate, NaiveDate) {
    let month_start = today.with_day(1).expect("day 1 is valid for every month");
    let next_month_first = if month_start.month() == 12 {
        NaiveDate::from_ymd_opt(month_start.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(month_start.year(), month_start.month() + 1, 1)
    }
    .expect("first day of next month is always valid");
    let month_end = next_month_first.pred_opt().expect("every month has a last day");
    (month_start, month_end)
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
    let coach = coaches_service::resolve(db, auth)
        .await?
        .ok_or_else(|| AppError::NotFound("coach not found".into()))?;

    let today = studio_clock::today(studio_clock::studio_tz(server), Utc::now());
    let course_ids = sessions_repository::find_course_ids_by_coach(db, coach.id).await?;
    sessions_repository::materialize_range(db, &course_ids, today, today).await?;

    let (today_sessions, pending_attendance) =
        repository::coach_today_and_pending(db, coach.id, today).await?;
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
    user_id: Uuid,
) -> Result<MemberReportResponse, AppError> {
    let today = studio_clock::today(studio_clock::studio_tz(server), Utc::now());

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
