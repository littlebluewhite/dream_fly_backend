//! HTTP integration tests for `/contact` endpoints.

mod common;

use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn submit_contact_is_public(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/contact")
        .json(&json!({
            "name": "Alice",
            "email": "alice@example.com",
            "subject": "Question",
            "message": "Hello",
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["email"], "alice@example.com");
    assert_eq!(body["status"], "new");
}

#[sqlx::test]
async fn submit_contact_rejects_invalid_email(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/contact")
        .json(&json!({
            "name": "Alice",
            "email": "not-an-email",
            "subject": "Q",
            "message": "hi",
        }))
        .await;
    assert_eq!(resp.status_code(), 422);
}

#[sqlx::test]
async fn list_inquiries_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/contact/inquiries").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn list_inquiries_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("cmem@example.com", "Password!234").await;
    let resp = app
        .get("/api/v1/contact/inquiries")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn list_inquiries_as_admin_returns_200(db: PgPool) {
    let app = spawn_test_app(db).await;

    // Seed one inquiry via the public endpoint, then read it as admin.
    app.post("/api/v1/contact")
        .json(&json!({
            "name": "A", "email": "a@example.com", "subject": "s", "message": "m",
        }))
        .await;

    let (_admin, token) = app.seed_admin().await;
    let resp = app
        .get("/api/v1/contact/inquiries")
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["inquiries"].as_array().unwrap().len(), 1);
}
