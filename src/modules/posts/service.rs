use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
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
    let post = match repository::create(
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
    {
        Ok(p) => p,
        Err(sqlx::Error::Database(ref db_err)) if db_err.is_unique_violation() => {
            return Err(AppError::Conflict("post slug already exists".into()));
        }
        Err(e) => return Err(AppError::Database(e)),
    };

    Ok(PostDetailResponse::from(post))
}

pub async fn update_post(
    db: &PgPool,
    id: Uuid,
    auth_user_id: Uuid,
    is_admin: bool,
    req: UpdatePostRequest,
) -> Result<PostDetailResponse, AppError> {
    // Verify the post exists and check ownership
    let existing = repository::find_by_id(db, id)
        .await?
        .ok_or_else(|| AppError::NotFound("post not found".into()))?;

    if existing.author_id != auth_user_id && !is_admin {
        return Err(AppError::Forbidden(
            "you can only update your own posts".into(),
        ));
    }

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

    // Check slug uniqueness if changing
    if let Some(ref new_slug) = req.slug {
        if let Some(existing_post) = repository::find_by_slug(db, new_slug).await? {
            if existing_post.id != id {
                return Err(AppError::Conflict("post slug already exists".into()));
            }
        }
    }

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
    .await?;

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
