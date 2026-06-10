//! HTTP integration tests for `/cart` endpoints.

mod common;

use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

async fn seed_product_via_admin(app: &common::http::TestApp, name: &str, stock: Option<i32>) -> Uuid {
    let (_admin, token) = app.seed_admin().await;
    let created: serde_json::Value = app
        .post("/api/v1/products")
        .authorization_bearer(&token)
        .json(&json!({
            "name": name,
            "product_type": "merchandise",
            "price_cents": 1000,
            "stock": stock,
        }))
        .await
        .json();
    Uuid::parse_str(created["id"].as_str().unwrap()).unwrap()
}

#[sqlx::test]
async fn get_cart_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/cart").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn get_cart_empty_for_new_user(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("c1@example.com", "Password!234").await;
    let resp = app
        .get("/api/v1/cart")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["total_cents"], 0);
}

#[sqlx::test]
async fn add_item_increases_cart(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("c2@example.com", "Password!234").await;
    let pid = seed_product_via_admin(&app, "Widget", Some(100)).await;

    let resp = app
        .post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "product_id": pid, "quantity": 3 }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["items"][0]["quantity"], 3);
    assert_eq!(body["total_cents"], 3000);
}

#[sqlx::test]
async fn add_item_nonexistent_product_returns_error(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("c3@example.com", "Password!234").await;
    let resp = app
        .post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "product_id": Uuid::now_v7(), "quantity": 1 }))
        .await;
    assert!(matches!(resp.status_code().as_u16(), 400 | 404));
}

#[sqlx::test]
async fn update_quantity_changes_value(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("c4@example.com", "Password!234").await;
    let pid = seed_product_via_admin(&app, "Gadget", Some(100)).await;
    app.post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "product_id": pid, "quantity": 1 }))
        .await;

    let resp = app
        .patch(&format!("/api/v1/cart/items/{pid}"))
        .authorization_bearer(&user.access_token)
        .json(&json!({ "quantity": 5 }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    assert_eq!(resp.json::<serde_json::Value>()["items"][0]["quantity"], 5);
}

#[sqlx::test]
async fn remove_item_empties_cart(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("c5@example.com", "Password!234").await;
    let pid = seed_product_via_admin(&app, "Gizmo", Some(100)).await;
    app.post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "product_id": pid, "quantity": 2 }))
        .await;

    let resp = app
        .delete(&format!("/api/v1/cart/items/{pid}"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200);
    assert_eq!(
        resp.json::<serde_json::Value>()["items"].as_array().unwrap().len(),
        0
    );
}

#[sqlx::test]
async fn clear_cart_returns_204(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("c6@example.com", "Password!234").await;
    let pid = seed_product_via_admin(&app, "Thing", Some(100)).await;
    app.post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "product_id": pid, "quantity": 1 }))
        .await;

    let resp = app
        .delete("/api/v1/cart")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 204);

    // GET /cart should now be empty.
    let resp = app
        .get("/api/v1/cart")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(
        resp.json::<serde_json::Value>()["items"].as_array().unwrap().len(),
        0
    );
}
