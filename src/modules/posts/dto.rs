use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use super::model::Post;
use crate::extractors::pagination::PageMeta;
use crate::utils::double_option::deserialize_some;
use crate::utils::url_validation::validate_stored_url;

/// List view — excludes content for efficiency
#[derive(Debug, Serialize)]
pub struct PostResponse {
    pub id: Uuid,
    pub author_id: Uuid,
    pub title: String,
    pub slug: String,
    pub excerpt: Option<String>,
    pub category: String,
    pub status: String,
    pub cover_image: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl From<Post> for PostResponse {
    fn from(p: Post) -> Self {
        Self {
            id: p.id,
            author_id: p.author_id,
            title: p.title,
            slug: p.slug,
            excerpt: p.excerpt,
            category: p.category.as_str().to_string(),
            status: p.status.as_str().to_string(),
            cover_image: p.cover_image,
            published_at: p.published_at,
            created_at: p.created_at,
        }
    }
}

/// Detail view — includes content and updated_at
#[derive(Debug, Serialize)]
pub struct PostDetailResponse {
    pub id: Uuid,
    pub author_id: Uuid,
    pub title: String,
    pub slug: String,
    pub content: String,
    pub excerpt: Option<String>,
    pub category: String,
    pub status: String,
    pub cover_image: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<Post> for PostDetailResponse {
    fn from(p: Post) -> Self {
        Self {
            id: p.id,
            author_id: p.author_id,
            title: p.title,
            slug: p.slug,
            content: p.content,
            excerpt: p.excerpt,
            category: p.category.as_str().to_string(),
            status: p.status.as_str().to_string(),
            cover_image: p.cover_image,
            published_at: p.published_at,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PostListResponse {
    pub posts: Vec<PostResponse>,
    #[serde(flatten)]
    pub meta: PageMeta,
}

#[derive(Debug, Deserialize, Validate)]
pub struct CreatePostRequest {
    #[validate(length(min = 1, max = 200))]
    pub title: String,
    #[validate(length(max = 200))]
    pub slug: Option<String>,
    #[validate(length(min = 1, max = 100_000))]
    pub content: String,
    #[validate(length(max = 500))]
    pub excerpt: Option<String>,
    #[validate(length(min = 1, max = 50))]
    pub category: String,
    #[validate(custom(function = "validate_stored_url"))]
    pub cover_image: Option<String>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct UpdatePostRequest {
    #[validate(length(min = 1, max = 200))]
    pub title: Option<String>,
    #[validate(length(max = 200))]
    pub slug: Option<String>,
    #[validate(length(min = 1, max = 100_000))]
    pub content: Option<String>,
    /// `Some(Some(v))` = set, `Some(None)` = clear to NULL, `None` = don't touch
    #[serde(default, deserialize_with = "deserialize_some")]
    pub excerpt: Option<Option<String>>,
    #[validate(length(max = 50))]
    pub category: Option<String>,
    #[validate(length(max = 50))]
    pub status: Option<String>,
    /// `Some(Some(v))` = set, `Some(None)` = clear to NULL, `None` = don't touch
    #[serde(default, deserialize_with = "deserialize_some")]
    pub cover_image: Option<Option<String>>,
}
