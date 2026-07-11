//! HTTP integration tests for `/products/*` endpoints.

mod common;

use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn list_products_public_empty(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/products").await;
    assert_eq!(resp.status_code(), 200);
    // `ProductListResponse` envelope: { products: [], total, page, per_page }.
    let body: serde_json::Value = resp.json();
    assert!(body["products"].as_array().unwrap().is_empty());
    assert_eq!(body["total"], 0);
}

#[sqlx::test]
async fn create_product_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;

    let resp = app
        .post("/api/v1/products")
        .json(&json!({
            "name": "T-shirt",
            "product_type": "merchandise",
            "price_cents": 1000,
        }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn create_product_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("pm@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/products")
        .authorization_bearer(&user.access_token)
        .json(&json!({
            "name": "T-shirt",
            "product_type": "merchandise",
            "price_cents": 1000,
        }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn create_product_as_admin_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/products")
        .authorization_bearer(&token)
        .json(&json!({
            "name": "T-shirt",
            "product_type": "merchandise",
            "price_cents": 1000,
            "stock": 50,
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["name"], "T-shirt");
    assert_eq!(body["stock"], 50);
    // valid_days/session_count weren't provided — must default to null.
    assert!(body["valid_days"].is_null());
    assert!(body["session_count"].is_null());

    // Now publicly listable.
    let list: serde_json::Value = app.get("/api/v1/products").await.json();
    assert_eq!(list["products"].as_array().unwrap().len(), 1);
    assert_eq!(list["total"], 1);
}

#[sqlx::test]
async fn create_product_with_valid_days_and_session_count(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/products")
        .authorization_bearer(&token)
        .json(&json!({
            "name": "10-Class Pass",
            "product_type": "course_package",
            "price_cents": 500000,
            "valid_days": 90,
            "session_count": 10,
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["valid_days"], 90);
    assert_eq!(body["session_count"], 10);
}

#[sqlx::test]
async fn update_product_sets_valid_days_and_session_count(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let created: serde_json::Value = app
        .post("/api/v1/products")
        .authorization_bearer(&token)
        .json(&json!({
            "name": "Membership",
            "product_type": "membership",
            "price_cents": 300000,
        }))
        .await
        .json();
    let id = created["id"].as_str().unwrap();

    let resp = app
        .patch(&format!("/api/v1/products/{id}"))
        .authorization_bearer(&token)
        .json(&json!({ "valid_days": 180, "session_count": 20 }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["valid_days"], 180);
    assert_eq!(body["session_count"], 20);
}

#[sqlx::test]
async fn create_product_rejects_invalid_price(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/products")
        .authorization_bearer(&token)
        .json(&json!({
            "name": "Bad",
            "product_type": "merchandise",
            "price_cents": -100,
        }))
        .await;
    assert!(matches!(resp.status_code().as_u16(), 400 | 422));
}

#[sqlx::test]
async fn get_product_by_slug_after_create(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let created: serde_json::Value = app
        .post("/api/v1/products")
        .authorization_bearer(&token)
        .json(&json!({
            "name": "Hoodie",
            "slug": "hoodie",
            "product_type": "merchandise",
            "price_cents": 2000,
        }))
        .await
        .json();
    let slug = created["slug"].as_str().unwrap().to_string();

    let resp = app.get(&format!("/api/v1/products/{slug}")).await;
    assert_eq!(resp.status_code(), 200);
    assert_eq!(resp.json::<serde_json::Value>()["slug"], slug);
}

#[sqlx::test]
async fn get_product_by_id_after_create(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let created: serde_json::Value = app
        .post("/api/v1/products")
        .authorization_bearer(&token)
        .json(&json!({
            "name": "Cap",
            "product_type": "merchandise",
            "price_cents": 500,
        }))
        .await
        .json();
    let id = created["id"].as_str().unwrap().to_string();

    let resp = app.get(&format!("/api/v1/products/{id}")).await;
    assert_eq!(resp.status_code(), 200);
    assert_eq!(resp.json::<serde_json::Value>()["id"].as_str().unwrap(), id);
}

#[sqlx::test]
async fn update_product_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let created: serde_json::Value = app
        .post("/api/v1/products")
        .authorization_bearer(&token)
        .json(&json!({
            "name": "Socks",
            "product_type": "merchandise",
            "price_cents": 500,
        }))
        .await
        .json();
    let id = created["id"].as_str().unwrap();

    let member = app.register_member("pm2@example.com", "Password!234").await;
    let resp = app
        .patch(&format!("/api/v1/products/{id}"))
        .authorization_bearer(&member.access_token)
        .json(&json!({ "price_cents": 999 }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

// ---------------------------------------------------------------------------
// BE#22 — PATCH `null` must clear nullable columns, not be silently ignored
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn update_product_clears_nullable_fields_to_null(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let created: serde_json::Value = app
        .post("/api/v1/products")
        .authorization_bearer(&token)
        .json(&json!({
            "name": "Clearable Ticket",
            "product_type": "ticket",
            "price_cents": 100000,
            "original_price_cents": 150000,
            "badge": "熱銷",
            "stock": 20,
            "valid_days": 30,
            "session_count": 5,
        }))
        .await
        .json();
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["original_price_cents"], 150000);
    assert_eq!(created["badge"], "熱銷");
    assert_eq!(created["stock"], 20);
    assert_eq!(created["valid_days"], 30);
    assert_eq!(created["session_count"], 5);

    // Explicit null on all five: must clear to NULL, not be silently
    // ignored. `stock: null` in particular means "unlimited stock" — the
    // ticket page's actual business semantics, not just a data-hygiene nit.
    let resp = app
        .patch(&format!("/api/v1/products/{id}"))
        .authorization_bearer(&token)
        .json(&json!({
            "original_price_cents": null,
            "badge": null,
            "stock": null,
            "valid_days": null,
            "session_count": null,
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["original_price_cents"].is_null());
    assert!(body["badge"].is_null());
    assert!(body["stock"].is_null());
    assert!(body["quota"].is_null());
    assert!(body["valid_days"].is_null());
    assert!(body["session_count"].is_null());

    let row: (Option<i64>, Option<String>, Option<i32>, Option<i32>, Option<i32>) = sqlx::query_as(
        "SELECT original_price_cents, badge, stock, valid_days, session_count FROM products WHERE id = $1",
    )
    .bind(Uuid::parse_str(id).unwrap())
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert!(row.0.is_none(), "original_price_cents must be NULL in the DB, not just absent from JSON");
    assert!(row.1.is_none(), "badge must be NULL in the DB, not just absent from JSON");
    assert!(row.2.is_none(), "stock must be NULL in the DB, not just absent from JSON");
    assert!(row.3.is_none(), "valid_days must be NULL in the DB, not just absent from JSON");
    assert!(row.4.is_none(), "session_count must be NULL in the DB, not just absent from JSON");

    // Field-absent PATCH afterward must not error and must leave the
    // now-NULL columns alone — proves "absent" stays distinct from "null".
    let resp2 = app
        .patch(&format!("/api/v1/products/{id}"))
        .authorization_bearer(&token)
        .json(&json!({ "name": "Renamed After Clear" }))
        .await;
    assert_eq!(resp2.status_code(), 200, "body={}", resp2.text());
    let body2: serde_json::Value = resp2.json();
    assert_eq!(body2["name"], "Renamed After Clear");
    assert!(body2["original_price_cents"].is_null());
    assert!(body2["badge"].is_null());
    assert!(body2["stock"].is_null());
    assert!(body2["valid_days"].is_null());
    assert!(body2["session_count"].is_null());
}
