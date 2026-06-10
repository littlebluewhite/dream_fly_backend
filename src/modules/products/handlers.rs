use axum::{
    Json,
    extract::{Path, Query, State},
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{
    CreateProductRequest, ProductListResponse, ProductQuery, ProductResponse, UpdateProductRequest,
};
use super::service;

#[tracing::instrument(skip_all)]
pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<ProductQuery>,
    Query(page): Query<PaginationParams>,
) -> Result<Json<ProductListResponse>, AppError> {
    let list = service::list(
        &state.db,
        params.product_type.as_deref(),
        page.page,
        page.per_page,
    )
    .await?;
    Ok(Json(list))
}

#[tracing::instrument(skip_all)]
pub async fn get_by_slug(
    State(state): State<AppState>,
    Path(slug_or_id): Path<String>,
) -> Result<Json<ProductResponse>, AppError> {
    // Try parsing as UUID first, fallback to slug lookup
    let product = if let Ok(id) = slug_or_id.parse::<Uuid>() {
        service::get_by_id(&state.db, id).await?
    } else {
        service::get_by_slug(&state.db, &slug_or_id).await?
    };
    Ok(Json(product))
}

#[tracing::instrument(skip_all)]
pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateProductRequest>,
) -> Result<Json<ProductResponse>, AppError> {
    auth.require_role("admin")?;
    let product = service::create(&state.db, req).await?;
    Ok(Json(product))
}

#[tracing::instrument(skip_all)]
pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    ValidatedJson(req): ValidatedJson<UpdateProductRequest>,
) -> Result<Json<ProductResponse>, AppError> {
    auth.require_role("admin")?;
    let id: Uuid = id
        .parse()
        .map_err(|_| AppError::BadRequest("invalid product id".into()))?;
    let product = service::update(&state.db, id, req).await?;
    Ok(Json(product))
}
