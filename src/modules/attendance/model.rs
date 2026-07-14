use chrono::{NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Closed status set for a single (session, enrolment) attendance mark.
/// Mirrors `enrolments::model::EnrolmentStatus`'s derive set — the closest
/// sibling "closed status enum" in this codebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "attendance_status", rename_all = "snake_case")]
pub enum AttendanceStatus {
    Present,
    Absent,
    Leave,
}

impl AttendanceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Absent => "absent",
            Self::Leave => "leave",
        }
    }
}

impl std::str::FromStr for AttendanceStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "present" => Ok(Self::Present),
            "absent" => Ok(Self::Absent),
            "leave" => Ok(Self::Leave),
            _ => Err(()),
        }
    }
}

/// `course_sessions` JOINed with its course's `coach_id` — used for the
/// coach-ownership authorization check on `GET /sessions/{id}/roster` and
/// `PUT /sessions/{id}/attendance` (a session's course never changes, so
/// this is a cheap single-row lookup, not a listing query), plus
/// `session_date`/`start_time` feeding `PUT /sessions/{id}/attendance`'s
/// "session already started" gate (`studio_clock::require_started`) — `GET
/// .../roster` doesn't gate, so it just ignores these two fields.
#[derive(Debug, sqlx::FromRow)]
pub struct SessionCourseRow {
    pub course_id: Uuid,
    pub coach_id: Option<Uuid>,
    pub session_date: NaiveDate,
    pub start_time: NaiveTime,
}

/// One row of `GET /sessions/{id}/roster` — a course's active enrolments
/// LEFT JOINed with `users` and this specific session's `attendance_records`
/// row (`None` when the student hasn't been marked for this session yet).
#[derive(Debug, sqlx::FromRow)]
pub struct RosterRow {
    pub enrolment_id: Uuid,
    pub user_id: Uuid,
    pub user_name: String,
    pub attendance_status: Option<AttendanceStatus>,
}

/// `{ course_id, course_name, enrolment_id }` — one entry in
/// `MyStudentRow.courses`, decoded straight out of a `jsonb_agg(...)`
/// correlated aggregate (see `repository::find_my_students`). Mirrors
/// `orders::model::OrderItemBrief`'s role (aggregate-row payload reused
/// directly as the response field type). `enrolment_id` is the active
/// enrolment tying this student to this course — the frontend's "write a
/// report card" action needs it to call `POST /report-cards`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudentCourseBrief {
    pub course_id: Uuid,
    pub course_name: String,
    pub enrolment_id: Uuid,
}

/// One distinct student across a coach's active courses' active enrolments,
/// for `GET /coaches/me/students`. `courses` is aggregated with `jsonb_agg`
/// in the same query (see `repository::find_my_students`) — one query for
/// the whole list, not one per student.
#[derive(Debug, sqlx::FromRow)]
pub struct MyStudentRow {
    pub user_id: Uuid,
    pub name: String,
    pub phone: Option<String>,
    pub courses: sqlx::types::Json<Vec<StudentCourseBrief>>,
}
