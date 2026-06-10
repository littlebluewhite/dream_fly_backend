use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::model::Post;

pub async fn find_published(
    db: &PgPool,
    limit: u32,
    offset: u32,
) -> Result<Vec<Post>, sqlx::Error> {
    sqlx::query_as::<_, Post>(
        "SELECT id, author_id, title, slug, content, excerpt, category, status, \
         cover_image, published_at, created_at, updated_at \
         FROM posts WHERE status = 'published' \
         ORDER BY published_at DESC NULLS LAST \
         LIMIT $1 OFFSET $2",
    )
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(db)
    .await
}

pub async fn count_published(db: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM posts WHERE status = 'published'")
        .fetch_one(db)
        .await
}

pub async fn find_by_slug(db: &PgPool, slug: &str) -> Result<Option<Post>, sqlx::Error> {
    sqlx::query_as::<_, Post>(
        "SELECT id, author_id, title, slug, content, excerpt, category, status, \
         cover_image, published_at, created_at, updated_at \
         FROM posts WHERE LOWER(slug) = LOWER($1)",
    )
    .bind(slug)
    .fetch_optional(db)
    .await
}

pub async fn find_by_id(db: &PgPool, id: Uuid) -> Result<Option<Post>, sqlx::Error> {
    sqlx::query_as::<_, Post>(
        "SELECT id, author_id, title, slug, content, excerpt, category, status, \
         cover_image, published_at, created_at, updated_at \
         FROM posts WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

pub async fn find_published_by_slug(
    db: &PgPool,
    slug: &str,
) -> Result<Option<Post>, sqlx::Error> {
    sqlx::query_as::<_, Post>(
        "SELECT id, author_id, title, slug, content, excerpt, category, status, \
         cover_image, published_at, created_at, updated_at \
         FROM posts WHERE LOWER(slug) = LOWER($1) AND status = 'published'",
    )
    .bind(slug)
    .fetch_optional(db)
    .await
}

pub async fn find_published_by_id(
    db: &PgPool,
    id: Uuid,
) -> Result<Option<Post>, sqlx::Error> {
    sqlx::query_as::<_, Post>(
        "SELECT id, author_id, title, slug, content, excerpt, category, status, \
         cover_image, published_at, created_at, updated_at \
         FROM posts WHERE id = $1 AND status = 'published'",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn create(
    db: &PgPool,
    author_id: Uuid,
    title: &str,
    slug: &str,
    content: &str,
    excerpt: Option<&str>,
    category: &str,
    cover_image: Option<&str>,
) -> Result<Post, sqlx::Error> {
    sqlx::query_as::<_, Post>(
        "INSERT INTO posts (id, author_id, title, slug, content, excerpt, category, \
         cover_image, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6::post_category, $7, now(), now()) \
         RETURNING id, author_id, title, slug, content, excerpt, category, status, \
         cover_image, published_at, created_at, updated_at",
    )
    .bind(author_id)
    .bind(title)
    .bind(slug)
    .bind(content)
    .bind(excerpt)
    .bind(category)
    .bind(cover_image)
    .fetch_one(db)
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn update(
    db: &PgPool,
    id: Uuid,
    title: Option<&str>,
    slug: Option<&str>,
    content: Option<&str>,
    excerpt: Option<Option<&str>>,
    category: Option<&str>,
    status: Option<&str>,
    cover_image: Option<Option<&str>>,
    published_at: Option<Option<DateTime<Utc>>>,
) -> Result<Option<Post>, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new("UPDATE posts SET updated_at = now()");

    if let Some(v) = title {
        qb.push(", title = ").push_bind(v);
    }
    if let Some(v) = slug {
        qb.push(", slug = ").push_bind(v);
    }
    if let Some(v) = content {
        qb.push(", content = ").push_bind(v);
    }
    if let Some(v) = excerpt {
        qb.push(", excerpt = ").push_bind(v);
    }
    if let Some(v) = category {
        qb.push(", category = ").push_bind(v).push("::post_category");
    }
    if let Some(v) = status {
        qb.push(", status = ").push_bind(v).push("::post_status");
    }
    if let Some(v) = cover_image {
        qb.push(", cover_image = ").push_bind(v);
    }
    if let Some(v) = published_at {
        qb.push(", published_at = ").push_bind(v);
    }

    qb.push(" WHERE id = ").push_bind(id);
    qb.push(
        " RETURNING id, author_id, title, slug, content, excerpt, category, status, \
          cover_image, published_at, created_at, updated_at",
    );

    qb.build_query_as::<Post>().fetch_optional(db).await
}

pub async fn delete(db: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM posts WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(result.rows_affected() > 0)
}
