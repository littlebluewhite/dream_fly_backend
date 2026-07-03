use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use super::model::Coupon;

#[derive(Debug, Serialize)]
pub struct CouponResponse {
    pub id: Uuid,
    pub code: String,
    pub discount_cents: i64,
    pub is_active: bool,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl From<Coupon> for CouponResponse {
    fn from(c: Coupon) -> Self {
        Self {
            id: c.id,
            code: c.code,
            discount_cents: c.discount_cents,
            is_active: c.is_active,
            expires_at: c.expires_at,
            created_at: c.created_at,
        }
    }
}

/// Response for `GET /coupons/{code}/validate` — intentionally just the two
/// fields a checkout flow needs, not the full admin `CouponResponse` shape.
#[derive(Debug, Serialize)]
pub struct CouponValidateResponse {
    pub code: String,
    pub discount_cents: i64,
}

#[derive(Debug, Serialize)]
pub struct CouponListResponse {
    pub coupons: Vec<CouponResponse>,
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
}

#[derive(Debug, Deserialize, Validate)]
pub struct CreateCouponRequest {
    #[validate(length(min = 1, max = 50))]
    pub code: String,
    #[validate(range(min = 1))]
    pub discount_cents: i64,
    pub expires_at: Option<DateTime<Utc>>,
}
