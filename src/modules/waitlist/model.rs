use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "waitlist_status", rename_all = "snake_case")]
pub enum WaitlistStatus {
    Waiting,
    Cancelled,
}

impl WaitlistStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Bare `waitlist_entries` table row.
#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct WaitlistEntry {
    pub id: Uuid,
    pub user_id: Uuid,
    pub course_id: Uuid,
    pub status: WaitlistStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `waitlist_entries` JOINed with `courses` for the `course_name` field
/// every response needs. Kept as its own flat row type (rather than
/// nesting a [`WaitlistEntry`] inside it) because sqlx's derived `FromRow`
/// maps one column per field and has no support for nested structs
/// (mirrors `enrolments::model::EnrolmentWithCourse`).
#[derive(Debug, sqlx::FromRow)]
pub struct WaitlistEntryWithCourse {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub status: WaitlistStatus,
    pub created_at: DateTime<Utc>,
}
