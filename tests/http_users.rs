//! HTTP integration tests for `/users/*` endpoints.

mod common;

use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn me_without_token_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/users/me").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn me_with_token_returns_own_profile(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("me@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/users/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["email"], "me@example.com");
    assert_eq!(body["id"].as_str().unwrap(), user.user_id.to_string());
    assert_eq!(body["roles"], json!(["member"]));
}

#[sqlx::test]
async fn me_as_admin_returns_admin_role(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/users/me")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let roles = body["roles"].as_array().expect("roles array");
    assert!(roles.contains(&json!("admin")));
}

#[sqlx::test]
async fn update_me_changes_name(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("update@example.com", "Password!234").await;

    let resp = app
        .patch("/api/v1/users/me")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "name": "Brand New Name" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["name"], "Brand New Name");
}

#[sqlx::test]
async fn update_me_rejects_short_name(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("val@example.com", "Password!234").await;

    let resp = app
        .patch("/api/v1/users/me")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "name": "X" }))
        .await;
    assert_eq!(resp.status_code(), 422);
}

#[sqlx::test]
async fn list_users_as_admin_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    // Seed a few users + an admin.
    app.register_member("u1@example.com", "Password!234").await;
    app.register_member("u2@example.com", "Password!234").await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/users")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["users"].as_array().unwrap().len() >= 3);
}

#[sqlx::test]
async fn list_users_as_admin_includes_roles_per_user(db: PgPool) {
    let app = spawn_test_app(db).await;
    let member = app.register_member("listroles@example.com", "Password!234").await;
    let (admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/users")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let users = body["users"].as_array().expect("users array");

    let member_entry = users
        .iter()
        .find(|u| u["id"] == member.user_id.to_string())
        .expect("member present in list");
    assert_eq!(member_entry["roles"], json!(["member"]));

    let admin_entry = users
        .iter()
        .find(|u| u["id"] == admin_id.to_string())
        .expect("admin present in list");
    let admin_roles = admin_entry["roles"].as_array().expect("roles array");
    assert!(admin_roles.contains(&json!("admin")));
}

#[sqlx::test]
async fn list_users_as_member_returns_forbidden(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("mem@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/users")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn get_user_by_id_as_admin_returns_profile(db: PgPool) {
    let app = spawn_test_app(db).await;
    let target = app.register_member("target@example.com", "Password!234").await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get(&format!("/api/v1/users/{}", target.user_id))
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    assert_eq!(body["email"], "target@example.com");
}

#[sqlx::test]
async fn get_user_by_id_as_member_returns_403(db: PgPool) {
    // `/users/:id` is admin-only — a plain member reading another user's
    // full profile by UUID must be rejected before the DB is consulted.
    let app = spawn_test_app(db).await;
    let target = app.register_member("victim@example.com", "Password!234").await;
    let caller = app.register_member("peeper@example.com", "Password!234").await;

    let resp = app
        .get(&format!("/api/v1/users/{}", target.user_id))
        .authorization_bearer(&caller.access_token)
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn get_user_nonexistent_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let ghost = Uuid::now_v7();
    let resp = app
        .get(&format!("/api/v1/users/{ghost}"))
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 404);
}
