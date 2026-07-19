use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Closed status set for a `leave_requests` row. Mirrors
/// `enrolments::model::EnrolmentStatus`/`attendance::model::AttendanceStatus`'s
/// derive set and `FromStr` pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "leave_status", rename_all = "snake_case")]
pub enum LeaveStatus {
    Pending,
    Approved,
    Rejected,
    Cancelled,
}

impl LeaveStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::str::FromStr for LeaveStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(()),
        }
    }
}

/// Bare `leave_requests` table row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LeaveRequest {
    pub id: Uuid,
    pub enrolment_id: Uuid,
    pub session_id: Uuid,
    pub reason: Option<String>,
    pub status: LeaveStatus,
    pub makeup_session_id: Option<Uuid>,
    pub decided_by: Option<Uuid>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A `course_sessions` row JOINed with its course's `name` — everything
/// `POST /leave-requests` and the makeup target validation need about a
/// session in one query.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SessionContext {
    pub course_id: Uuid,
    pub course_name: String,
    pub session_date: NaiveDate,
    pub start_time: NaiveTime,
}

/// One row of `GET /leave-requests/me` — `leave_requests` JOINed with its
/// enrolment's course and its own/makeup `course_sessions` rows. Field names
/// mirror `LeaveRequestResponse` 1:1 (see `dto.rs`).
#[derive(Debug, sqlx::FromRow)]
pub struct MyLeaveRequestRow {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub session_id: Uuid,
    pub session_date: NaiveDate,
    pub start_time: NaiveTime,
    pub reason: Option<String>,
    pub status: LeaveStatus,
    pub makeup_session_id: Option<Uuid>,
    pub makeup_session_date: Option<NaiveDate>,
    pub makeup_start_time: Option<NaiveTime>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Same shape as [`MyLeaveRequestRow`] plus the student's `user_id`/`name` —
/// feeds `GET /leave-requests` (coach/admin list), which spans multiple
/// students rather than being scoped to the caller.
#[derive(Debug, sqlx::FromRow)]
pub struct AdminLeaveRequestRow {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub user_id: Uuid,
    pub user_name: String,
    pub session_id: Uuid,
    pub session_date: NaiveDate,
    pub start_time: NaiveTime,
    pub reason: Option<String>,
    pub status: LeaveStatus,
    pub makeup_session_id: Option<Uuid>,
    pub makeup_session_date: Option<NaiveDate>,
    pub makeup_start_time: Option<NaiveTime>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Everything `PATCH /leave-requests/{id}` (approve/reject) needs about a
/// leave request in one query: its current status (must be `pending`), the
/// enrolment/session pair to upsert into `attendance_records` on approval,
/// and the course's `coach_id` (authorization) plus `course_name`/
/// `session_date` (the approval/rejection notification's Chinese copy).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LeaveDecisionContext {
    pub status: LeaveStatus,
    pub enrolment_id: Uuid,
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub coach_id: Option<Uuid>,
    pub session_date: NaiveDate,
    pub start_time: NaiveTime,
}

/// Ownership context for `DELETE /leave-requests/{id}` — just enough to
/// check "is this the owning member" and "is it still pending" before the
/// conditional cancel UPDATE.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LeaveRequestOwnerRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub status: LeaveStatus,
}

/// Locked (`FOR UPDATE OF lr`) context for `POST /leave-requests/{id}/makeup`
/// — the leave request's own session/course (to assemble the response and
/// validate "makeup target must be the same course"), its owning
/// `user_id`, current `status`, and current `makeup_session_id`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LeaveRequestForMakeup {
    pub id: Uuid,
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub status: LeaveStatus,
    pub makeup_session_id: Option<Uuid>,
    pub session_date: NaiveDate,
    pub start_time: NaiveTime,
    pub reason: Option<String>,
}
