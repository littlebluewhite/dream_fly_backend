use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::utils::slug::slugify;

use super::dto::{CreateVenueRequest, UpdateVenueRequest, VenueResponse};
use super::repository;

fn venue_to_response(v: super::model::Venue) -> VenueResponse {
    VenueResponse {
        id: v.id,
        category_id: v.category_id,
        name: v.name,
        slug: v.slug,
        description: v.description,
        features: v.features,
        image_url: v.image_url,
        is_active: v.is_active,
        created_at: v.created_at,
    }
}

pub async fn list_active(db: &PgPool) -> Result<Vec<VenueResponse>, AppError> {
    let venues = repository::find_all_active(db).await?;
    Ok(venues.into_iter().map(venue_to_response).collect())
}

pub async fn get_by_slug(db: &PgPool, slug: &str) -> Result<VenueResponse, AppError> {
    let venue = repository::find_by_slug(db, slug)
        .await?
        .ok_or_else(|| AppError::NotFound("venue not found".into()))?;
    Ok(venue_to_response(venue))
}

pub async fn create_venue(
    db: &PgPool,
    req: &CreateVenueRequest,
) -> Result<VenueResponse, AppError> {
    let slug = req.slug.clone().unwrap_or_else(|| slugify(&req.name));

    let venue = repository::create_venue(
        db,
        &req.name,
        &slug,
        req.category_id,
        req.description.as_deref(),
        &req.features,
        req.image_url.as_deref(),
    )
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e {
            // Migration defines a functional lowercase unique index
            // named `uq_venues_slug_lower` rather than the default
            // `venues_slug_key`, so match that constraint explicitly.
            if db_err.constraint() == Some("uq_venues_slug_lower") {
                return AppError::Conflict(format!("venue slug '{}' already exists", slug));
            }
        }
        AppError::Database(e)
    })?;

    Ok(venue_to_response(venue))
}

/// `PATCH /venues/{id}` — admin only (checked by the handler). Slug
/// uniqueness is enforced by the DB's `uq_venues_slug_lower` functional
/// index; a violation surfaces as `sqlx::Error::Database` here and is
/// translated to 409 — same idiom as `create_venue` above (see its comment
/// for why the constraint name is matched explicitly).
pub async fn update_venue(
    db: &PgPool,
    id: Uuid,
    req: &UpdateVenueRequest,
) -> Result<VenueResponse, AppError> {
    let venue = repository::update(
        db,
        id,
        req.name.as_deref(),
        req.slug.as_deref(),
        req.category_id,
        req.description.as_ref().map(|o| o.as_deref()),
        req.features.as_deref(),
        req.image_url.as_ref().map(|o| o.as_deref()),
        req.is_active,
    )
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e {
            if db_err.constraint() == Some("uq_venues_slug_lower") {
                let slug = req.slug.as_deref().unwrap_or_default();
                return AppError::Conflict(format!("venue slug '{}' already exists", slug));
            }
        }
        AppError::Database(e)
    })?
    .ok_or_else(|| AppError::NotFound("venue not found".into()))?;

    Ok(venue_to_response(venue))
}
