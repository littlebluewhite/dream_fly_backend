use chrono::{NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use super::model::TimeSlot;

#[derive(Debug, Deserialize)]
pub struct ScheduleQuery {
    pub year: i32,
    pub month: u32,
}

#[derive(Debug, Deserialize)]
pub struct AvailabilityQuery {
    pub date: String,
}

#[derive(Debug, Serialize)]
pub struct TimeSlotResponse {
    pub id: Uuid,
    pub date: NaiveDate,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub venue_id: Option<Uuid>,
    pub course_id: Option<Uuid>,
    pub capacity: i32,
    pub booked: i32,
    pub status: String,
}

impl From<TimeSlot> for TimeSlotResponse {
    fn from(ts: TimeSlot) -> Self {
        Self {
            id: ts.id,
            date: ts.date,
            start_time: ts.start_time,
            end_time: ts.end_time,
            venue_id: ts.venue_id,
            course_id: ts.course_id,
            capacity: ts.capacity,
            booked: ts.booked,
            status: ts.status.as_str().to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DaySchedule {
    pub date: NaiveDate,
    pub slots: Vec<TimeSlotResponse>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct CreateSlotsRequest {
    #[validate(length(min = 1))]
    #[validate(nested)]
    pub slots: Vec<SlotEntry>,
}

#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct SlotEntry {
    #[validate(length(min = 10, max = 10))]
    pub date: String,
    #[validate(length(min = 5, max = 8))]
    pub start_time: String,
    #[validate(length(min = 5, max = 8))]
    pub end_time: String,
    pub venue_id: Option<Uuid>,
    pub course_id: Option<Uuid>,
    #[validate(range(min = 1, max = 10000))]
    pub capacity: i32,
}
