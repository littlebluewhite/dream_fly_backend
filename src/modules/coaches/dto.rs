use chrono::{DateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Serialize)]
pub struct CoachResponse {
    pub id: Uuid,
    pub user_id: Uuid,
    pub title: String,
    pub bio: Option<String>,
    pub experience: Option<String>,
    pub specialties: Vec<String>,
    pub certifications: Vec<String>,
    pub is_active: bool,
    pub display_order: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct CoachDetailResponse {
    pub coach: CoachResponse,
    pub schedules: Vec<CoachScheduleResponse>,
}

#[derive(Debug, Serialize)]
pub struct CoachScheduleResponse {
    pub id: Uuid,
    pub day_of_week: i16,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub is_available: bool,
}

#[derive(Debug, Deserialize, Validate)]
pub struct UpdateScheduleRequest {
    #[validate(length(max = 100))]
    #[validate(nested)]
    pub schedules: Vec<ScheduleEntry>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct ScheduleEntry {
    #[validate(range(min = 0, max = 6))]
    pub day_of_week: i16,
    #[validate(length(min = 5, max = 8))]
    pub start_time: String,
    #[validate(length(min = 5, max = 8))]
    pub end_time: String,
    pub is_available: bool,
}

#[derive(Debug, Serialize)]
pub struct ClockRecordResponse {
    pub id: Uuid,
    pub clock_in: DateTime<Utc>,
    pub clock_out: Option<DateTime<Utc>>,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct ClockNoteRequest {
    #[validate(length(max = 500))]
    pub note: Option<String>,
}
