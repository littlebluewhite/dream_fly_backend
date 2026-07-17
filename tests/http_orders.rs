//! HTTP integration tests for `/orders/*` endpoints.

mod common;

use chrono::{Duration, TimeZone, Utc};
use common::fixtures::{seed_course_with_capacity, seed_entitlement_product};
use common::http::{spawn_test_app, spawn_test_app_with, TestApp};
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
        .json(&json!({ "item_type": "product", "item_id": pid, "quantity": 2 }))
        .await;

    let resp = app
        .post("/api/v1/orders")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["total_cents"].as_i64().unwrap() > 0);
    assert!(body["items"].as_array().unwrap().len() >= 1);
    // Checkout creates the order already `paid` — there's no separate
    // payment-capture step in this application.
    assert_eq!(body["status"], "paid");

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

/// Task P4-B1: `payment_method` omitted entirely defaults to `credit_card`
/// (back-compat — existing checkout callers that never send this field
/// must keep working).
#[sqlx::test]
async fn checkout_without_payment_method_defaults_to_credit_card(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("o7@example.com", "Password!234").await;
    let pid = seed_product_via_admin(&app, "Bundle", Some(10)).await;

    app.post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "item_type": "product", "item_id": pid, "quantity": 1 }))
        .await;

    let resp = app
        .post("/api/v1/orders")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["payment_method"], "credit_card");
}

/// A valid, explicitly-supplied `payment_method` is persisted and echoed
/// back on the order.
#[sqlx::test]
async fn checkout_with_valid_payment_method_persists_it(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("o8@example.com", "Password!234").await;
    let pid = seed_product_via_admin(&app, "Bundle", Some(10)).await;

    app.post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "item_type": "product", "item_id": pid, "quantity": 1 }))
        .await;

    let resp = app
        .post("/api/v1/orders")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "payment_method": "line_pay" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["payment_method"], "line_pay");
}

/// A `payment_method` outside the supported value domain is rejected before
/// any order is created — 422, and cart items must survive untouched.
#[sqlx::test]
async fn checkout_with_invalid_payment_method_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("o9@example.com", "Password!234").await;
    let pid = seed_product_via_admin(&app, "Bundle", Some(10)).await;

    app.post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "item_type": "product", "item_id": pid, "quantity": 1 }))
        .await;

    let resp = app
        .post("/api/v1/orders")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "payment_method": "bitcoin" }))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());

    // Rejected checkout must not clear the cart or create an order.
    let cart = app
        .get("/api/v1/cart")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(
        cart.json::<serde_json::Value>()["items"].as_array().unwrap().len(),
        1,
        "cart must survive a rejected checkout"
    );
}

#[sqlx::test]
async fn my_orders_returns_only_mine(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("o3@example.com", "Password!234").await;
    let pid = seed_product_via_admin(&app, "X", Some(10)).await;
    app.post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "item_type": "product", "item_id": pid, "quantity": 1 }))
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

    // OrderSummary.items brief — name comes from the order_items snapshot,
    // not a live product join.
    let items = body["orders"][0]["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "X");
    assert_eq!(items[0]["quantity"], 1);
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
        .json(&json!({ "item_type": "product", "item_id": pid, "quantity": 1 }))
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
        .json(&json!({ "item_type": "product", "item_id": pid, "quantity": 1 }))
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

#[sqlx::test]
async fn admin_list_orders_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/orders").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn admin_list_orders_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("o5@example.com", "Password!234").await;
    let resp = app
        .get("/api/v1/orders")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn admin_list_orders_as_admin_paginates_and_includes_user_info(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("o6@example.com", "Password!234").await;
    let pid = seed_product_via_admin(&app, "Bundle", Some(10)).await;

    for _ in 0..3 {
        app.post("/api/v1/cart/items")
            .authorization_bearer(&user.access_token)
            .json(&json!({ "item_type": "product", "item_id": pid, "quantity": 1 }))
            .await;
        app.post("/api/v1/orders")
            .authorization_bearer(&user.access_token)
            .await
            .assert_status_ok();
    }

    let (_admin, admin_token) = app.seed_admin().await;
    let resp = app
        .get("/api/v1/orders?page=1&per_page=2")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["orders"].as_array().unwrap().len(), 2);
    assert_eq!(body["total"], 3);
    assert_eq!(body["page"], 1);
    assert_eq!(body["per_page"], 2);

    let first = &body["orders"][0];
    assert_eq!(first["user_email"], "o6@example.com");
    assert!(first["user_name"].is_string());
    assert!(first["order_number"].is_string());
    assert_eq!(first["status"], "paid");

    let items = first["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "Bundle");
    assert_eq!(items[0]["quantity"], 1);
}

// ---------------------------------------------------------------------
// Clock seam: order-number datestamp is the studio-LOCAL day (Phase 6
// Commit B — the fourth and last authorized user-visible behavior change).
// ---------------------------------------------------------------------

/// A checkout at a Taipei-evening instant (UTC 16:00–24:00), when Taipei
/// (UTC+8) has already rolled into the next calendar day, stamps the order
/// number with the studio-LOCAL day sampled from the injected clock — not
/// the UTC day `Utc::now()` used to stamp. Studio timezone pinned to
/// `Asia/Taipei` and "now" pinned via `MockClock`.
#[sqlx::test]
async fn checkout_order_number_uses_studio_local_day_not_utc(db: PgPool) {
    let app = spawn_test_app_with(db, |cfg| {
        cfg.server.studio_timezone = "Asia/Taipei".into();
    })
    .await;
    // 2026-07-14 16:30:00Z = 2026-07-15 00:30 Taipei — the studio's calendar
    // day is already the 15th while UTC is still the 14th (UTC day + 1).
    app.clock
        .set(Utc.with_ymd_and_hms(2026, 7, 14, 16, 30, 0).unwrap());

    let user = app
        .register_member("taipei-eve@example.com", "Password!234")
        .await;
    let pid = seed_product_via_admin(&app, "Bundle", Some(10)).await;
    app.post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "item_type": "product", "item_id": pid, "quantity": 1 }))
        .await;

    let resp = app
        .post("/api/v1/orders")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let order_number = body["order_number"].as_str().expect("order_number");

    // "DF-YYYYMMDD........": the 8-digit date field is the Taipei day
    // (20260715), not the UTC day (20260714).
    assert_eq!(
        &order_number[3..11],
        "20260715",
        "order_number={order_number}"
    );
}

