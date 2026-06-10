//! HTTP integration tests for `/schedule/*` endpoints.

mod common;

use chrono::{Datelike, Duration, Utc};
use common::fixtures::seed_time_slot_full;
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn get_monthly_schedule_public(db: PgPool) {
    let app = spawn_test_app(db).await;
    let now = Utc::now();
    let resp = app
        .get(&format!(
            "/api/v1/schedule?year={}&month={}",
            now.year(),
            now.month()
        ))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    assert!(resp.json::<serde_json::Value>().is_array());
}

#[sqlx::test]
async fn get_monthly_schedule_missing_params_returns_400(db: PgPool) {
    let app = spawn_test_app(db).await;
    // Missing `year` and `month` → axum Query rejection is a 400.
    let resp = app.get("/api/v1/schedule").await;
    assert_eq!(resp.status_code(), 400);
}

#[sqlx::test]
async fn get_availability_returns_seeded_slot(db: PgPool) {
    let app = spawn_test_app(db).await;
    let id = seed_time_slot_full(&app.db, None, None, 10).await;

    let date = (Utc::now() + Duration::days(2)).date_naive();
    let resp = app
        .get(&format!("/api/v1/schedule/availability?date={date}"))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().unwrap();
    assert!(arr.iter().any(|s| s["id"].as_str().unwrap() == id.to_string()));
}

#[sqlx::test]
async fn create_slots_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/schedule/slots")
        .json(&json!({ "slots": [] }))
        .await;
    // Auth extractor runs before validation → 401.
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn create_slots_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("smem@example.com", "Password!234").await;
    let resp = app
        .post("/api/v1/schedule/slots")
        .authorization_bearer(&user.access_token)
        .json(&json!({
            "slots": [{
                "date": "2030-01-01",
                "start_time": "09:00",
                "end_time": "10:00",
                "capacity": 10,
            }]
        }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn create_slots_as_admin_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/schedule/slots")
        .authorization_bearer(&token)
        .json(&json!({
            "slots": [{
                "date": "2030-01-01",
                "start_time": "09:00",
                "end_time": "10:00",
                "capacity": 10,
            }]
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body.as_array().unwrap().len(), 1);
}

