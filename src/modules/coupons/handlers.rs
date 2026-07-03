use axum::{
    Json,
    extract::{Path, Query, State},
};

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{CouponListResponse, CouponResponse, CouponValidateResponse, CreateCouponRequest};
use super::service;

/// Validate a coupon code (any authenticated user, no role check).
#[tracing::instrument(skip_all)]
pub async fn validate(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(code): Path<String>,
) -> Result<Json<CouponValidateResponse>, AppError> {
    let result = service::validate_coupon(&state.db, &code).await?;
    Ok(Json(result))
}

/// Create a coupon (admin only).
#[tracing::instrument(skip_all)]
pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateCouponRequest>,
) -> Result<Json<CouponResponse>, AppError> {
    auth.require_role("admin")?;
    let coupon = service::create_coupon(&state.db, req).await?;
    Ok(Json(coupon))
}

/// List coupons, paginated (admin only).
#[tracing::instrument(skip_all)]
pub async fn list(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<PaginationParams>,
) -> Result<Json<CouponListResponse>, AppError> {
    auth.require_role("admin")?;
    let result = service::list_coupons(&state.db, &params).await?;
    Ok(Json(result))
}
