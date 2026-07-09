//! No new tables — this module is pure cross-module aggregation over
//! `orders`/`users`/`enrolments`/`courses`/`coaches`/`waitlist_entries`/
//! `course_sessions`/`attendance_records`/`conversations`/`messages`/
//! `contact_inquiries` (all owned by other modules' migrations). Only the
//! three multi-column rows (two admin sub-lists plus the activity feed's
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

/// One row of `GET /reports/admin`'s `coaches` list.
#[derive(Debug, sqlx::FromRow)]
pub struct AdminCoachRow {
    pub coach_id: Uuid,
    pub name: String,
    pub course_count: i64,
    pub student_count: i64,
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
