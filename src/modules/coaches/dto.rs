use chrono::{DateTime, NaiveTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;
use validator::Validate;

use crate::utils::url_validation::validate_stored_url;

use super::model::{ClockRecord, Coach, CoachSchedule};

#[derive(Debug, Serialize)]
pub struct CoachResponse {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub title: String,
    pub bio: Option<String>,
    pub experience: Option<String>,
    pub specialties: Vec<String>,
    pub certifications: Vec<String>,
    pub is_active: bool,
    pub display_order: i32,
    pub slug: Option<String>,
    pub photo_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<Coach> for CoachResponse {
    fn from(c: Coach) -> Self {
        Self {
            id: c.id,
            user_id: c.user_id,
            name: c.name,
            title: c.title,
            bio: c.bio,
            experience: c.experience,
            specialties: c.specialties,
            certifications: c.certifications,
            is_active: c.is_active,
            display_order: c.display_order,
            slug: c.slug,
            photo_url: c.photo_url,
            created_at: c.created_at,
        }
    }
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

impl From<CoachSchedule> for CoachScheduleResponse {
    fn from(s: CoachSchedule) -> Self {
        Self {
            id: s.id,
            day_of_week: s.day_of_week,
            start_time: s.start_time,
            end_time: s.end_time,
            is_available: s.is_available,
        }
    }
}

/// `POST /coaches` (admin). Binds an existing user (created via the
/// existing `POST /users`) to a coach profile. `user_id` is the only field
/// naming *which* user; everything else configures the coach profile
/// itself. `title` is `String` (not `Option`) because `coaches.title` is
/// `NOT NULL` with no `DEFAULT` — mirrors how `venues::CreateVenueRequest.name`
/// handles the same NOT-NULL-without-default shape. `specialties`/
/// `certifications` default to an empty list (matches the column's
/// `DEFAULT '{}'`) and `is_active`/`display_order` are resolved to the
/// column defaults (`true`/`0`) in `service::create_coach` when omitted.
#[derive(Debug, Deserialize, Validate)]
pub struct CreateCoachRequest {
    pub user_id: Uuid,
    #[validate(length(min = 1, max = 100))]
    pub title: String,
    #[validate(length(max = 5000))]
    pub bio: Option<String>,
    #[validate(length(max = 5000))]
    pub experience: Option<String>,
    #[serde(default)]
    pub specialties: Vec<String>,
    #[serde(default)]
    pub certifications: Vec<String>,
    pub is_active: Option<bool>,
    pub display_order: Option<i32>,
    #[validate(length(max = 100))]
    pub slug: Option<String>,
    #[validate(custom(function = "validate_stored_url"))]
    pub photo_url: Option<String>,
}

/// Plain `Option<Option<T>>` cannot distinguish "key absent" from "key
/// present with JSON `null`" — serde's built-in `Option<T>` deserialize
/// collapses a `null` straight to the *outer* `None`, so a bare
/// `Option<Option<T>>` field could never actually clear a nullable column
/// back to `NULL` via PATCH. Paired with `#[serde(default)]`, this makes the
/// present-with-`null` case reach the *inner* `Option`, producing
/// `Some(None)` (clear) instead of `None` (don't touch) — mirrors
/// `venues::dto::deserialize_some` / `rewards::dto::deserialize_some`.
fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

/// Partial update payload for `PATCH /coaches/{id}`. Every field optional;
/// coach name (`users.name`) is out of scope here — that's edited via the
/// existing `PATCH /users/{id}`. `bio`/`experience`/`slug`/`photo_url` use
/// `Option<Option<T>>` (paired with `deserialize_some`) so callers can
/// distinguish "don't touch" (`None`), "set to NULL" (`Some(None)`), and
/// "set to value" (`Some(Some(v))`) — those four columns are the nullable
/// ones. No `#[validate]` on those four fields (validator can't express
/// nested `Option` cleanly; the DB schema is the backstop — mirrors
/// `venues::dto::UpdateVenueRequest`).
#[derive(Debug, Deserialize, Validate)]
pub struct UpdateCoachRequest {
    #[validate(length(min = 1, max = 100))]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub bio: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub experience: Option<Option<String>>,
    pub specialties: Option<Vec<String>>,
    pub certifications: Option<Vec<String>>,
    pub is_active: Option<bool>,
    pub display_order: Option<i32>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub slug: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub photo_url: Option<Option<String>>,
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

impl From<ClockRecord> for ClockRecordResponse {
    fn from(r: ClockRecord) -> Self {
        Self {
            id: r.id,
            clock_in: r.clock_in,
            clock_out: r.clock_out,
            note: r.note,
            created_at: r.created_at,
        }
    }
}

#[derive(Debug, Deserialize, Validate)]
pub struct ClockNoteRequest {
    #[validate(length(max = 500))]
    pub note: Option<String>,
}
