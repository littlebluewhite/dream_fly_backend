use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use crate::utils::double_option::deserialize_some;
use crate::utils::url_validation::validate_stored_url;

use super::model::Venue;

#[derive(Debug, Serialize)]
pub struct VenueCategoryResponse {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub icon: Option<String>,
    pub display_order: i32,
}

#[derive(Debug, Serialize)]
pub struct VenueResponse {
    pub id: Uuid,
    pub category_id: Option<Uuid>,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub features: Vec<String>,
    pub image_url: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

impl From<Venue> for VenueResponse {
    fn from(v: Venue) -> Self {
        Self {
            id: v.id,
            category_id: v.category_id,
            name: v.name,
            slug: v.slug,
            description: v.description,
            features: v.features,
            image_url: v.image_url,
            is_active: v.is_active,
            created_at: v.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct VenueWithCategoryResponse {
    pub venue: VenueResponse,
    pub category: Option<VenueCategoryResponse>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct CreateVenueRequest {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    #[validate(length(max = 200))]
    pub slug: Option<String>,
    pub category_id: Option<Uuid>,
    #[validate(length(max = 2000))]
    pub description: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[validate(custom(function = "validate_stored_url"))]
    pub image_url: Option<String>,
}

/// Partial update payload for `PATCH /venues/{id}`. Every field optional;
/// `category_id`/`description`/`image_url` use `Option<Option<T>>` (paired
/// with `deserialize_some`) so callers can distinguish "don't touch"
/// (`None`), "set to NULL" (`Some(None)`), and "set to value"
/// (`Some(Some(v))`). No `#[validate]` on those three fields (validator
/// can't express nested `Option` cleanly; the DB schema is the backstop —
/// mirrors `products::dto::UpdateProductRequest`).
#[derive(Debug, Deserialize, Validate)]
pub struct UpdateVenueRequest {
    #[validate(length(min = 1, max = 100))]
    pub name: Option<String>,
    #[validate(length(max = 200))]
    pub slug: Option<String>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub category_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub description: Option<Option<String>>,
    pub features: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub image_url: Option<Option<String>>,
    pub is_active: Option<bool>,
}
