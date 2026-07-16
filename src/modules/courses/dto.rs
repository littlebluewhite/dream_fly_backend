use chrono::{DateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use crate::extractors::pagination::PageMeta;
use crate::utils::double_option::deserialize_some;

use super::model::{Course, CourseScheduleSlot};

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

#[derive(Debug, Serialize)]
pub struct CourseResponse {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub level: String,
    pub description: Option<String>,
    pub duration_minutes: i32,
    pub price_cents: i64,
    pub max_students: i32,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    pub features: Vec<String>,
    pub is_active: bool,
    pub coach_id: Option<Uuid>,
    pub category: Option<String>,
    pub schedule_text: Option<String>,
    pub is_highlighted: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub enrolled_count: i64,
    pub waitlist_count: i64,
}

impl From<Course> for CourseResponse {
    fn from(c: Course) -> Self {
        Self {
            id: c.id,
            name: c.name,
            slug: c.slug,
            level: c.level.as_str().to_string(),
            description: c.description,
            duration_minutes: c.duration_minutes,
            price_cents: c.price_cents,
            max_students: c.max_students,
            min_age: c.min_age,
            max_age: c.max_age,
            features: c.features,
            is_active: c.is_active,
            coach_id: c.coach_id,
            category: c.category,
            schedule_text: c.schedule_text,
            is_highlighted: c.is_highlighted,
            created_at: c.created_at,
            updated_at: c.updated_at,
            enrolled_count: c.enrolled_count,
            waitlist_count: c.waitlist_count,
        }
    }
}

/// `GET /courses/{id}` (and `POST`/`PATCH`) response — `CourseResponse` plus
/// `schedule_slots`. Deliberately not used by the list endpoint
/// (`CourseListResponse` stays `Vec<CourseResponse>`) to avoid an N+1 slots
/// query per row — see docs/api/integration-contract.md §3.3.
#[derive(Debug, Serialize)]
pub struct CourseDetailResponse {
    #[serde(flatten)]
    pub course: CourseResponse,
    pub schedule_slots: Vec<CourseScheduleSlotResponse>,
}

#[derive(Debug, Serialize)]
pub struct CourseListResponse {
    pub courses: Vec<CourseResponse>,
    #[serde(flatten)]
    pub meta: PageMeta,
}

#[derive(Debug, Deserialize, Validate)]
pub struct CreateCourseRequest {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    #[validate(length(max = 200))]
    pub slug: Option<String>,
    #[validate(length(min = 1, max = 32))]
    pub level: String,
    #[validate(length(max = 5000))]
    pub description: Option<String>,
    #[validate(range(min = 1, max = 1440))]
    pub duration_minutes: i32,
    #[validate(range(min = 0, max = 100_000_000))]
    pub price_cents: i64,
    #[validate(range(min = 1, max = 10_000))]
    pub max_students: i32,
    #[validate(range(min = 0, max = 150))]
    pub min_age: Option<i32>,
    #[validate(range(min = 0, max = 150))]
    pub max_age: Option<i32>,
    pub features: Option<Vec<String>>,
    pub coach_id: Option<Uuid>,
    #[validate(length(max = 50))]
    pub category: Option<String>,
    #[validate(length(max = 100))]
    pub schedule_text: Option<String>,
    #[serde(default)]
    pub is_highlighted: bool,
    #[validate(nested)]
    pub schedule_slots: Option<Vec<CourseScheduleSlotEntry>>,
}

/// Partial update payload for `PATCH /courses/{id}`. Every field optional;
/// `min_age`/`max_age`/`coach_id`/`category`/`schedule_text` use
/// `Option<Option<T>>` (paired with `deserialize_some`) so callers can
/// distinguish "don't touch" (`None`), "set to NULL" (`Some(None)`), and
/// "set to value" (`Some(Some(v))`) — those five columns are the nullable
/// ones. No `#[validate]` on those five fields (validator can't express
/// nested `Option` cleanly; the DB schema is the backstop — mirrors
/// `venues::dto::UpdateVenueRequest`).
#[derive(Debug, Deserialize, Validate)]
pub struct UpdateCourseRequest {
    #[validate(length(min = 1, max = 100))]
    pub name: Option<String>,
    #[validate(length(max = 200))]
    pub slug: Option<String>,
    #[validate(length(min = 1, max = 32))]
    pub level: Option<String>,
    #[validate(length(max = 5000))]
    pub description: Option<String>,
    #[validate(range(min = 1, max = 1440))]
    pub duration_minutes: Option<i32>,
    #[validate(range(min = 0, max = 100_000_000))]
    pub price_cents: Option<i64>,
    #[validate(range(min = 1, max = 10_000))]
    pub max_students: Option<i32>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub min_age: Option<Option<i32>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub max_age: Option<Option<i32>>,
    pub features: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub coach_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub category: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub schedule_text: Option<Option<String>>,
    pub is_highlighted: Option<bool>,
    /// Not present (`None`) leaves existing slots untouched; `Some(vec)`
    /// (including an empty vec) replaces the entire set within the same
    /// transaction as the course row update — see `courses::service`.
    #[validate(nested)]
    pub schedule_slots: Option<Vec<CourseScheduleSlotEntry>>,
}
