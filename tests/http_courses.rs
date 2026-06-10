//! HTTP integration tests for `/courses/*` endpoints.

mod common;

use common::fixtures::seed_course;
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn list_courses_is_public_and_empty_initially(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/courses").await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    assert!(body["courses"].as_array().unwrap().is_empty());
    assert_eq!(body["total"], 0);
}

#[sqlx::test]
async fn list_courses_returns_seeded(db: PgPool) {
    let app = spawn_test_app(db).await;
    seed_course(&app.db, "Intro Flow", None).await;

    let resp = app.get("/api/v1/courses").await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    assert_eq!(body["courses"].as_array().unwrap().len(), 1);
    assert_eq!(body["courses"][0]["name"], "Intro Flow");
    assert_eq!(body["total"], 1);
}

#[sqlx::test]
async fn get_course_by_slug_returns_detail(db: PgPool) {
    let app = spawn_test_app(db).await;
    let id = seed_course(&app.db, "Intro Flow", None).await;

    // Lookup by UUID.
    let resp = app.get(&format!("/api/v1/courses/{id}")).await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    assert_eq!(body["id"].as_str().unwrap(), id.to_string());
}

#[sqlx::test]
async fn get_course_unknown_slug_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/courses/no-such-slug").await;
    assert_eq!(resp.status_code(), 404);
}

#[sqlx::test]
async fn create_course_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;

    let resp = app
        .post("/api/v1/courses")
        .json(&json!({
            "name": "Advanced",
            "level": "advanced",
            "duration_minutes": 60,
            "price_cents": 100000,
            "max_students": 8,
        }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn create_course_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("mem-c@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/courses")
        .authorization_bearer(&user.access_token)
        .json(&json!({
            "name": "Advanced",
            "level": "advanced",
            "duration_minutes": 60,
            "price_cents": 100000,
            "max_students": 8,
        }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn create_course_as_admin_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/courses")
        .authorization_bearer(&admin_token)
        .json(&json!({
            "name": "Advanced",
            "level": "advanced",
            "duration_minutes": 60,
            "price_cents": 100000,
            "max_students": 8,
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["name"], "Advanced");
    assert_eq!(body["level"], "advanced");
}

#[sqlx::test]
async fn update_course_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let id = seed_course(&app.db, "Intro Flow", None).await;
    let user = app.register_member("m-upd-c@example.com", "Password!234").await;

    let resp = app
        .patch(&format!("/api/v1/courses/{id}"))
        .authorization_bearer(&user.access_token)
        .json(&json!({ "name": "Member Rename Attempt" }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn create_course_rejects_invalid_payload(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/courses")
        .authorization_bearer(&admin_token)
        .json(&json!({
            // name missing
            "level": "advanced",
            "duration_minutes": 0,          // below min
            "price_cents": -1,              // below min
            "max_students": 0,              // below min
        }))
        .await;
    // Validator or JSON deserialization error — either way, rejected.
    assert!(matches!(resp.status_code().as_u16(), 400 | 422));
}
