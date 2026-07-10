//! No new tables — this module is pure cross-module aggregation over
//! `orders`/`order_items`/`products`/`users`/`enrolments`/`courses`/
//! `coaches`/`waitlist_entries`/`course_sessions`/`attendance_records`/
//! `bookings`/`time_slots`/`conversations`/`messages`/`contact_inquiries`
//! (all owned by other modules' migrations). Only the multi-column rows
//! (admin sub-lists, the KPI/income aggregates, and the activity feed's
//! merged-UNION row) get a named `FromRow` struct here; every other
//! aggregate query decodes straight into a scalar or tuple in
//! `repository.rs` (mirrors `sessions::repository::materialize_range`'s
//! tuple-decoded candidates query — no dedicated struct needed for a
//! handful of primitive columns).

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// One row of `GET /reports/admin`'s `courses` list, before `fill_rate` is
/// derived (see `service::safe_ratio`).
#[derive(Debug, sqlx::FromRow)]
pub struct AdminCourseRow {
    pub course_id: Uuid,
    pub name: String,
    pub enrolled: i64,
    pub max_students: i32,
    pub waitlist_count: i64,
}

/// One row of `GET /reports/admin`'s `coaches` list. `revenue_cents_12m`
/// carries the coach-revenue attribution 口徑 (Round 4 Phase 4): course 類
/// order line 毛額歸 `courses.coach_id`;票券/裝備/場租不歸因 — see
/// `repository::coach_reports`. `att_present`/`att_absent` are the raw
/// present/absent counts across the coach's courses (all-time, `leave`
/// excluded) the service turns into `attendance_rate` = present/(present+
/// absent) via `service::safe_ratio` (無資料 → null).
#[derive(Debug, sqlx::FromRow)]
pub struct AdminCoachRow {
    pub coach_id: Uuid,
    pub name: String,
    pub course_count: i64,
    pub student_count: i64,
    pub revenue_cents_12m: i64,
    pub att_present: i64,
    pub att_absent: i64,
}

/// One fixed-bucket cell of the three human-flow distributions
/// (`attendance_distribution` / `age_distribution` / `tier_distribution`) —
/// the shared `(bucket, count)` shape every one of them zero-fills to its own
/// fixed set of buckets (see `repository::attendance_distribution` /
/// `age_distribution` / `tier_distribution`). `bucket` is a backend-neutral
/// key (Chinese labels are the frontend's job); `count` is the member headcount.
#[derive(Debug, sqlx::FromRow)]
pub struct BucketCountRow {
    pub bucket: String,
    pub count: i64,
}

/// One of the trailing-6-month retention cohort rows (see
/// `repository::retention`). `new_count`/`returning_count` split the month's
/// active members (≥1 `present`) by whether this is their first-ever active
/// month; `prev_active_count`/`retained_count` are the raw inputs the service
/// turns into `rate` = |上月活躍 ∩ 本月活躍| / |上月活躍| via
/// `service::safe_ratio` (上月空集合 → null). `month` is `YYYY-MM` in the
/// studio timezone.
#[derive(Debug, sqlx::FromRow)]
pub struct RetentionRow {
    pub month: String,
    pub new_count: i64,
    pub returning_count: i64,
    pub prev_active_count: i64,
    pub retained_count: i64,
}

/// One weekday bucket of `repository::weekday_load` — `weekday` is
/// `0=Sunday..6=Saturday` (PostgreSQL `EXTRACT(DOW)`, contract §3.18), and
/// `present_count` is the `present` attendance headcount on that weekday
/// across the trailing 30 days' materialized sessions (zero-filled to all 7).
#[derive(Debug, sqlx::FromRow)]
pub struct WeekdayLoadRow {
    pub weekday: i16,
    pub present_count: i64,
}

/// One venue's summed session minutes this studio month (see
/// `repository::venue_usage`). `venue` is the non-NULL
/// `course_schedule_slots.venue` a session resolves to via the reversible
/// `(course_id, day_of_week, start_time)` key; `minutes` is the summed
/// session duration. Not a fixed-bucket dimension — venues with no sessions
/// simply don't appear.
#[derive(Debug, sqlx::FromRow)]
pub struct VenueUsageRow {
    pub venue: String,
    pub minutes: i64,
}

/// The admin report's honest 2-stage 試上→報名 funnel (see
/// `repository::funnel`) — both counts over the trailing 90 studio days:
/// `trial_inquiries` = `contact_inquiries` with `inquiry_type = 'trial'`;
/// `new_enrolments` = `enrolments` created excluding `cancelled`. No
/// fabricated intermediate stages.
#[derive(Debug, sqlx::FromRow)]
pub struct FunnelRow {
    pub trial_inquiries: i64,
    pub new_enrolments: i64,
}

/// One row of `repository::kpis`'s single multi-scalar-subquery SELECT —
/// three this/last studio-month count pairs plus the raw present/absent
/// counts the service turns into `attendance_rate` (present/(present+
/// absent), `leave` 不入分母;無資料月 → null via `service::safe_ratio`).
#[derive(Debug, sqlx::FromRow)]
pub struct KpiRow {
    pub new_members_this: i64,
    pub new_members_last: i64,
    pub new_enrolments_this: i64,
    pub new_enrolments_last: i64,
    pub paid_orders_this: i64,
    pub paid_orders_last: i64,
    pub present_this: i64,
    pub absent_this: i64,
    pub present_last: i64,
    pub absent_last: i64,
}

/// One (month, source) bucket of `repository::income_by_source` — the
/// shared per-source income aggregation `service::admin_report` derives
/// both `revenue_breakdown` (current month) and `income_sources_12m` from.
/// `month` is `YYYY-MM` in the studio timezone; `source` is one of
/// `course`/`ticket`/`membership`/`course_package`/`merchandise`/
/// `venue_rental`; `orders_count` counts distinct orders (or bookings, for
/// `venue_rental`) touching the bucket; `units` sums line quantities (one
/// booking = one unit).
#[derive(Debug, sqlx::FromRow)]
pub struct IncomeSourceRow {
    pub month: String,
    pub source: String,
    pub gross_cents: i64,
    pub orders_count: i64,
    pub units: i64,
}

// ---------------------------------------------------------------------------
// GET /reports/admin/activity
// ---------------------------------------------------------------------------

/// One row of the `GET /reports/admin/activity` UNION across four
/// operational-event sources (new user / paid order / new enrolment / new
/// contact inquiry) — see `repository::recent_activity`. `detail` holds the
/// one piece of source-specific text every `kind`'s label needs (user's
/// name / order's order_number / enrolment's course name / inquiry's
/// subject-or-name); `amount_cents` and `inquiry_type` are populated only
/// by the `order` and `inquiry` branches respectively (`NULL` from the
/// other three). Kept as separate nullable columns — rather than a single
/// "extra" JSONB blob — so each branch's SELECT list stays a plain,
/// readable set of columns. `service::activity_label` is the one place that
/// turns this raw row into the response's (Traditional Chinese) label
/// string.
#[derive(Debug, sqlx::FromRow)]
pub struct ActivityRow {
    pub kind: String,
    pub detail: String,
    pub amount_cents: Option<i64>,
    pub inquiry_type: Option<String>,
    pub occurred_at: DateTime<Utc>,
}
