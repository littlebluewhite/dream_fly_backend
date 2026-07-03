use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::modules::courses::model::CourseLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "enrolment_status", rename_all = "snake_case")]
pub enum EnrolmentStatus {
    Active,
    Cancelled,
}

impl EnrolmentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Bare `enrolments` table row. This is what `enrol_from_purchase_tx`
/// returns — it has no course fields since the checkout flow that calls it
/// already holds the `Course` it validated capacity against.
#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Enrolment {
    pub id: Uuid,
    pub user_id: Uuid,
    pub course_id: Uuid,
    pub order_id: Option<Uuid>,
    pub status: EnrolmentStatus,
    pub enrolled_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `enrolments` JOINed with `courses` for the fields every response needs
/// (`course_name`, `course_level`, `schedule_text`). Kept as its own flat
/// row type (rather than nesting an [`Enrolment`] inside it) because sqlx's
/// derived `FromRow` maps one column per field and has no support for
/// nested structs.
#[derive(Debug, sqlx::FromRow)]
pub struct EnrolmentWithCourse {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub course_level: CourseLevel,
    pub schedule_text: Option<String>,
    pub status: EnrolmentStatus,
    pub enrolled_at: DateTime<Utc>,
}
