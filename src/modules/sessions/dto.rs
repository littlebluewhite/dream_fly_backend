use chrono::{NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::model::{CourseSession, MyScheduleRow, SessionStatus, TodaySessionRow};

// ---------------------------------------------------------------------------
// course_sessions — GET /courses/{id}/sessions
// ---------------------------------------------------------------------------

/// Query params for `GET /courses/{id}/sessions?from=&to=`. Both are optional
/// `YYYY-MM-DD` strings — defaults (today / +28 days) and range validation
/// (>60 days, or to < from -> 422) are applied in `service`.
#[derive(Debug, Deserialize)]
pub struct SessionsRangeQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CourseSessionResponse {
    pub id: Uuid,
    pub course_id: Uuid,
    pub session_date: NaiveDate,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub status: String,
}

impl CourseSessionResponse {
    pub fn from_session(s: CourseSession, status: SessionStatus) -> Self {
        Self {
            id: s.id,
            course_id: s.course_id,
            session_date: s.session_date,
            start_time: s.start_time,
            end_time: s.end_time,
            status: status.as_str().to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// GET /sessions/today
// ---------------------------------------------------------------------------

/// Note: `id` is included even though the task brief's field list for this
/// endpoint didn't enumerate it — Task 2 (attendance/roster) needs a session
/// id to record attendance against, and this task is documented as its
/// foundation, so omitting it would just force an extra round-trip. Flagged
/// in the task report as a deliberate, low-risk addition.
///
/// Round 4 Task B8 added `coach_name`/`venue` (additive — shared by both the
/// coach and admin branches of `GET /sessions/today`, see `sessions::
/// service::today_sessions`). Both are nullable: `coach_name` when the
/// course has no assigned coach, `venue` when no `course_schedule_slots` row
/// matches the session's derived `(course_id, day_of_week, start_time)`.
#[derive(Debug, Serialize)]
pub struct TodaySessionResponse {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub coach_name: Option<String>,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub enrolled_count: i64,
    pub venue: Option<String>,
    pub status: String,
}

impl TodaySessionResponse {
    pub fn from_row(r: TodaySessionRow, status: SessionStatus) -> Self {
        Self {
            id: r.id,
            course_id: r.course_id,
            course_name: r.course_name,
            coach_name: r.coach_name,
            start_time: r.start_time,
            end_time: r.end_time,
            enrolled_count: r.enrolled_count,
            venue: r.venue,
            status: status.as_str().to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// GET /schedule/me
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct MyScheduleEntryResponse {
    pub course_id: Uuid,
    pub course_name: String,
    pub coach_name: Option<String>,
    pub day_of_week: i16,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub venue: Option<String>,
}

impl From<MyScheduleRow> for MyScheduleEntryResponse {
    fn from(r: MyScheduleRow) -> Self {
        Self {
            course_id: r.course_id,
            course_name: r.course_name,
            coach_name: r.coach_name,
            day_of_week: r.day_of_week,
            start_time: r.start_time,
            end_time: r.end_time,
            venue: r.venue,
        }
    }
}
