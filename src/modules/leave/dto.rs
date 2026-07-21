use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use crate::extractors::pagination::PageMeta;

use super::model::{AdminLeaveRequestRow, MyLeaveRequestRow};

// ---------------------------------------------------------------------------
// POST /leave-requests
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Validate)]
pub struct CreateLeaveRequestRequest {
    pub session_id: Uuid,
    #[validate(length(max = 500))]
    pub reason: Option<String>,
}

/// The makeup booking attached to a leave request: the target session's id,
/// date, and start time, moved as one unit. A leave request either has a
/// booked makeup (all three present) or none (all three absent); grouping the
/// three columns behind a single `Option` makes the "half-set" state (id
/// present but date/time null, and vice-versa) unrepresentable at every
/// assembly site (task D5 / ADR-0008). The wire shape stays flat — the two
/// response structs expand this back into three top-level fields (see their
/// `new` constructors); it is only an assembly-time grouping, never serialized.
#[derive(Debug, Clone)]
pub struct MakeupInfo {
    pub session_id: Uuid,
    pub session_date: NaiveDate,
    pub start_time: NaiveTime,
}

impl MakeupInfo {
    /// Zip a leave-request row's three nullable makeup columns into
    /// `Option<MakeupInfo>`. The `/me` and admin-list queries LEFT JOIN the
    /// makeup session, so the three are always all-`Some` (booked) or
    /// all-`None` (not booked); a mixed row would be a query bug and collapses
    /// to `None` here rather than emitting a half-set response.
    fn from_columns(
        session_id: Option<Uuid>,
        session_date: Option<NaiveDate>,
        start_time: Option<NaiveTime>,
    ) -> Option<Self> {
        match (session_id, session_date, start_time) {
            (Some(session_id), Some(session_date), Some(start_time)) => {
                Some(Self { session_id, session_date, start_time })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct LeaveRequestResponse {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub session_id: Uuid,
    pub session_date: NaiveDate,
    pub start_time: NaiveTime,
    pub reason: Option<String>,
    pub status: String,
    pub makeup_session_id: Option<Uuid>,
    pub makeup_session_date: Option<NaiveDate>,
    pub makeup_start_time: Option<NaiveTime>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl LeaveRequestResponse {
    /// Assemble a response, expanding the grouped `makeup` into the three flat
    /// wire fields in one place. Every assembly site — the three `service`
    /// literals and the `From<MyLeaveRequestRow>` below — routes through here,
    /// so "makeup fields are all-set or all-null" is guaranteed by the
    /// `Option<MakeupInfo>` parameter rather than re-checked by hand each time.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: Uuid,
        course_id: Uuid,
        course_name: String,
        session_id: Uuid,
        session_date: NaiveDate,
        start_time: NaiveTime,
        reason: Option<String>,
        status: String,
        makeup: Option<MakeupInfo>,
        decided_at: Option<DateTime<Utc>>,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            course_id,
            course_name,
            session_id,
            session_date,
            start_time,
            reason,
            status,
            makeup_session_id: makeup.as_ref().map(|m| m.session_id),
            makeup_session_date: makeup.as_ref().map(|m| m.session_date),
            makeup_start_time: makeup.map(|m| m.start_time),
            decided_at,
            created_at,
        }
    }
}

impl From<MyLeaveRequestRow> for LeaveRequestResponse {
    fn from(r: MyLeaveRequestRow) -> Self {
        Self::new(
            r.id,
            r.course_id,
            r.course_name,
            r.session_id,
            r.session_date,
            r.start_time,
            r.reason,
            r.status.as_str().to_string(),
            MakeupInfo::from_columns(r.makeup_session_id, r.makeup_session_date, r.makeup_start_time),
            r.decided_at,
            r.created_at,
        )
    }
}

// ---------------------------------------------------------------------------
// GET /leave-requests?status=&course_id= (coach/admin)
// ---------------------------------------------------------------------------

/// Query params for the coach/admin list. Both filters are optional; a
/// present `status` is validated against `LeaveStatus` in `service` (422 on
/// an unrecognized value).
#[derive(Debug, Deserialize)]
pub struct LeaveRequestQuery {
    pub status: Option<String>,
    pub course_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct AdminLeaveRequestResponse {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub user_id: Uuid,
    pub user_name: String,
    pub session_id: Uuid,
    pub session_date: NaiveDate,
    pub start_time: NaiveTime,
    pub reason: Option<String>,
    pub status: String,
    pub makeup_session_id: Option<Uuid>,
    pub makeup_session_date: Option<NaiveDate>,
    pub makeup_start_time: Option<NaiveTime>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl AdminLeaveRequestResponse {
    /// Sibling of [`LeaveRequestResponse::new`] with the extra `user_id`/
    /// `user_name` this coach/admin-facing shape carries. The single
    /// `From<AdminLeaveRequestRow>` assembly site routes through here so the
    /// same all-or-nothing makeup invariant holds.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: Uuid,
        course_id: Uuid,
        course_name: String,
        user_id: Uuid,
        user_name: String,
        session_id: Uuid,
        session_date: NaiveDate,
        start_time: NaiveTime,
        reason: Option<String>,
        status: String,
        makeup: Option<MakeupInfo>,
        decided_at: Option<DateTime<Utc>>,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            course_id,
            course_name,
            user_id,
            user_name,
            session_id,
            session_date,
            start_time,
            reason,
            status,
            makeup_session_id: makeup.as_ref().map(|m| m.session_id),
            makeup_session_date: makeup.as_ref().map(|m| m.session_date),
            makeup_start_time: makeup.map(|m| m.start_time),
            decided_at,
            created_at,
        }
    }
}

impl From<AdminLeaveRequestRow> for AdminLeaveRequestResponse {
    fn from(r: AdminLeaveRequestRow) -> Self {
        Self::new(
            r.id,
            r.course_id,
            r.course_name,
            r.user_id,
            r.user_name,
            r.session_id,
            r.session_date,
            r.start_time,
            r.reason,
            r.status.as_str().to_string(),
            MakeupInfo::from_columns(r.makeup_session_id, r.makeup_session_date, r.makeup_start_time),
            r.decided_at,
            r.created_at,
        )
    }
}

#[derive(Debug, Serialize)]
pub struct LeaveRequestListResponse {
    pub leave_requests: Vec<AdminLeaveRequestResponse>,
    #[serde(flatten)]
    pub meta: PageMeta,
}

// ---------------------------------------------------------------------------
// PATCH /leave-requests/{id}
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Validate)]
pub struct DecideLeaveRequestRequest {
    #[validate(length(min = 1, max = 32))]
    pub status: String,
}

// ---------------------------------------------------------------------------
// POST /leave-requests/{id}/makeup
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Validate)]
pub struct MakeupRequest {
    pub session_id: Uuid,
}
