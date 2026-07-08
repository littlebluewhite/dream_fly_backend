use axum::{
    Json,
    extract::{Path, State},
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{CreateVenueRequest, UpdateVenueRequest, VenueResponse};
use super::service;

#[tracing::instrument(skip_all)]
pub async fn list(
    State(state): State<AppState>,
) -> Result<Json<Vec<VenueResponse>>, AppError> {
    let venues = service::list_active(&state.db).await?;
    Ok(Json(venues))
}

#[tracing::instrument(skip_all)]
pub async fn get_by_slug(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<VenueResponse>, AppError> {
    let venue = service::get_by_slug(&state.db, &slug).await?;
    Ok(Json(venue))
}

#[tracing::instrument(skip_all)]
pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateVenueRequest>,
) -> Result<Json<VenueResponse>, AppError> {
    auth.require_role("admin")?;
    let venue = service::create_venue(&state.db, &req).await?;
    Ok(Json(venue))
}

#[tracing::instrument(skip_all)]
pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<UpdateVenueRequest>,
) -> Result<Json<VenueResponse>, AppError> {
    auth.require_role("admin")?;
    let venue = service::update_venue(&state.db, id, &req).await?;
    Ok(Json(venue))
}
