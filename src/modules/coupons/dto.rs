use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;
use validator::Validate;

use crate::extractors::pagination::PageMeta;

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

/// Response for `GET /coupons/{code}/validate` — the checkout-flow shape,
/// not the full admin `CouponResponse`. `discount_cents` is always the
/// coupon's face value, never clamped. `applied_discount_cents` is an
/// optional preview of what checkout would actually apply —
/// `min(discount_cents, subtotal_cents)`, the same clamp
/// `orders::pricing::clamp_coupon_discount` uses — populated only when the
/// caller supplies `?subtotal_cents=` (see `ValidateCouponQuery`).
/// `#[serde(skip_serializing_if)]` keeps a response with no `subtotal_cents`
/// byte-for-byte identical to before this field existed; the exact-body
/// assertions in `tests/http_coupons.rs` are the regression net for that.
#[derive(Debug, Serialize)]
pub struct CouponValidateResponse {
    pub code: String,
    pub discount_cents: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_discount_cents: Option<i64>,
}

/// Query parameters for `GET /coupons/{code}/validate`. `subtotal_cents` is
/// optional; omitting it is exactly the pre-existing behavior described on
/// `CouponValidateResponse`.
#[derive(Debug, Deserialize)]
pub struct ValidateCouponQuery {
    pub subtotal_cents: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct CouponListResponse {
    pub coupons: Vec<CouponResponse>,
    #[serde(flatten)]
    pub meta: PageMeta,
}

#[derive(Debug, Deserialize, Validate)]
pub struct CreateCouponRequest {
    #[validate(length(min = 1, max = 50))]
    pub code: String,
    #[validate(range(min = 1))]
    pub discount_cents: i64,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Plain `Option<Option<T>>` cannot distinguish "key absent" from "key
/// present with JSON `null`" — serde's built-in `Option<T>` deserialize
/// collapses a `null` straight to the *outer* `None`, so a bare
/// `Option<Option<T>>` field could never actually clear a nullable column
/// back to `NULL` via PATCH. Paired with `#[serde(default)]`, this makes the
/// present-with-`null` case reach the *inner* `Option`, producing
/// `Some(None)` (clear) instead of `None` (don't touch) — mirrors
/// `venues::dto::deserialize_some` / `rewards::dto::deserialize_some`.
fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

/// Partial update payload for `PATCH /coupons/{id}`. `code` is intentionally
/// absent — it's the identifier already handed out to customers and must
/// never change post-creation. `expires_at` uses `Option<Option<DateTime<Utc>>>`
/// (paired with `deserialize_some`) so callers can distinguish "don't touch"
/// (`None`), "clear to permanently valid" (`Some(None)`), and "set to a new
/// expiry" (`Some(Some(v))`). No `#[validate]` on `expires_at` (validator
/// can't express nested `Option` cleanly — mirrors
/// `venues::dto::UpdateVenueRequest`).
#[derive(Debug, Deserialize, Validate)]
pub struct UpdateCouponRequest {
    #[validate(range(min = 1))]
    pub discount_cents: Option<i64>,
    pub is_active: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub expires_at: Option<Option<DateTime<Utc>>>,
}
