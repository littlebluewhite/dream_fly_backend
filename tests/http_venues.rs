//! HTTP integration tests for `/venues/*` endpoints.

mod common;

use common::fixtures::{seed_venue, seed_venue_category};
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn list_venues_public(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/venues").await;
    assert_eq!(resp.status_code(), 200);
    assert!(resp.json::<serde_json::Value>().is_array());
}

#[sqlx::test]
async fn list_venues_returns_seeded(db: PgPool) {
    let app = spawn_test_app(db).await;
    let cat = seed_venue_category(&app.db, "Indoor").await;
    let id = seed_venue(&app.db, "Hall A", Some(cat)).await;

    let resp = app.get("/api/v1/venues").await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"].as_str().unwrap(), id.to_string());
    assert_eq!(arr[0]["name"], "Hall A");
}

#[sqlx::test]
async fn get_venue_by_slug_returns_detail(db: PgPool) {
    let app = spawn_test_app(db).await;
    seed_venue(&app.db, "Hall A", None).await;
    // seed_venue generates slug like "hall-a-<hex>"; fetch by listing to get it.
    let list: serde_json::Value = app.get("/api/v1/venues").await.json();
    let slug = list[0]["slug"].as_str().unwrap();

    let resp = app.get(&format!("/api/v1/venues/{slug}")).await;
    assert_eq!(resp.status_code(), 200);
    assert_eq!(resp.json::<serde_json::Value>()["slug"], slug);
}

#[sqlx::test]
async fn get_venue_unknown_slug_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/venues/does-not-exist").await;
    assert_eq!(resp.status_code(), 404);
}

#[sqlx::test]
async fn create_venue_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/venues")
        .json(&json!({ "name": "New Hall" }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn create_venue_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("vmem@example.com", "Password!234").await;
    let resp = app
        .post("/api/v1/venues")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "name": "New Hall" }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn create_venue_as_admin_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let resp = app
        .post("/api/v1/venues")
        .authorization_bearer(&token)
        .json(&json!({
            "name": "New Hall",
            "description": "Large training hall",
            "features": ["mat", "bar"],
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    assert_eq!(resp.json::<serde_json::Value>()["name"], "New Hall");
}
