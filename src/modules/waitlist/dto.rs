use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use super::model::WaitlistEntryWithCourse;

#[derive(Debug, Serialize)]
pub struct WaitlistResponse {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

impl From<WaitlistEntryWithCourse> for WaitlistResponse {
    fn from(w: WaitlistEntryWithCourse) -> Self {
        Self {
            id: w.id,
            course_id: w.course_id,
            course_name: w.course_name,
            status: w.status.as_str().to_string(),
            created_at: w.created_at,
        }
    }
}

#[derive(Debug, Deserialize, Validate)]
pub struct CreateWaitlistRequest {
    pub course_id: Uuid,
}

/// Query params for the admin-only `GET /waitlist?course_id=` listing.
/// Deliberately plain `Deserialize` (no `Validate`) — the endpoint's own
/// extractor impl (see `handlers.rs`) maps a missing/invalid `course_id`
/// straight to `AppError::Validation` (422) via axum's `Query` rejection.
#[derive(Debug, Deserialize)]
pub struct WaitlistQuery {
    pub course_id: Uuid,
}
