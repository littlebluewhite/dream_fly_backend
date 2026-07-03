use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use super::model::Product;

#[derive(Debug, Serialize)]
pub struct ProductResponse {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub product_type: String,
    pub description: Option<String>,
    pub price_cents: i64,
    pub original_price_cents: Option<i64>,
    pub features: Vec<String>,
    pub is_highlighted: bool,
    pub badge: Option<String>,
    pub stock: Option<i32>,
    pub valid_days: Option<i32>,
    pub session_count: Option<i32>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<Product> for ProductResponse {
    fn from(p: Product) -> Self {
        let product_type = match p.product_type {
            super::model::ProductType::Ticket => "ticket",
            super::model::ProductType::CoursePackage => "course_package",
            super::model::ProductType::Membership => "membership",
            super::model::ProductType::Merchandise => "merchandise",
        };
        Self {
            id: p.id,
            name: p.name,
            slug: p.slug,
            product_type: product_type.to_string(),
            description: p.description,
            price_cents: p.price_cents,
            original_price_cents: p.original_price_cents,
            features: p.features,
            is_highlighted: p.is_highlighted,
            badge: p.badge,
            stock: p.stock,
            valid_days: p.valid_days,
            session_count: p.session_count,
            is_active: p.is_active,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

#[derive(Debug, Deserialize, Validate)]
pub struct CreateProductRequest {
    #[validate(length(min = 1, max = 200))]
    pub name: String,
    #[validate(length(max = 200))]
    pub slug: Option<String>,
    #[validate(length(min = 1, max = 32))]
    pub product_type: String,
    #[validate(length(max = 5000))]
    pub description: Option<String>,
    #[validate(range(min = 0, max = 100_000_000))]
    pub price_cents: i64,
    #[validate(range(min = 0, max = 100_000_000))]
    pub original_price_cents: Option<i64>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub is_highlighted: bool,
    #[validate(length(max = 50))]
    pub badge: Option<String>,
    #[validate(range(min = 0, max = 1_000_000))]
    pub stock: Option<i32>,
    #[validate(range(min = 1, max = 3650))]
    pub valid_days: Option<i32>,
    #[validate(range(min = 1, max = 1000))]
    pub session_count: Option<i32>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct UpdateProductRequest {
    #[validate(length(min = 1, max = 200))]
    pub name: Option<String>,
    #[validate(length(max = 200))]
    pub slug: Option<String>,
    #[validate(length(min = 1, max = 32))]
    pub product_type: Option<String>,
    #[validate(length(max = 5000))]
    pub description: Option<String>,
    #[validate(range(min = 0, max = 100_000_000))]
    pub price_cents: Option<i64>,
    /// `Some(Some(v))` = set to v, `Some(None)` = clear to NULL, `None` = don't touch
    pub original_price_cents: Option<Option<i64>>,
    pub features: Option<Vec<String>>,
    pub is_highlighted: Option<bool>,
    /// `Some(Some(v))` = set to v, `Some(None)` = clear to NULL, `None` = don't touch
    pub badge: Option<Option<String>>,
    /// `Some(Some(v))` = set to v, `Some(None)` = clear to NULL, `None` = don't touch
    pub stock: Option<Option<i32>>,
    /// `Some(Some(v))` = set to v, `Some(None)` = clear to NULL, `None` = don't touch
    pub valid_days: Option<Option<i32>>,
    /// `Some(Some(v))` = set to v, `Some(None)` = clear to NULL, `None` = don't touch
    pub session_count: Option<Option<i32>>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ProductQuery {
    pub product_type: Option<String>,
}

/// Paginated response envelope for `GET /products`. Matches the shape of
/// the other list endpoints (orders, posts, bookings) so clients get a
/// consistent pagination contract across modules.
#[derive(Debug, Serialize)]
pub struct ProductListResponse {
    pub products: Vec<ProductResponse>,
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
}
