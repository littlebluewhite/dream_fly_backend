use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use strum::VariantArray;
use uuid::Uuid;

/// Closed status set for a `leave_requests` row. Mirrors
/// `enrolments::model::EnrolmentStatus`/`attendance::model::AttendanceStatus`'s
/// derive set and `FromStr` pattern.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ts_rs::TS, VariantArray,
)]
#[sqlx(type_name = "leave_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum LeaveStatus {
    Pending,
    Approved,
    Rejected,
    Cancelled,
}

impl LeaveStatus {
    /// Every variant, in wire-spelling order — single owner of the value
    /// domain; `FromStr` derives from this instead of hand-copying the list.
    pub const ALL: &[Self] = Self::VARIANTS;

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
        Self::ALL.iter().find(|v| v.as_str() == s).copied().ok_or(())
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
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub session_date: NaiveDate,
    pub start_time: NaiveTime,
}

/// The leave-request read projection's one row shape — `leave_requests`
/// JOINed with its enrolment's course and its own/makeup `course_sessions`
/// rows. Field names mirror `LeaveRequestResponse` 1:1 (see `dto.rs`). Single
/// owner of the projection's column list: `repository::VIEW_COLUMNS`/
/// `VIEW_JOINS` assemble it for every read (`find_my_leave_requests`,
/// `find_admin_list`) and every write that responds with it (`insert`,
/// `decide_tx`, `set_makeup_session_tx`, via a data-modifying CTE's
/// `RETURNING` re-joined through the same two consts) — a write's response is
/// this same row, not a hand-copied echo of it. `cancel_if_pending_tx`/
/// `cancel_pending_for_enrolments_tx` also change the row but return the bare
/// `LeaveRequest`, not this view — `DELETE` has no response body to project.
#[derive(Debug, sqlx::FromRow)]
pub struct LeaveRequestView {
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

/// Same shape as [`LeaveRequestView`] plus the student's `user_id`/`name` —
/// feeds `GET /leave-requests` (coach/admin list), which spans multiple
/// students rather than being scoped to the caller. `#[sqlx(flatten)]` (this
/// repo's first use) keeps [`LeaveRequestView`] the projection's single
/// owner even here, rather than re-declaring its 13 fields.
#[derive(Debug, sqlx::FromRow)]
pub struct AdminLeaveRequestRow {
    #[sqlx(flatten)]
    pub view: LeaveRequestView,
    pub user_id: Uuid,
    pub user_name: String,
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
    /// Whether `enrolment_id` is still in `active_enrolments` — a cancelled
    /// enrolment may still be rejected but no longer approved (task 3).
    pub enrolment_active: bool,
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
    /// Whether the leave's enrolment is still in `active_enrolments` — a
    /// cancelled enrolment can no longer book a makeup (task 3).
    pub enrolment_active: bool,
}
