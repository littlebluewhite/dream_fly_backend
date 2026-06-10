use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use super::model::Course;

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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
            created_at: c.created_at,
            updated_at: c.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CourseListResponse {
    pub courses: Vec<CourseResponse>,
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
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
}

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
    pub min_age: Option<Option<i32>>,
    pub max_age: Option<Option<i32>>,
    pub features: Option<Vec<String>>,
    pub coach_id: Option<Option<Uuid>>,
}
