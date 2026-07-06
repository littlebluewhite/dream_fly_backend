use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

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

impl From<MyLeaveRequestRow> for LeaveRequestResponse {
    fn from(r: MyLeaveRequestRow) -> Self {
        Self {
            id: r.id,
            course_id: r.course_id,
            course_name: r.course_name,
            session_id: r.session_id,
            session_date: r.session_date,
            start_time: r.start_time,
            reason: r.reason,
            status: r.status.as_str().to_string(),
            makeup_session_id: r.makeup_session_id,
            makeup_session_date: r.makeup_session_date,
            makeup_start_time: r.makeup_start_time,
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
    fn from(r: AdminLeaveRequestRow) -> Self {
        Self {
            id: r.id,
            course_id: r.course_id,
            course_name: r.course_name,
            user_id: r.user_id,
            user_name: r.user_name,
            session_id: r.session_id,
            session_date: r.session_date,
            start_time: r.start_time,
            reason: r.reason,
            status: r.status.as_str().to_string(),
            makeup_session_id: r.makeup_session_id,
            makeup_session_date: r.makeup_session_date,
            makeup_start_time: r.makeup_start_time,
            decided_at: r.decided_at,
            created_at: r.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct LeaveRequestListResponse {
    pub leave_requests: Vec<AdminLeaveRequestResponse>,
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
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
