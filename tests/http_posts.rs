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
async fn create_post_as_coach_succeeds(db: PgPool) {
    // staff gate (admin-or-coach) parity — `create_post_as_admin_succeeds`
    // above already covers admin; this was the only moved staff-gate site
    // across the six Step 9 modules without a coach-passes-the-gate test.
    let app = spawn_test_app(db).await;
    let (_coach_user, token) = app.seed_user_with_roles("p-coach@example.com", &["coach"]).await;

    let resp = app
        .post("/api/v1/posts")
        .authorization_bearer(&token)
        .json(&json!({
            "title": "Coach Post",
            "content": "Body",
            "category": "article",
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    assert_eq!(resp.json::<serde_json::Value>()["title"], "Coach Post");
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

// ---------------------------------------------------------------------------
// BE#22 — PATCH `null` must clear nullable columns, not be silently ignored
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn update_post_clears_excerpt_and_cover_image_to_null(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let created: serde_json::Value = app
        .post("/api/v1/posts")
        .authorization_bearer(&token)
        .json(&json!({
            "title": "Clearable Post",
            "content": "Body content",
            "category": "article",
            "excerpt": "An excerpt",
            "cover_image": "https://example.com/cover.jpg",
        }))
        .await
        .json();
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["excerpt"], "An excerpt");
    assert_eq!(created["cover_image"], "https://example.com/cover.jpg");

    // Explicit null on both: must clear to NULL, not be silently ignored.
    let resp = app
        .patch(&format!("/api/v1/posts/{id}"))
        .authorization_bearer(&token)
        .json(&json!({ "excerpt": null, "cover_image": null }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["excerpt"].is_null());
    assert!(body["cover_image"].is_null());

    let row: (Option<String>, Option<String>) =
        sqlx::query_as("SELECT excerpt, cover_image FROM posts WHERE id = $1")
            .bind(Uuid::parse_str(id).unwrap())
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(row.0.is_none(), "excerpt must be NULL in the DB, not just absent from JSON");
    assert!(row.1.is_none(), "cover_image must be NULL in the DB, not just absent from JSON");

    // Field-absent PATCH afterward must not error and must leave the
    // now-NULL columns alone — proves "absent" stays distinct from "null".
    let resp2 = app
        .patch(&format!("/api/v1/posts/{id}"))
        .authorization_bearer(&token)
        .json(&json!({ "title": "Renamed After Clear" }))
        .await;
    assert_eq!(resp2.status_code(), 200, "body={}", resp2.text());
    let body2: serde_json::Value = resp2.json();
    assert_eq!(body2["title"], "Renamed After Clear");
    assert!(body2["excerpt"].is_null());
    assert!(body2["cover_image"].is_null());

    // Re-populate both, then PATCH with the fields absent — populated
    // values must survive an absent-key update (guards the "absent
    // accidentally clears" regression class in the deserialize_some wiring).
    let resp3 = app
        .patch(&format!("/api/v1/posts/{id}"))
        .authorization_bearer(&token)
        .json(&json!({
            "excerpt": "Refilled excerpt",
            "cover_image": "https://example.com/refilled-cover.jpg",
        }))
        .await;
    assert_eq!(resp3.status_code(), 200, "body={}", resp3.text());

    let resp4 = app
        .patch(&format!("/api/v1/posts/{id}"))
        .authorization_bearer(&token)
        .json(&json!({ "title": "Renamed With Values Intact" }))
        .await;
    assert_eq!(resp4.status_code(), 200, "body={}", resp4.text());
    let body4: serde_json::Value = resp4.json();
    assert_eq!(body4["excerpt"], "Refilled excerpt");
    assert_eq!(body4["cover_image"], "https://example.com/refilled-cover.jpg");
}
