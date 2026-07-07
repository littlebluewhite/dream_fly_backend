use sqlx::PgPool;

use crate::error::AppError;
use crate::extractors::pagination::PaginationParams;

use super::dto::{CouponListResponse, CouponResponse, CouponValidateResponse, CreateCouponRequest};
use super::repository;

pub async fn validate_coupon(db: &PgPool, code: &str) -> Result<CouponValidateResponse, AppError> {
    let coupon = repository::find_valid_by_code(db, code)
        .await?
        .ok_or_else(|| AppError::NotFound("coupon not found".into()))?;

    Ok(CouponValidateResponse {
        code: coupon.code,
        discount_cents: coupon.discount_cents,
    })
}

pub async fn create_coupon(
    db: &PgPool,
    req: CreateCouponRequest,
) -> Result<CouponResponse, AppError> {
    // Rely on the DB unique index on `code` for uniqueness — avoids a TOCTOU
    // race between a SELECT check and the INSERT (same pattern as
    // `products::service::create`).
    let coupon = match repository::create(db, &req.code, req.discount_cents, req.expires_at).await
    {
        Ok(c) => c,
        Err(sqlx::Error::Database(ref db_err)) if db_err.is_unique_violation() => {
            return Err(AppError::Conflict("coupon code already exists".into()));
        }
        Err(e) => return Err(AppError::Database(e)),
    };

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
