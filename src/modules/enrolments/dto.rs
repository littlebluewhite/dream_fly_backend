use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::model::{EnrolmentWithCourse, MyEnrolmentRow};

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
/// `attended`/`total` attendance stats (contract §3.12: 出勤統計以已點名場次為準).
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