/// Cross-day idempotent replay: the datestamp is generated exactly once (at
/// the first checkout) and never regenerated. Replaying the same
/// Idempotency-Key a full studio day later returns the original order —
/// original number, no new row — because the idempotency pre-check
/// short-circuits before the order number is ever built. The contract's
/// idempotency promise does not loosen once "now" is an injected parameter.
#[sqlx::test]
async fn checkout_idempotent_replay_across_studio_day_keeps_original_order(db: PgPool) {
    let app = spawn_test_app_with(db, |cfg| {
        cfg.server.studio_timezone = "Asia/Taipei".into();
    })
    .await;
    // Pin to a fixed Taipei-evening instant — studio day D = 2026-07-15.
    app.clock
        .set(Utc.with_ymd_and_hms(2026, 7, 14, 16, 30, 0).unwrap());

    let user = app
        .register_member("replay-day@example.com", "Password!234")
        .await;
    let pid = seed_product_via_admin(&app, "Bundle", Some(10)).await;
    app.post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "item_type": "product", "item_id": pid, "quantity": 1 }))
        .await;

    let first: serde_json::Value = app
        .post("/api/v1/orders")
        .authorization_bearer(&user.access_token)
        .add_header("idempotency-key", "cross-day-key-1")
        .await
        .json();
    let first_number = first["order_number"]
        .as_str()
        .expect("order_number")
        .to_string();
    assert_eq!(&first_number[3..11], "20260715", "first stamps studio day D");

    // Advance a full day → studio day D+1 (2026-07-16).
    app.clock.advance(Duration::days(1));

    let second: serde_json::Value = app
        .post("/api/v1/orders")
        .authorization_bearer(&user.access_token)
        .add_header("idempotency-key", "cross-day-key-1")
        .await
        .json();

    // Same order returned verbatim — the stamp was NOT regenerated to D+1.
    assert_eq!(
        second["order_number"].as_str().unwrap(),
        first_number,
        "replay must return the original order number"
    );

    // And the replay created no second order row.
    let order_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orders WHERE user_id = $1")
        .bind(user.user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(order_count, 1, "replay must not create a second order");
}

// ---------------------------------------------------------------------
// Step 10e row 12: admin refund over HTTP returns the cancelled artifacts —
// `fetch_artifacts` re-reads the latest enrolment/subscription rows, and the
// response DTOs are unchanged (their `status` fields simply now read
// `cancelled`).
// ---------------------------------------------------------------------

#[sqlx::test]
async fn update_status_refund_via_http_returns_cancelled_artifacts(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("refund-http@example.com", "Password!234")
        .await;
    let course = seed_course_with_capacity(&app.db, "HTTP Refund Course", None, 12).await;
    let membership =
        seed_entitlement_product(&app.db, "http-refund-membership", "membership", 8_000, None, None)
            .await;

    // Build a cart with a course (→ enrolment) and a membership (→ subscription).
    app.post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "item_type": "course", "item_id": course }))
        .await;
    app.post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "item_type": "product", "item_id": membership, "quantity": 1 }))
        .await;

    let order: serde_json::Value = app
        .post("/api/v1/orders")
        .authorization_bearer(&user.access_token)
        .await
        .json();
    let order_id = order["id"].as_str().unwrap().to_string();
    assert_eq!(order["enrolments"].as_array().unwrap().len(), 1);
    assert_eq!(order["subscriptions"].as_array().unwrap().len(), 1);

    // Admin refunds the order.
    let (_admin, admin_token) = app.seed_admin().await;
    let resp = app
        .patch(&format!("/api/v1/orders/{order_id}/status"))
        .authorization_bearer(&admin_token)
        .json(&json!({ "status": "refunded" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["status"], "refunded");
    assert_eq!(
        body["enrolments"][0]["status"], "cancelled",
        "enrolment artifact reads cancelled"
    );
    assert_eq!(
        body["subscriptions"][0]["status"], "cancelled",
        "subscription artifact reads cancelled"
    );
}
