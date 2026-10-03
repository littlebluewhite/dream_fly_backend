use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ts_rs::TS)]
#[sqlx(type_name = "post_category", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PostCategory {
    Announcement,
    Article,
    Promotion,
    Event,
}

impl PostCategory {
    /// Every variant, in wire-spelling order — single owner of the value
    /// domain; `FromStr` and the allowed-values text both derive from this
    /// instead of hand-copying the list.
    pub const ALL: [Self; 4] = [
        Self::Announcement,
        Self::Article,
        Self::Promotion,
        Self::Event,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Announcement => "announcement",
            Self::Article => "article",
            Self::Promotion => "promotion",
            Self::Event => "event",
        }
    }
}

impl std::str::FromStr for PostCategory {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // 既有 wire 政策：大小寫不敏感，先轉小寫再比對。
        let s = s.to_lowercase();
        Self::ALL.into_iter().find(|v| v.as_str() == s).ok_or(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ts_rs::TS)]
#[sqlx(type_name = "post_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PostStatus {
    Draft,
    Published,
    Archived,
}

impl PostStatus {
    /// Every variant, in wire-spelling order — single owner of the value
    /// domain; `FromStr` and the allowed-values text both derive from this
    /// instead of hand-copying the list.
    pub const ALL: [Self; 3] = [Self::Draft, Self::Published, Self::Archived];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Published => "published",
            Self::Archived => "archived",
        }
    }
}

impl std::str::FromStr for PostStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // 既有 wire 政策：大小寫不敏感，先轉小寫再比對。
        let s = s.to_lowercase();
        Self::ALL.into_iter().find(|v| v.as_str() == s).ok_or(())
    }
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Post {
    pub id: Uuid,
    pub author_id: Uuid,
    pub title: String,
    pub slug: String,
    pub content: String,
    pub excerpt: Option<String>,
    pub category: PostCategory,
    pub status: PostStatus,
    pub cover_image: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
