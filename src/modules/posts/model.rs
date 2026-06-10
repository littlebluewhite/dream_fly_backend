use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "post_category", rename_all = "snake_case")]
pub enum PostCategory {
    Announcement,
    Article,
    Promotion,
    Event,
}

impl PostCategory {
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
        match s.to_lowercase().as_str() {
            "announcement" => Ok(Self::Announcement),
            "article" => Ok(Self::Article),
            "promotion" => Ok(Self::Promotion),
            "event" => Ok(Self::Event),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "post_status", rename_all = "snake_case")]
pub enum PostStatus {
    Draft,
    Published,
    Archived,
}

impl PostStatus {
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
        match s.to_lowercase().as_str() {
            "draft" => Ok(Self::Draft),
            "published" => Ok(Self::Published),
            "archived" => Ok(Self::Archived),
            _ => Err(()),
        }
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
