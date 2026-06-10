//! HTTP integration tests for `/posts/*` endpoints.

mod common;

use common::fixtures::seed_post;
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn list_posts_public_only_published(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("author@example.com", "Password!234").await;
    seed_post(&app.db, user.user_id, "Published", true).await;
    seed_post(&app.db, user.user_id, "Draft", false).await;

    let resp = app.get("/api/v1/posts").await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    let posts = body["posts"].as_array().unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["status"], "published");
}

#[sqlx::test]
async fn get_post_by_slug_returns_detail(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("author2@example.com", "Password!234").await;
    let post_id = seed_post(&app.db, user.user_id, "Hello", true).await;

    // Fetch by UUID
    let resp = app.get(&format!("/api/v1/posts/{post_id}")).await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    assert_eq!(body["title"], "Hello");
    // Detail view should contain `content` (list view doesn't).
    assert!(body.get("content").is_some());
}

#[sqlx::test]
async fn get_post_unknown_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .get(&format!("/api/v1/posts/{}", Uuid::now_v7()))
        .await;
    assert_eq!(resp.status_code(), 404);
}

#[sqlx::test]
async fn create_post_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/posts")
        .json(&json!({
            "title": "New",
            "content": "Hello",
            "category": "article",
        }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn create_post_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("p-mem@example.com", "Password!234").await;

    // Member is neither admin nor coach → 403
    let resp = app
        .post("/api/v1/posts")
        .authorization_bearer(&user.access_token)
        .json(&json!({
            "title": "New",
            "content": "Hello",
            "category": "article",
        }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn create_post_as_admin_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/posts")
        .authorization_bearer(&token)
        .json(&json!({
            "title": "Announcement",
            "content": "Body",
            "category": "announcement",
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    assert_eq!(resp.json::<serde_json::Value>()["title"], "Announcement");
}

#[sqlx::test]
async fn delete_post_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let author = app.register_member("auth3@example.com", "Password!234").await;
    let post_id = seed_post(&app.db, author.user_id, "Hi", true).await;

    let member = app.register_member("p-mem2@example.com", "Password!234").await;
    let resp = app
        .delete(&format!("/api/v1/posts/{post_id}"))
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn delete_post_as_admin_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let author = app.register_member("auth4@example.com", "Password!234").await;
    let post_id = seed_post(&app.db, author.user_id, "Goodbye", true).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .delete(&format!("/api/v1/posts/{post_id}"))
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 204);
}
