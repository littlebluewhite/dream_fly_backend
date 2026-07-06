use chrono::{NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use super::model::{CourseScheduleSlot, CourseSession, MyScheduleRow, TodaySessionRow};

// ---------------------------------------------------------------------------
// course_schedule_slots — request (courses' Create/UpdateCourseRequest embed
// `Option<Vec<CourseScheduleSlotEntry>>`) and response.
// ---------------------------------------------------------------------------

/// One weekly-slot entry in a `POST /courses` / `PATCH /courses/{id}` body.
/// Mirrors `coaches::dto::ScheduleEntry` (day_of_week + "HH:MM" strings),
/// plus an optional `venue`.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct CourseScheduleSlotEntry {
    #[validate(range(min = 0, max = 6))]
    pub day_of_week: i16,
    #[validate(length(min = 5, max = 8))]
    pub start_time: String,
    #[validate(length(min = 5, max = 8))]
    pub end_time: String,
    #[validate(length(max = 100))]
    pub venue: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CourseScheduleSlotResponse {
    pub id: Uuid,
    pub day_of_week: i16,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub venue: Option<String>,
}

impl From<CourseScheduleSlot> for CourseScheduleSlotResponse {
    fn from(s: CourseScheduleSlot) -> Self {
        Self {
            id: s.id,
            day_of_week: s.day_of_week,
            start_time: s.start_time,
            end_time: s.end_time,
            venue: s.venue,
        }
    }
}

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
}

impl From<CourseSession> for CourseSessionResponse {
    fn from(s: CourseSession) -> Self {
        Self {
            id: s.id,
            course_id: s.course_id,
            session_date: s.session_date,
            start_time: s.start_time,
            end_time: s.end_time,
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
#[derive(Debug, Serialize)]
pub struct TodaySessionResponse {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub enrolled_count: i64,
}

impl From<TodaySessionRow> for TodaySessionResponse {
    fn from(r: TodaySessionRow) -> Self {
        Self {
            id: r.id,
            course_id: r.course_id,
            course_name: r.course_name,
            start_time: r.start_time,
            end_time: r.end_time,
            enrolled_count: r.enrolled_count,
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
