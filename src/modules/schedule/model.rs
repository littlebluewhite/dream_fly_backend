use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "slot_status", rename_all = "lowercase")]
pub enum SlotStatus {
    Available,
    Limited,
    Full,
    Closed,
}

impl SlotStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Limited => "limited",
            Self::Full => "full",
            Self::Closed => "closed",
        }
    }
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct TimeSlot {
    pub id: Uuid,
    pub date: NaiveDate,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub venue_id: Option<Uuid>,
    pub course_id: Option<Uuid>,
    pub capacity: i32,
    pub booked: i32,
    pub status: SlotStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
