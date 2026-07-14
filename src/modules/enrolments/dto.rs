use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::model::{EnrolmentAttendanceRow, EnrolmentWithCourse, MyEnrolmentRow};

#[derive(Debug, Serialize)]
pub struct EnrolmentResponse {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub course_level: String,
    pub schedule_text: Option<String>,
    pub status: String,
    pub enrolled_at: DateTime<Utc>,
}

impl From<EnrolmentWithCourse> for EnrolmentResponse {
    fn from(e: EnrolmentWithCourse) -> Self {
        Self {
            id: e.id,
            course_id: e.course_id,
            course_name: e.course_name,
            course_level: e.course_level.as_str().to_string(),
            schedule_text: e.schedule_text,
            status: e.status.as_str().to_string(),
            enrolled_at: e.enrolled_at,
        }
    }
}

/// `GET /enrolments/me` response entry — `EnrolmentResponse`'s fields plus
/// `attended`/`total` attendance stats (contract §3.12: `countable_attendance`
/// caliber — `present`/`absent` count toward `total`, `leave` and
/// never-marked sessions don't; `attended` is the `present` subset).
#[derive(Debug, Serialize)]
pub struct MyEnrolmentResponse {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub course_level: String,
    pub schedule_text: Option<String>,
    pub status: String,
    pub enrolled_at: DateTime<Utc>,
    pub attended: i64,
    pub total: i64,
}

impl From<MyEnrolmentRow> for MyEnrolmentResponse {
    fn from(e: MyEnrolmentRow) -> Self {
        Self {
            id: e.id,
            course_id: e.course_id,
            course_name: e.course_name,
            course_level: e.course_level.as_str().to_string(),
            schedule_text: e.schedule_text,
            status: e.status.as_str().to_string(),
            enrolled_at: e.enrolled_at,
            attended: e.attended,
            total: e.total,
        }
    }
}

/// `GET /enrolments/{id}/attendance` response entry — this enrolment's
/// per-session attendance timeline, oldest to newest (contract §3.12; see
/// also §3.19 Attendance for the `status` enum's meaning).
#[derive(Debug, Serialize)]
pub struct AttendanceEntryResponse {
    pub session_date: NaiveDate,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub status: String,
    pub marked_at: DateTime<Utc>,
}

impl From<EnrolmentAttendanceRow> for AttendanceEntryResponse {
    fn from(r: EnrolmentAttendanceRow) -> Self {
        Self {
            session_date: r.session_date,
            start_time: r.start_time,
            end_time: r.end_time,
            status: r.status.as_str().to_string(),
            marked_at: r.marked_at,
        }
    }
}
