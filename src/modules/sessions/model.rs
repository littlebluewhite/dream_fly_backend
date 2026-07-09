use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::Serialize;
use uuid::Uuid;

/// A course's structured weekly meeting pattern — one row per (day_of_week,
/// start_time). Mirrors `coach_schedules`' shape. `day_of_week` is 0=Sunday
/// .. 6=Saturday (PostgreSQL `EXTRACT(DOW)` convention — see
/// `repository::materialize_range`).
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct CourseScheduleSlot {
    pub id: Uuid,
    pub course_id: Uuid,
    pub day_of_week: i16,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub venue: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A materialized calendar-date occurrence of a `CourseScheduleSlot`. No
/// `status` column — v1 has no course-suspension feature, and "live"/"done"
/// are derived from wall-clock time by the caller rather than stored.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct CourseSession {
    pub id: Uuid,
    pub course_id: Uuid,
    pub session_date: NaiveDate,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub created_at: DateTime<Utc>,
}

/// One row of `GET /sessions/today` — a materialized session JOINed with its
/// course name, plus a live count of that course's active enrolments (same
/// correlated-subquery pattern as `courses::model::Course::enrolled_count`).
/// Round 4 Task B8 additive fields: `coach_name` (JOIN courses -> coaches ->
/// users, `None` when the course has no assigned coach) and `venue`
/// (resolved by rejoining `course_schedule_slots` on the session's derived
/// `(course_id, day_of_week, start_time)` — the same reversible key
/// `course_schedule_slots_unique` enforces — `None` when no slot matches,
/// e.g. the slot was edited or deleted after this session materialized).
#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct TodaySessionRow {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub coach_name: Option<String>,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub enrolled_count: i64,
    pub venue: Option<String>,
}

/// One row of `GET /schedule/me` — a course's weekly slot (not a materialized
/// session) JOINed with its course name and coach name. `coach_name` is
/// `None` when the course has no assigned coach (`courses.coach_id` is
/// nullable).
#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct MyScheduleRow {
    pub course_id: Uuid,
    pub course_name: String,
    pub coach_name: Option<String>,
    pub day_of_week: i16,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub venue: Option<String>,
}
