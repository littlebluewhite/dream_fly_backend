use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "product_type", rename_all = "snake_case")]
pub enum ProductType {
    Ticket,
    CoursePackage,
    Membership,
    Merchandise,
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Product {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub product_type: ProductType,
    pub description: Option<String>,
    pub price_cents: i64,
    pub original_price_cents: Option<i64>,
    pub features: Vec<String>,
    pub is_highlighted: bool,
    pub badge: Option<String>,
    pub stock: Option<i32>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
