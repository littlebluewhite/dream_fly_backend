use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::pagination::PaginationParams;
use crate::modules::orders::pricing;

use super::dto::{
    CouponListResponse, CouponResponse, CouponValidateResponse, CreateCouponRequest,
    UpdateCouponRequest,
};
use super::repository;

/// `GET /coupons/{code}/validate`. `subtotal_cents`, when supplied, previews
/// `applied_discount_cents` via the same clamp checkout applies
/// (`orders::pricing::clamp_coupon_discount`); `discount_cents` in the
/// response always stays the coupon's face value.
///
/// The negative-`subtotal_cents` check runs *before* the coupon lookup, so
/// precedence is unambiguous: an unknown/expired code combined with a
/// negative `subtotal_cents` is a 422 (bad input), not a 404 (the coupon
/// lookup never runs).
pub async fn validate_coupon(
    db: &PgPool,
    code: &str,
    subtotal_cents: Option<i64>,
) -> Result<CouponValidateResponse, AppError> {
    if let Some(s) = subtotal_cents {
        if s < 0 {
            return Err(AppError::Validation("subtotal_cents must be >= 0".into()));
        }
    }

    let coupon = repository::find_valid_by_code(db, code)
        .await?
        .ok_or_else(|| AppError::NotFound("coupon not found".into()))?;

    let applied_discount_cents =
        subtotal_cents.map(|s| pricing::clamp_coupon_discount(coupon.discount_cents, s));

    Ok(CouponValidateResponse {
        code: coupon.code,
        discount_cents: coupon.discount_cents,
        applied_discount_cents,
    })
}

pub async fn create_coupon(
    db: &PgPool,
    req: CreateCouponRequest,
) -> Result<CouponResponse, AppError> {
    // Rely on the DB unique index on `code` for uniqueness — avoids a TOCTOU
    // race between a SELECT check and the INSERT (same pattern as
    // `products::service::create`).
    let coupon = repository::create(db, &req.code, req.discount_cents, req.expires_at)
        .await
        .map_err(|e| AppError::conflict_on_unique(e, "coupon code already exists"))?;

    Ok(CouponResponse::from(coupon))
}

pub async fn list_coupons(
    db: &PgPool,
    pagination: &PaginationParams,
) -> Result<CouponListResponse, AppError> {
    let total = repository::count_all(db).await?;
    let coupons = repository::find_all(db, pagination.limit(), pagination.offset()).await?;

    Ok(CouponListResponse {
        coupons: coupons.into_iter().map(CouponResponse::from).collect(),
        meta: pagination.meta(total),
    })
}

/// `PATCH /coupons/{id}` — admin only (checked by the handler). `code` is
/// immutable and not part of `UpdateCouponRequest`.
pub async fn update_coupon(
    db: &PgPool,
    id: Uuid,
    req: UpdateCouponRequest,
) -> Result<CouponResponse, AppError> {
    let coupon = repository::update(db, id, req.discount_cents, req.is_active, req.expires_at)
        .await?
        .ok_or_else(|| AppError::NotFound("coupon not found".into()))?;

    Ok(CouponResponse::from(coupon))
}

/// `DELETE /coupons/{id}` — admin only (checked by the handler). Hard
/// delete; see `repository::delete`'s doc comment for why this is safe
/// (orders keep a `code` string snapshot, no FK to this table).
pub async fn delete_coupon(db: &PgPool, id: Uuid) -> Result<(), AppError> {
    let deleted = repository::delete(db, id).await?;
    if !deleted {
        return Err(AppError::NotFound("coupon not found".into()));
    }
    Ok(())
}
