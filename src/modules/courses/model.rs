use chrono::{DateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "course_level", rename_all = "lowercase")]
pub enum CourseLevel {
    Foundation,
    Beginner,
    Intermediate,
    Advanced,
    Elite,
}

impl CourseLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Foundation => "foundation",
            Self::Beginner => "beginner",
            Self::Intermediate => "intermediate",
            Self::Advanced => "advanced",
            Self::Elite => "elite",
        }
    }
}

impl std::str::FromStr for CourseLevel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "foundation" => Ok(Self::Foundation),
            "beginner" => Ok(Self::Beginner),
            "intermediate" => Ok(Self::Intermediate),
            "advanced" => Ok(Self::Advanced),
            "elite" => Ok(Self::Elite),
            _ => Err(()),
        }
    }
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Course {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub level: CourseLevel,
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
    /// Computed via a correlated subquery against `enrolments` (not a table
    /// column) — inlined at each query site in `repository.rs`; the COUNT
    /// predicate's owner is `courses::seats` (see that module's doc).
    pub enrolled_count: i64,
    /// Computed via a correlated subquery against `waitlist_entries` (not a
    /// table column) — inlined at each query site in `repository.rs`.
    pub waitlist_count: i64,
}

/// A course's structured weekly meeting pattern — one row per (day_of_week,
/// start_time). Mirrors `coach_schedules`' shape. `day_of_week` is 0=Sunday
/// .. 6=Saturday (PostgreSQL `EXTRACT(DOW)` convention — see
/// `repository::materialize_range`).
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct CourseScheduleSlot {
    pub id: Uuid,
    pub course_id: Uuid,
    pub day_of_week: i16,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub venue: Option<String>,
    pub created_at: DateTime<Utc>,
}
