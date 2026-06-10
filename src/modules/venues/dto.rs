use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use crate::utils::url_validation::validate_stored_url;

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
