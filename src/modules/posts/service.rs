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

/// The `invalid category` 422 message — built from `PostCategory::ALL`
/// (ADR-0005) so the allowed-values list can't drift out of sync with the
/// enum; `create_post`/`update_post` share this one owner.
fn invalid_category_message() -> String {
    format!(
        "invalid category, must be one of: {}",
        PostCategory::ALL.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
    )
}

/// The `invalid status` 422 message — built from `PostStatus::ALL`
/// (ADR-0005), same rationale as [`invalid_category_message`].
fn invalid_status_message() -> String {
    format!(
        "invalid status, must be one of: {}",
        PostStatus::ALL.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
    )
}

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
    let category: PostCategory = req
        .category
        .parse()
        .map_err(|_| AppError::Validation(invalid_category_message()))?;

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
        category,
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

    // Validate category if provided — parsed once into `PostCategory` so
    // the repository writes the lowercased value instead of the caller's
    // raw (possibly mixed-case) string.
    let category: Option<PostCategory> = req
        .category
        .as_deref()
        .map(|s| s.parse::<PostCategory>())
        .transpose()
        .map_err(|_| AppError::Validation(invalid_category_message()))?;

    // Validate status if provided — parsed once into `PostStatus` so the
    // published_at decision below matches on the enum instead of re-parsing
    // the string a second time.
    let new_status: Option<PostStatus> = req
        .status
        .as_deref()
        .map(|s| s.parse::<PostStatus>())
        .transpose()
        .map_err(|_| AppError::Validation(invalid_status_message()))?;

    // If transitioning to published and currently not published, set published_at
    let published_at: Option<Option<chrono::DateTime<chrono::Utc>>> =
        if should_stamp_published_at(new_status.as_ref(), existing.published_at) {
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
        category,
        new_status,
        req.cover_image.as_ref().map(|o| o.as_deref()),
        published_at,
    )
    .await
    .map_err(|e| {
        AppError::conflict_on_constraint(e, "uq_posts_slug_lower", "post slug already exists")
    })?;

    post.map(PostDetailResponse::from)
        .ok_or_else(|| AppError::NotFound("post not found".into()))
}

/// Whether a `PATCH /posts/{id}` status update should stamp `published_at`
/// with the current time — true only the first time a post transitions into
/// `published`; a post that was already published before, or isn't
/// transitioning to `published` at all, leaves `published_at` untouched.
fn should_stamp_published_at(
    new_status: Option<&PostStatus>,
    existing_published_at: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    matches!(new_status, Some(PostStatus::Published)) && existing_published_at.is_none()
}

pub async fn delete_post(db: &PgPool, id: Uuid) -> Result<(), AppError> {
    let deleted = repository::delete(db, id).await?;
    if !deleted {
        return Err(AppError::NotFound("post not found".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_stamp_published_at_true_on_first_publish() {
        assert!(should_stamp_published_at(Some(&PostStatus::Published), None));
    }

    #[test]
    fn should_stamp_published_at_false_when_already_published_before() {
        assert!(!should_stamp_published_at(
            Some(&PostStatus::Published),
            Some(chrono::Utc::now())
        ));
    }

    #[test]
    fn should_stamp_published_at_false_when_not_transitioning_to_published() {
        assert!(!should_stamp_published_at(Some(&PostStatus::Draft), None));
    }
}
