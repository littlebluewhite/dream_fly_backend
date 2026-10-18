use chrono::{DateTime, NaiveTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Coach {
    pub id: Uuid,
    pub user_id: Uuid,
    /// Joined from `users.name` — coaches has no name column of its own.
    pub name: String,
    pub title: String,
    pub bio: Option<String>,
    pub experience: Option<String>,
    pub specialties: Vec<String>,
    pub certifications: Vec<String>,
    pub is_active: bool,
    pub display_order: i32,
    /// 欄位級 `UNIQUE`(`coaches_slug_key`),大小寫敏感——不像 courses/
    /// products/posts/venues 用 `LOWER(slug)` 唯一索引。coaches 沒有以 slug
    /// 查找的端點(明細走 UUID),slug 只是展示欄位;已知落差。
    pub slug: Option<String>,
    pub photo_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct CoachSchedule {
    pub id: Uuid,
    pub coach_id: Uuid,
    pub day_of_week: i16,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub is_available: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct ClockRecord {
    pub id: Uuid,
    pub coach_id: Uuid,
    pub clock_in: DateTime<Utc>,
    pub clock_out: Option<DateTime<Utc>>,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}
