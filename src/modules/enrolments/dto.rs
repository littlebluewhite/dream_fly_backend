use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::model::EnrolmentWithCourse;

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
