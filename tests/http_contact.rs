//! HTTP integration tests for `/contact` endpoints.

mod common;

use common::http::{TestApp, spawn_test_app};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

/// Seed one inquiry via the public endpoint and return its id — used by the
/// PATCH tests below, which need an existing row to operate on.
async fn seed_inquiry(app: &TestApp) -> Uuid {
    let resp = app
        .post("/api/v1/contact")
        .json(&json!({
            "name": "Carol", "email": "carol@example.com",
            "subject": "s", "message": "m",
        }))
        .await;
    let body: serde_json::Value = resp.json();
    Uuid::parse_str(body["id"].as_str().expect("id")).expect("parse id")
}

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
    // Round 4 Task B5: existing callers that don't send the new fields must
    // see unchanged behavior — inquiry_type defaults to "general", metadata
    // is absent (null).
    assert_eq!(body["inquiry_type"], "general");
    assert!(body["metadata"].is_null());
}

#[sqlx::test]
async fn submit_contact_with_trial_type_and_metadata_round_trips(db: PgPool) {
    let app = spawn_test_app(db).await;
    let metadata = json!({
        "category": "tumbling",
        "student_age": 8,
        "preferred_day": "Saturday",
        "preferred_slot": "10:00-11:00",
        "parent_name": "Bob",
        "parent_phone": "0912345678",
        "student_name": "Alice Jr",
        "note": "first time trying a class",
    });
    let resp = app
        .post("/api/v1/contact")
        .json(&json!({
            "name": "Bob",
            "email": "bob@example.com",
            "subject": "Trial class",
            "message": "We'd like to try a class",
            "inquiry_type": "trial",
            "metadata": metadata,
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["inquiry_type"], "trial");
    assert_eq!(body["metadata"], metadata);
}

#[sqlx::test]
async fn submit_contact_invalid_inquiry_type_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/contact")
        .json(&json!({
            "name": "Eve",
            "email": "eve@example.com",
            "subject": "s",
            "message": "m",
            "inquiry_type": "bogus",
        }))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
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

// ---------------------------------------------------------------------------
// PATCH /contact/inquiries/{id} (Round 4 Task B5 — admin follow-up)
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn update_inquiry_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let id = seed_inquiry(&app).await;

    let resp = app
        .patch(&format!("/api/v1/contact/inquiries/{id}"))
        .json(&json!({ "status": "in_progress" }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn update_inquiry_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let id = seed_inquiry(&app).await;
    let user = app
        .register_member("cmem-upd@example.com", "Password!234")
        .await;

    let resp = app
        .patch(&format!("/api/v1/contact/inquiries/{id}"))
        .authorization_bearer(&user.access_token)
        .json(&json!({ "status": "in_progress" }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn update_inquiry_as_admin_returns_200(db: PgPool) {
    let app = spawn_test_app(db).await;
    let id = seed_inquiry(&app).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .patch(&format!("/api/v1/contact/inquiries/{id}"))
        .authorization_bearer(&token)
        .json(&json!({ "status": "in_progress" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
}

#[sqlx::test]
async fn update_inquiry_status_changes(db: PgPool) {
    let app = spawn_test_app(db).await;
    let id = seed_inquiry(&app).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .patch(&format!("/api/v1/contact/inquiries/{id}"))
        .authorization_bearer(&token)
        .json(&json!({ "status": "resolved" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["status"], "resolved");
    // assigned_to wasn't in the patch body — must stay untouched (null),
    // proving this is a partial update, not a full replace.
    assert!(body["assigned_to"].is_null());
}

#[sqlx::test]
async fn update_inquiry_invalid_status_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let id = seed_inquiry(&app).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .patch(&format!("/api/v1/contact/inquiries/{id}"))
        .authorization_bearer(&token)
        .json(&json!({ "status": "bogus" }))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

#[sqlx::test]
async fn update_inquiry_assigns_to_user(db: PgPool) {
    let app = spawn_test_app(db).await;
    let id = seed_inquiry(&app).await;
    let (admin_id, token) = app.seed_admin().await;

    let resp = app
        .patch(&format!("/api/v1/contact/inquiries/{id}"))
        .authorization_bearer(&token)
        .json(&json!({ "assigned_to": admin_id.to_string() }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["assigned_to"], admin_id.to_string());
}

#[sqlx::test]
async fn update_inquiry_clears_assigned_to_to_null(db: PgPool) {
    let app = spawn_test_app(db).await;
    let id = seed_inquiry(&app).await;
    let (admin_id, token) = app.seed_admin().await;

    // First assign, then clear — proves the explicit-null path is distinct
    // from "field absent".
    let assign_resp = app
        .patch(&format!("/api/v1/contact/inquiries/{id}"))
        .authorization_bearer(&token)
        .json(&json!({ "assigned_to": admin_id.to_string() }))
        .await;
    assert_eq!(
        assign_resp.status_code(),
        200,
        "body={}",
        assign_resp.text()
    );

    let resp = app
        .patch(&format!("/api/v1/contact/inquiries/{id}"))
        .authorization_bearer(&token)
        .json(&json!({ "assigned_to": null }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["assigned_to"].is_null());

    let db_value: Option<Uuid> =
        sqlx::query_scalar("SELECT assigned_to FROM contact_inquiries WHERE id = $1")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(
        db_value.is_none(),
        "assigned_to must be NULL in the DB, not just absent from JSON"
    );
}

#[sqlx::test]
async fn update_inquiry_unknown_id_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let missing_id = Uuid::now_v7();

    let resp = app
        .patch(&format!("/api/v1/contact/inquiries/{missing_id}"))
        .authorization_bearer(&token)
        .json(&json!({ "status": "resolved" }))
        .await;
    assert_eq!(resp.status_code(), 404);
}
