use sqlx::PgPool;
use uuid::Uuid;

use super::model::{Venue, VenueCategory};

pub async fn find_all_active(db: &PgPool) -> Result<Vec<Venue>, sqlx::Error> {
    sqlx::query_as::<_, Venue>(
        "SELECT id, category_id, name, slug, description, features, image_url, \
         is_active, created_at, updated_at \
         FROM venues \
         WHERE is_active = true \
         ORDER BY name",
    )
    .fetch_all(db)
    .await
}

pub async fn find_by_slug(db: &PgPool, slug: &str) -> Result<Option<Venue>, sqlx::Error> {
    sqlx::query_as::<_, Venue>(
        "SELECT id, category_id, name, slug, description, features, image_url, \
         is_active, created_at, updated_at \
         FROM venues WHERE LOWER(slug) = LOWER($1)",
    )
    .bind(slug)
    .fetch_optional(db)
    .await
}

pub async fn find_by_id(db: &PgPool, id: Uuid) -> Result<Option<Venue>, sqlx::Error> {
    sqlx::query_as::<_, Venue>(
        "SELECT id, category_id, name, slug, description, features, image_url, \
         is_active, created_at, updated_at \
         FROM venues WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

pub async fn create_venue(
    db: &PgPool,
    name: &str,
    slug: &str,
    category_id: Option<Uuid>,
    description: Option<&str>,
    features: &[String],
    image_url: Option<&str>,
) -> Result<Venue, sqlx::Error> {
    sqlx::query_as::<_, Venue>(
        "INSERT INTO venues (id, category_id, name, slug, description, features, image_url, is_active, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, true, NOW(), NOW()) RETURNING *",
    )
    .bind(category_id)
    .bind(name)
    .bind(slug)
    .bind(description)
    .bind(features)
    .bind(image_url)
    .fetch_one(db)
    .await
}

/// Partial (PATCH-style) update — every argument optional; `category_id`/
/// `description`/`image_url` are `Option<Option<T>>` so callers can
/// distinguish "don't touch" (`None`) from "set to NULL" (`Some(None)`)
/// from "set to value" (`Some(Some(v))`). Template: `courses::repository::update`.
/// Returns `Ok(None)` if `id` doesn't match any row (caller maps to 404).
#[allow(clippy::too_many_arguments)]
pub async fn update(
    db: &PgPool,
    id: Uuid,
    name: Option<&str>,
    slug: Option<&str>,
    category_id: Option<Option<Uuid>>,
    description: Option<Option<&str>>,
    features: Option<&[String]>,
    image_url: Option<Option<&str>>,
    is_active: Option<bool>,
) -> Result<Option<Venue>, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new("UPDATE venues SET updated_at = now()");

    if let Some(v) = name {
        qb.push(", name = ").push_bind(v);
    }
    if let Some(v) = slug {
        qb.push(", slug = ").push_bind(v);
    }
    if let Some(v) = category_id {
        qb.push(", category_id = ").push_bind(v);
    }
    if let Some(v) = description {
        qb.push(", description = ").push_bind(v);
    }
    if let Some(v) = features {
        qb.push(", features = ").push_bind(v);
    }
    if let Some(v) = image_url {
        qb.push(", image_url = ").push_bind(v);
    }
    if let Some(v) = is_active {
        qb.push(", is_active = ").push_bind(v);
    }

    qb.push(" WHERE id = ").push_bind(id);
    qb.push(" RETURNING *");

    qb.build_query_as::<Venue>().fetch_optional(db).await
}

pub async fn find_all_categories(db: &PgPool) -> Result<Vec<VenueCategory>, sqlx::Error> {
    sqlx::query_as::<_, VenueCategory>(
        "SELECT id, name, slug, icon, display_order, created_at \
         FROM venue_categories \
         ORDER BY display_order, name",
    )
    .fetch_all(db)
    .await
}

pub async fn find_category_by_id(
    db: &PgPool,
    id: Uuid,
) -> Result<Option<VenueCategory>, sqlx::Error> {
    sqlx::query_as::<_, VenueCategory>(
        "SELECT id, name, slug, icon, display_order, created_at \
         FROM venue_categories WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}
