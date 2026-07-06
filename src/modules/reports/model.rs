//! No new tables — this module is pure cross-module aggregation over
//! `orders`/`users`/`enrolments`/`courses`/`coaches`/`waitlist_entries`/
//! `course_sessions`/`attendance_records`/`conversations`/`messages` (all
//! owned by other modules' migrations). Only the two multi-column admin
//! sub-list rows get a named `FromRow` struct here; every other aggregate
//! query decodes straight into a scalar or tuple in `repository.rs`
//! (mirrors `sessions::repository::materialize_range`'s tuple-decoded
//! candidates query — no dedicated struct needed for a handful of
//! primitive columns).

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
