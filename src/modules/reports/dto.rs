use serde::Serialize;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// GET /reports/admin
// ---------------------------------------------------------------------------

/// One entry of `revenue.trend` — oldest first, 12 entries, zero-filled for
/// months with no paid-family revenue (see `repository::revenue_trend`).
#[derive(Debug, Serialize)]
pub struct RevenueMonthPoint {
    pub month: String,
    pub revenue_cents: i64,
}

#[derive(Debug, Serialize)]
pub struct AdminRevenueSection {
    pub this_month_cents: i64,
    pub last_month_cents: i64,
    pub trend: Vec<RevenueMonthPoint>,
}

#[derive(Debug, Serialize)]
pub struct AdminMembersSection {
    pub total: i64,
    pub new_this_month: i64,
    pub active: i64,
}

/// `fill_rate` is `None` when `max_students` is 0 — cannot happen through
/// normal writes (`courses_max_students_pos CHECK (max_students > 0)`), but
/// the divide-by-zero guard is defensive rather than trusting the DB
/// constraint to hold forever (see `service::safe_ratio`).
#[derive(Debug, Serialize)]
pub struct AdminCourseReportRow {
    pub course_id: Uuid,
    pub name: String,
    pub enrolled: i64,
    pub max_students: i32,
    pub fill_rate: Option<f64>,
    pub waitlist_count: i64,
}

#[derive(Debug, Serialize)]
pub struct AdminCoachReportRow {
    pub coach_id: Uuid,
    pub name: String,
    pub course_count: i64,
    pub student_count: i64,
}

#[derive(Debug, Serialize)]
pub struct AdminReportResponse {
    pub revenue: AdminRevenueSection,
    pub members: AdminMembersSection,
    pub courses: Vec<AdminCourseReportRow>,
    pub coaches: Vec<AdminCoachReportRow>,
}

// ---------------------------------------------------------------------------
// GET /reports/coach
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct CoachReportResponse {
    pub today_sessions: i64,
    pub pending_attendance: i64,
    pub unread_messages: i64,
    pub student_count: i64,
    pub attendance_rate_30d: Option<f64>,
}

// ---------------------------------------------------------------------------
// GET /reports/me
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct MemberReportResponse {
    pub attended_total: i64,
    pub attendance_rate: Option<f64>,
    pub points_balance: i64,
    pub active_enrolments: i64,
    pub upcoming_sessions_7d: i64,
}
