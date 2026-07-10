use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::modules::attendance::model::AttendanceStatus;
use crate::modules::courses::model::CourseLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "enrolment_status", rename_all = "snake_case")]
pub enum EnrolmentStatus {
    Active,
    Cancelled,
}

impl EnrolmentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Bare `enrolments` table row. This is what `enrol_from_purchase_tx`
/// returns — it has no course fields since the checkout flow that calls it
/// already holds the `Course` it validated capacity against.
#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Enrolment {
    pub id: Uuid,
    pub user_id: Uuid,
    pub course_id: Uuid,
    pub order_id: Option<Uuid>,
    pub status: EnrolmentStatus,
    pub enrolled_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `enrolments` JOINed with `courses` for the fields every response needs
/// (`course_name`, `course_level`, `schedule_text`). Kept as its own flat
/// row type (rather than nesting an [`Enrolment`] inside it) because sqlx's
/// derived `FromRow` maps one column per field and has no support for
/// nested structs.
#[derive(Debug, sqlx::FromRow)]
pub struct EnrolmentWithCourse {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub course_level: CourseLevel,
    pub schedule_text: Option<String>,
    pub status: EnrolmentStatus,
    pub enrolled_at: DateTime<Utc>,
}

/// Same shape as [`EnrolmentWithCourse`] plus two attendance-stat columns,
/// aggregated via a `LEFT JOIN attendance_records` in
/// `repository::find_by_user_with_course` — feeds `GET /enrolments/me` only.
/// Kept as a separate row type (rather than adding these columns to
/// `EnrolmentWithCourse` itself) because that struct is also decoded from
/// `cancel_if_active_tx`'s `RETURNING` and from `repository::find_by_order`'s
/// checkout-summary query, neither of which computes attendance stats.
#[derive(Debug, sqlx::FromRow)]
pub struct MyEnrolmentRow {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub course_level: CourseLevel,
    pub schedule_text: Option<String>,
    pub status: EnrolmentStatus,
    pub enrolled_at: DateTime<Utc>,
    /// Count of this enrolment's `attendance_records` with `status = 'present'`.
    pub attended: i64,
    /// Count of this enrolment's `attendance_records` total (i.e. how many
    /// sessions have been marked for it so far, regardless of status).
    pub total: i64,
}

/// One row of `GET /enrolments/{id}/attendance` — a single marked session
/// for one enrolment, JOINed with `course_sessions` for the date/time
/// fields. Reuses `attendance::model::AttendanceStatus` directly rather than
/// redefining the enum (same closed set, same `sqlx::Type` mapping).
#[derive(Debug, sqlx::FromRow)]
pub struct EnrolmentAttendanceRow {
    pub session_date: NaiveDate,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub status: AttendanceStatus,
    pub marked_at: DateTime<Utc>,
}
