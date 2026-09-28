use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use crate::extractors::pagination::PageMeta;

use super::model::{AdminLeaveRequestRow, LeaveRequestView};

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
/// response structs' `From` impls expand this back into three top-level
/// fields; it is only an assembly-time grouping, never serialized.
#[derive(Debug, Clone)]
struct MakeupInfo {
    session_id: Uuid,
    session_date: NaiveDate,
    start_time: NaiveTime,
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

impl From<LeaveRequestView> for LeaveRequestResponse {
    /// Sole expansion site for the member-facing shape — every write
    /// (`create`/`decide`/`makeup`, each now handing back its own
    /// [`LeaveRequestView`] straight from the DB) and the `/me` list route
    /// through here. Expands the three nullable makeup columns into
    /// `Option<MakeupInfo>` and back out to the three flat wire fields in one
    /// place, so "all-set or all-null" is guaranteed by the `Option`
    /// rather than re-checked by hand each time.
    fn from(r: LeaveRequestView) -> Self {
        let makeup =
            MakeupInfo::from_columns(r.makeup_session_id, r.makeup_session_date, r.makeup_start_time);
        Self {
            id: r.id,
            course_id: r.course_id,
            course_name: r.course_name,
            session_id: r.session_id,
            session_date: r.session_date,
            start_time: r.start_time,
            reason: r.reason,
            status: r.status.as_str().to_string(),
            makeup_session_id: makeup.as_ref().map(|m| m.session_id),
            makeup_session_date: makeup.as_ref().map(|m| m.session_date),
            makeup_start_time: makeup.map(|m| m.start_time),
            decided_at: r.decided_at,
            created_at: r.created_at,
        }
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

impl From<AdminLeaveRequestRow> for AdminLeaveRequestResponse {
    /// Routes through [`LeaveRequestResponse`]'s conversion for the 11
    /// shared fields, then adds the two admin-only ones — an explicit field
    /// list rather than `#[serde(flatten)]`, so this struct's own
    /// declaration order still controls the wire's key order.
    fn from(r: AdminLeaveRequestRow) -> Self {
        let base = LeaveRequestResponse::from(r.view);
        Self {
            id: base.id,
            course_id: base.course_id,
            course_name: base.course_name,
            user_id: r.user_id,
            user_name: r.user_name,
            session_id: base.session_id,
            session_date: base.session_date,
            start_time: base.start_time,
            reason: base.reason,
            status: base.status,
            makeup_session_id: base.makeup_session_id,
            makeup_session_date: base.makeup_session_date,
            makeup_start_time: base.makeup_start_time,
            decided_at: base.decided_at,
            created_at: base.created_at,
        }
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
