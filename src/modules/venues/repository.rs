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
