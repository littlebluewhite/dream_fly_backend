use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::utils::slug::slugify;

use super::dto::{
    CreatePostRequest, PostDetailResponse, PostListResponse, PostResponse, UpdatePostRequest,
};
use super::model::{PostCategory, PostStatus};
use super::repository;

pub async fn list_published(
    db: &PgPool,
    pagination: &PaginationParams,
) -> Result<PostListResponse, AppError> {
    let total = repository::count_published(db).await?;
    let posts = repository::find_published(db, pagination.limit(), pagination.offset()).await?;
    Ok(PostListResponse {
        posts: posts.into_iter().map(PostResponse::from).collect(),
        meta: pagination.meta(total),
    })
}

pub async fn get_by_slug_or_id(db: &PgPool, param: &str) -> Result<PostDetailResponse, AppError> {
    let post = if let Ok(id) = param.parse::<Uuid>() {
        repository::find_by_id(db, id).await?
    } else {
        repository::find_by_slug(db, param).await?
    };

    post.map(PostDetailResponse::from)
        .ok_or_else(|| AppError::NotFound("post not found".into()))
}

/// Public-facing lookup — only returns published posts.
pub async fn get_published_by_slug_or_id(
    db: &PgPool,
    param: &str,
) -> Result<PostDetailResponse, AppError> {
    let post = if let Ok(id) = param.parse::<Uuid>() {
        repository::find_published_by_id(db, id).await?
    } else {
        repository::find_published_by_slug(db, param).await?
    };

    post.map(PostDetailResponse::from)
        .ok_or_else(|| AppError::NotFound("post not found".into()))
}

pub async fn create_post(
    db: &PgPool,
    author_id: Uuid,
    req: CreatePostRequest,
) -> Result<PostDetailResponse, AppError> {
    // Validate category
    let _: PostCategory = req.category.parse().map_err(|_| {
        AppError::Validation(
            "invalid category, must be one of: announcement, article, promotion, event".into(),
        )
    })?;

    let slug = req.slug.unwrap_or_else(|| slugify(&req.title));

    // Rely on the DB unique index for slug uniqueness — avoids TOCTOU race
    // between a SELECT check and the INSERT.
    let post = repository::create(
        db,
        author_id,
        &req.title,
        &slug,
        &req.content,
        req.excerpt.as_deref(),
        &req.category.to_lowercase(),
        req.cover_image.as_deref(),
    )
    .await
    .map_err(|e| AppError::conflict_on_unique(e, "post slug already exists"))?;

    Ok(PostDetailResponse::from(post))
}

/// `PATCH /posts/{id}` — ownership checked via `auth.owns_or_admin` below.
/// Slug uniqueness is enforced by the DB's `uq_posts_slug_lower` functional
/// index rather than a SELECT-then-check precheck: the old precheck read
/// `find_by_slug` and only wrote afterward, leaving a window where two
/// concurrent requests could both pass the check and then both write — a
/// TOCTOU race. Relying on the constraint instead makes the DB the single
/// source of truth, so a collision surfaces here as `sqlx::Error::Database`
/// and is translated to 409 — same idiom as `create_post` above (see its
/// comment for why this avoids the same race on the INSERT path).
pub async fn update_post(
    db: &PgPool,
    id: Uuid,
    auth: &AuthUser,
    req: UpdatePostRequest,
) -> Result<PostDetailResponse, AppError> {
    // Verify the post exists and check ownership
    let existing = repository::find_by_id(db, id)
        .await?
        .ok_or_else(|| AppError::NotFound("post not found".into()))?;

    auth.owns_or_admin(existing.author_id, "you can only update your own posts")?;

    // Validate category if provided
    if let Some(ref category) = req.category {
        let _: PostCategory = category.parse().map_err(|_| {
            AppError::Validation(
                "invalid category, must be one of: announcement, article, promotion, event".into(),
            )
        })?;
    }

    // Validate status if provided
    let status_str = if let Some(ref status) = req.status {
        let _: PostStatus = status.parse().map_err(|_| {
            AppError::Validation(
                "invalid status, must be one of: draft, published, archived".into(),
            )
        })?;
        Some(status.to_lowercase())
    } else {
        None
    };

    // If transitioning to published and currently not published, set published_at
    let published_at: Option<Option<chrono::DateTime<chrono::Utc>>> =
        if status_str.as_deref() == Some("published") && existing.published_at.is_none() {
            Some(Some(chrono::Utc::now()))
        } else {
            None // don't touch published_at
        };

    let post = repository::update(
        db,
        id,
        req.title.as_deref(),
        req.slug.as_deref(),
        req.content.as_deref(),
        req.excerpt.as_ref().map(|o| o.as_deref()),
        req.category.as_deref(),
        status_str.as_deref(),
        req.cover_image.as_ref().map(|o| o.as_deref()),
        published_at,
    )
    .await
    .map_err(|e| AppError::conflict_on_constraint(e, "uq_posts_slug_lower", "post slug already exists"))?;

    post.map(PostDetailResponse::from)
        .ok_or_else(|| AppError::NotFound("post not found".into()))
}

pub async fn delete_post(db: &PgPool, id: Uuid) -> Result<(), AppError> {
    let deleted = repository::delete(db, id).await?;
    if !deleted {
        return Err(AppError::NotFound("post not found".into()));
    }
    Ok(())
}
