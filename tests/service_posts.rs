//! Integration tests for `posts::service`.
//!
//! Covered paths:
//! - `create_post` rejects unknown category with Validation
//! - `create_post` auto-generates slug from title
//! - `create_post` Conflict on duplicate slug
//! - `update_post` by non-author non-admin → Forbidden
//! - `update_post` by author → succeeds
//! - `update_post` by admin (not author) → succeeds
//! - `update_post` transitioning draft→published sets published_at
//! - `delete_post` NotFound for random id
//! - `list_published` returns only published posts, paginated

mod common;

use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::extractors::auth::AuthUser;
use dream_fly_backend::extractors::pagination::PaginationParams;
use dream_fly_backend::modules::posts::dto::{CreatePostRequest, UpdatePostRequest};
use dream_fly_backend::modules::posts::service;

fn auth_for(user_id: Uuid, roles: &[&str]) -> AuthUser {
    AuthUser {
        user_id,
        email: format!("{user_id}@example.com"),
        roles: roles.iter().map(|r| (*r).to_string()).collect(),
    }
}

fn create_req(title: &str, category: &str) -> CreatePostRequest {
    CreatePostRequest {
        title: title.into(),
        slug: None,
        content: "Some body content.".into(),
        excerpt: Some("Short excerpt".into()),
        category: category.into(),
        cover_image: None,
    }
}

async fn set_status_draft(db: &PgPool, post_id: Uuid) {
    sqlx::query(
        "UPDATE posts SET status = 'draft'::post_status, published_at = NULL WHERE id = $1",
    )
    .bind(post_id)
    .execute(db)
    .await
    .expect("reset to draft");
}

#[sqlx::test]
async fn create_post_invalid_category_returns_validation(db: PgPool) {
    let author = common::seed_member(&db, "a@example.com", "hunter22-secret").await;
    let err = service::create_post(&db, author, create_req("Hi", "nonsense"))
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Validation(_)));
}

#[sqlx::test]
async fn create_post_auto_generates_slug(db: PgPool) {
    let author = common::seed_member(&db, "a@example.com", "hunter22-secret").await;
    let post = service::create_post(&db, author, create_req("Hello World", "article"))
        .await
        .expect("create_post");
    assert_eq!(post.title, "Hello World");
    assert_eq!(post.slug, "hello-world");
    assert_eq!(post.author_id, author);
}

#[sqlx::test]
async fn create_post_duplicate_slug_returns_conflict(db: PgPool) {
    let author = common::seed_member(&db, "a@example.com", "hunter22-secret").await;
    service::create_post(
        &db,
        author,
        CreatePostRequest {
            slug: Some("shared".into()),
            ..create_req("First", "article")
        },
    )
    .await
    .unwrap();

    let err = service::create_post(
        &db,
        author,
        CreatePostRequest {
            slug: Some("shared".into()),
            ..create_req("Second", "article")
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)));
}

#[sqlx::test]
async fn update_post_by_non_author_non_admin_returns_forbidden(db: PgPool) {
    let author = common::seed_member(&db, "a@example.com", "hunter22-secret").await;
    let other = common::seed_member(&db, "b@example.com", "hunter22-secret").await;
    let post = service::create_post(&db, author, create_req("Mine", "article"))
        .await
        .unwrap();

    let err = service::update_post(
        &db,
        post.id,
        &auth_for(other, &["member"]),
        UpdatePostRequest {
            title: Some("Pwned".into()),
            slug: None,
            content: None,
            excerpt: None,
            category: None,
            status: None,
            cover_image: None,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)));
}

#[sqlx::test]
async fn update_post_by_author_succeeds(db: PgPool) {
    let author = common::seed_member(&db, "a@example.com", "hunter22-secret").await;
    let post = service::create_post(&db, author, create_req("Mine", "article"))
        .await
        .unwrap();

    let updated = service::update_post(
        &db,
        post.id,
        &auth_for(author, &["member"]),
        UpdatePostRequest {
            title: Some("Renamed".into()),
            slug: None,
            content: None,
            excerpt: None,
            category: None,
            status: None,
            cover_image: None,
        },
    )
    .await
    .expect("author may update");

    assert_eq!(updated.title, "Renamed");
}

#[sqlx::test]
async fn update_post_by_admin_on_other_author_succeeds(db: PgPool) {
    let author = common::seed_member(&db, "a@example.com", "hunter22-secret").await;
    let admin = common::seed_member(&db, "admin@example.com", "hunter22-secret").await;
    let post = service::create_post(&db, author, create_req("Mine", "article"))
        .await
        .unwrap();

    service::update_post(
        &db,
        post.id,
        &auth_for(admin, &["admin"]),
        UpdatePostRequest {
            title: Some("Admin Fixed".into()),
            slug: None,
            content: None,
            excerpt: None,
            category: None,
            status: None,
            cover_image: None,
        },
    )
    .await
    .expect("admin may update any post");
}

#[sqlx::test]
async fn update_post_draft_to_published_sets_published_at(db: PgPool) {
    let author = common::seed_member(&db, "a@example.com", "hunter22-secret").await;
    let post = service::create_post(&db, author, create_req("Draft", "article"))
        .await
        .unwrap();
    // `create_post` always produces `published` via repository default; force
    // back to draft so we can verify the published_at transition.
    set_status_draft(&db, post.id).await;

    let updated = service::update_post(
        &db,
        post.id,
        &auth_for(author, &["member"]),
        UpdatePostRequest {
            title: None,
            slug: None,
            content: None,
            excerpt: None,
            category: None,
            status: Some("published".into()),
            cover_image: None,
        },
    )
    .await
    .expect("publish transition");

    assert_eq!(updated.status, "published");
    assert!(
        updated.published_at.is_some(),
        "published_at should be set on first publish"
    );
}

#[sqlx::test]
async fn delete_post_nonexistent_returns_not_found(db: PgPool) {
    let err = service::delete_post(&db, Uuid::now_v7()).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn list_published_paginates_and_excludes_drafts(db: PgPool) {
    let author = common::seed_member(&db, "a@example.com", "hunter22-secret").await;

    // Seed 3 published posts via fixtures helper, plus 1 draft.
    for i in 0..3 {
        common::fixtures::seed_post(&db, author, &format!("Published {i}"), true).await;
    }
    common::fixtures::seed_post(&db, author, "Hidden Draft", false).await;

    let page = service::list_published(
        &db,
        &PaginationParams {
            page: 1,
            per_page: 10,
        },
    )
    .await
    .expect("list");

    assert_eq!(page.posts.len(), 3);
    assert_eq!(page.meta.total, 3);
    for p in &page.posts {
        assert_eq!(p.status, "published");
    }
}
