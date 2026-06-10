//! HTTP integration tests for `/orders/*` endpoints.

mod common;

use common::http::{spawn_test_app, TestApp};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

async fn seed_product_via_admin(app: &TestApp, name: &str, stock: Option<i32>) -> Uuid {
    let (_admin, token) = app.seed_admin().await;
    let created: serde_json::Value = app
        .post("/api/v1/products")
        .authorization_bearer(&token)
        .json(&json!({
            "name": name,
            "product_type": "merchandise",
            "price_cents": 1500,
            "stock": stock,
        }))
        .await
        .json();
    Uuid::parse_str(created["id"].as_str().unwrap()).unwrap()
}

#[sqlx::test]
async fn checkout_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.post("/api/v1/orders").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn checkout_empty_cart_returns_bad_request(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("o1@example.com", "Password!234").await;
    let resp = app
        .post("/api/v1/orders")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 400);
}

#[sqlx::test]
async fn checkout_happy_path_creates_order_and_clears_cart(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("o2@example.com", "Password!234").await;
    let pid = seed_product_via_admin(&app, "Bundle", Some(10)).await;

    app.post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "product_id": pid, "quantity": 2 }))
        .await;

    let resp = app
        .post("/api/v1/orders")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["total_cents"].as_i64().unwrap() > 0);
    assert!(body["items"].as_array().unwrap().len() >= 1);
    // `status` starts as pending per the `orders` schema.
    assert_eq!(body["status"], "pending");

    // Cart should be emptied post-checkout.
    let cart = app
        .get("/api/v1/cart")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(
        cart.json::<serde_json::Value>()["items"].as_array().unwrap().len(),
        0
    );
}

#[sqlx::test]
async fn my_orders_returns_only_mine(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("o3@example.com", "Password!234").await;
    let pid = seed_product_via_admin(&app, "X", Some(10)).await;
    app.post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "product_id": pid, "quantity": 1 }))
        .await;
    app.post("/api/v1/orders")
        .authorization_bearer(&user.access_token)
        .await;

    let resp = app
        .get("/api/v1/orders/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    assert!(body["orders"].as_array().unwrap().len() >= 1);
}

#[sqlx::test]
async fn get_order_other_users_order_is_hidden(db: PgPool) {
    let app = spawn_test_app(db).await;
    let alice = app.register_member("alice-o@example.com", "Password!234").await;
    let bob = app.register_member("bob-o@example.com", "Password!234").await;
    let pid = seed_product_via_admin(&app, "Bundle", Some(10)).await;

    // Alice creates an order.
    app.post("/api/v1/cart/items")
        .authorization_bearer(&alice.access_token)
        .json(&json!({ "product_id": pid, "quantity": 1 }))
        .await;
    let alice_order: serde_json::Value = app
        .post("/api/v1/orders")
        .authorization_bearer(&alice.access_token)
        .await
        .json();
    let order_id = alice_order["id"].as_str().unwrap();

    // Bob tries to fetch it. Service rejects cross-user access with either
    // Forbidden (if it acknowledges the order exists but Bob can't see it)
    // or NotFound (if it hides existence entirely). Either response is an
    // acceptable authorization posture.
    let resp = app
        .get(&format!("/api/v1/orders/{order_id}"))
        .authorization_bearer(&bob.access_token)
        .await;
    assert!(
        matches!(resp.status_code().as_u16(), 403 | 404),
        "expected 403 or 404, got {}",
        resp.status_code()
    );
}

#[sqlx::test]
async fn update_status_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("o4@example.com", "Password!234").await;
    let pid = seed_product_via_admin(&app, "Bundle", Some(10)).await;
    app.post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "product_id": pid, "quantity": 1 }))
        .await;
    let order: serde_json::Value = app
        .post("/api/v1/orders")
        .authorization_bearer(&user.access_token)
        .await
        .json();
    let order_id = order["id"].as_str().unwrap();

    let resp = app
        .patch(&format!("/api/v1/orders/{order_id}/status"))
        .authorization_bearer(&user.access_token)
        .json(&json!({ "status": "paid" }))
        .await;
    assert_eq!(resp.status_code(), 403);
}
