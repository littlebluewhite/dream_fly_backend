use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{
    CouponListResponse, CouponResponse, CouponValidateResponse, CreateCouponRequest,
    UpdateCouponRequest, ValidateCouponQuery,
};
use super::service;

/// Validate a coupon code (any authenticated user, no role check). With
/// `?subtotal_cents=`, the response also previews `applied_discount_cents`
/// (see `CouponValidateResponse`'s doc comment); omitting it leaves the
/// response unchanged from before this parameter existed.
#[tracing::instrument(skip_all)]
pub async fn validate(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(code): Path<String>,
    Query(params): Query<ValidateCouponQuery>,
) -> Result<Json<CouponValidateResponse>, AppError> {
    let result = service::validate_coupon(&state.db, &code, params.subtotal_cents).await?;
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

/// Update a coupon (admin only). `code` is immutable — not part of the PATCH
/// body; see `UpdateCouponRequest`.
#[tracing::instrument(skip_all)]
pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<UpdateCouponRequest>,
) -> Result<Json<CouponResponse>, AppError> {
    auth.require_role("admin")?;
    let coupon = service::update_coupon(&state.db, id, req).await?;
    Ok(Json(coupon))
}

/// Delete a coupon (admin only). Hard delete — safe because orders store
/// only a `code` string snapshot with no FK to this table (see
/// `coupons::repository::delete`'s doc comment).
#[tracing::instrument(skip_all)]
pub async fn delete(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    auth.require_role("admin")?;
    service::delete_coupon(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
