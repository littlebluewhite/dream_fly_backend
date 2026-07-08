//! HTTP integration tests for `/venues/*` endpoints.

mod common;

use common::fixtures::{seed_venue, seed_venue_category};
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

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

// ---------------------------------------------------------------------------
// PATCH /venues/{id} (Round 4 Task B1)
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn update_venue_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let id = seed_venue(&app.db, "Hall A", None).await;

    let resp = app
        .patch(&format!("/api/v1/venues/{id}"))
        .json(&json!({ "name": "Renamed" }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn update_venue_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let id = seed_venue(&app.db, "Hall A", None).await;
    let user = app
        .register_member("vmem-upd@example.com", "Password!234")
        .await;

    let resp = app
        .patch(&format!("/api/v1/venues/{id}"))
        .authorization_bearer(&user.access_token)
        .json(&json!({ "name": "Member Rename Attempt" }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn update_venue_as_admin_partial_update_only_name_changes(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let cat = seed_venue_category(&app.db, "Indoor").await;
    let id = seed_venue(&app.db, "Hall A", Some(cat)).await;

    let resp = app
        .patch(&format!("/api/v1/venues/{id}"))
        .authorization_bearer(&token)
        .json(&json!({ "name": "Hall A Renamed" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["name"], "Hall A Renamed");
    // Every other field must keep its seeded value — this is the crux of
    // the "partial update" contract: omitted fields are untouched, not
    // reset to defaults.
    assert_eq!(body["category_id"].as_str().unwrap(), cat.to_string());
    assert_eq!(body["description"], "Test venue");
    assert_eq!(body["features"], json!(["mat", "bar"]));
    assert!(body["image_url"].is_null());
    assert_eq!(body["is_active"], true);
}

#[sqlx::test]
async fn update_venue_clears_description_to_null(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let id = seed_venue(&app.db, "Hall B", None).await;

    let resp = app
        .patch(&format!("/api/v1/venues/{id}"))
        .authorization_bearer(&token)
        .json(&json!({ "description": null }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["description"].is_null());
    // name wasn't in the patch body, so it must remain untouched — proves
    // the explicit-null path is distinct from "field absent".
    assert_eq!(body["name"], "Hall B");

    let db_value: Option<String> =
        sqlx::query_scalar("SELECT description FROM venues WHERE id = $1")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(
        db_value.is_none(),
        "description must be NULL in the DB, not just absent from JSON"
    );
}

#[sqlx::test]
async fn update_venue_duplicate_slug_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let id_a = seed_venue(&app.db, "Hall A", None).await;
    let id_b = seed_venue(&app.db, "Hall B", None).await;

    let slug_a: String = sqlx::query_scalar("SELECT slug FROM venues WHERE id = $1")
        .bind(id_a)
        .fetch_one(&app.db)
        .await
        .unwrap();

    let resp = app
        .patch(&format!("/api/v1/venues/{id_b}"))
        .authorization_bearer(&token)
        .json(&json!({ "slug": slug_a }))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

#[sqlx::test]
async fn update_venue_unknown_id_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let missing_id = Uuid::now_v7();

    let resp = app
        .patch(&format!("/api/v1/venues/{missing_id}"))
        .authorization_bearer(&token)
        .json(&json!({ "name": "Ghost" }))
        .await;
    assert_eq!(resp.status_code(), 404);
}

#[sqlx::test]
async fn update_venue_empty_body_is_noop(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let id = seed_venue(&app.db, "Hall C", None).await;

    let resp = app
        .patch(&format!("/api/v1/venues/{id}"))
        .authorization_bearer(&token)
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["name"], "Hall C");
    assert_eq!(body["description"], "Test venue");
}
