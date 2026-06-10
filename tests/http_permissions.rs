//! HTTP integration tests for `/roles/*` (RBAC) endpoints.

mod common;

use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn list_roles_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/roles").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn list_roles_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("rmem@example.com", "Password!234").await;
    let resp = app
        .get("/api/v1/roles")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn list_roles_as_admin_returns_seeded_roles(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/roles")
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    // Migration 00002 seeds admin, coach, member, guest — expect all 4.
    let body: serde_json::Value = resp.json();
    let names: Vec<String> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    for expected in ["admin", "coach", "member", "guest"] {
        assert!(names.contains(&expected.to_string()), "missing {expected}");
    }
}

#[sqlx::test]
async fn create_role_as_admin_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/roles")
        .authorization_bearer(&token)
        .json(&json!({
            "name": "marketing",
            "description": "Marketing team",
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    assert_eq!(resp.json::<serde_json::Value>()["name"], "marketing");
}

#[sqlx::test]
async fn create_role_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/roles")
        .json(&json!({ "name": "x" }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn assign_and_remove_role_round_trip(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let target = app.register_member("target@example.com", "Password!234").await;

    // Look up the `coach` role id from the seeded list.
    let roles: serde_json::Value = app
        .get("/api/v1/roles")
        .authorization_bearer(&token)
        .await
        .json();
    let coach_role_id = roles
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "coach")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Assign
    let assign = app
        .post(&format!("/api/v1/roles/{coach_role_id}/users"))
        .authorization_bearer(&token)
        .json(&json!({ "user_id": target.user_id }))
        .await;
    assert_eq!(assign.status_code(), 200, "body={}", assign.text());

    // Remove
    let remove = app
        .delete(&format!(
            "/api/v1/roles/{coach_role_id}/users/{}",
            target.user_id
        ))
        .authorization_bearer(&token)
        .await;
    assert_eq!(remove.status_code(), 204);
}

#[sqlx::test]
async fn create_role_as_member_returns_403(db: PgPool) {
    // Defense-in-depth: any admin-only handler should reject members with
    // 403 even when the input payload is well-formed, so a forgotten
    // `require_role` call in a future refactor cannot silently open the
    // endpoint up.
    let app = spawn_test_app(db).await;
    let user = app.register_member("m-create@example.com", "Password!234").await;
    let resp = app
        .post("/api/v1/roles")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "name": "unauthorized-role" }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn remove_role_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("m-remove@example.com", "Password!234").await;
    let target = app.register_member("t-remove@example.com", "Password!234").await;

    // Any role id is fine — the member guard fires before the DB is hit.
    let fake_role = uuid::Uuid::now_v7();
    let resp = app
        .delete(&format!(
            "/api/v1/roles/{fake_role}/users/{}",
            target.user_id
        ))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn assign_role_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("plain@example.com", "Password!234").await;
    let other = app.register_member("other@example.com", "Password!234").await;

    let roles: serde_json::Value = {
        let (_admin, token) = app.seed_admin().await;
        app.get("/api/v1/roles")
            .authorization_bearer(&token)
            .await
            .json()
    };
    let coach_role_id = roles
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "coach")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = app
        .post(&format!("/api/v1/roles/{coach_role_id}/users"))
        .authorization_bearer(&user.access_token)
        .json(&json!({ "user_id": other.user_id }))
        .await;
    assert_eq!(resp.status_code(), 403);
}
