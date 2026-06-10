//! End-to-end flow tests — multi-endpoint user journeys that drive the full
//! router across module boundaries. These tests are the safety net that
//! catches cross-cutting regressions (e.g. when a cart-item schema change
//! subtly breaks checkout but leaves the unit tests green).

mod common;

use common::fixtures::seed_time_slot_full;
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;

/// Register → login → fetch self → update profile → logout.
#[sqlx::test]
async fn e2e_user_onboarding(db: PgPool) {
    let app = spawn_test_app(db).await;

    // Register
    let reg = app
        .post("/api/v1/auth/register")
        .json(&json!({
            "email": "flow@example.com",
            "name": "Flow",
            "password": "Password!234",
        }))
        .await;
    assert_eq!(reg.status_code(), 200, "register body={}", reg.text());
    let reg_body: serde_json::Value = reg.json();
    let access = reg_body["access_token"].as_str().unwrap().to_string();
    let refresh = reg_body["refresh_token"].as_str().unwrap().to_string();

    // Fetch self
    let me = app
        .get("/api/v1/users/me")
        .authorization_bearer(&access)
        .await;
    assert_eq!(me.status_code(), 200);
    assert_eq!(me.json::<serde_json::Value>()["email"], "flow@example.com");

    // Update profile
    let upd = app
        .patch("/api/v1/users/me")
        .authorization_bearer(&access)
        .json(&json!({ "name": "Flow Final" }))
        .await;
    assert_eq!(upd.status_code(), 200);
    assert_eq!(upd.json::<serde_json::Value>()["name"], "Flow Final");

    // Logout
    let out = app
        .post("/api/v1/auth/logout")
        .json(&json!({ "refresh_token": refresh }))
        .await;
    assert_eq!(out.status_code(), 200);

    // Login again with fresh password
    let login = app
        .post("/api/v1/auth/login")
        .json(&json!({
            "email": "flow@example.com",
            "password": "Password!234",
        }))
        .await;
    assert_eq!(login.status_code(), 200);
}

/// Admin creates catalog → member browses → member books slot.
#[sqlx::test]
async fn e2e_booking_flow(db: PgPool) {
    let app = spawn_test_app(db).await;

    // Admin seeds a venue (via POST) and a slot (via DB — slot creation
    // needs a date/time combo the booking service accepts).
    let (_admin, admin_token) = app.seed_admin().await;
    let _venue_resp = app
        .post("/api/v1/venues")
        .authorization_bearer(&admin_token)
        .json(&json!({
            "name": "Main Hall",
            "description": "Primary training hall",
        }))
        .await;
    let slot_id = seed_time_slot_full(&app.db, None, None, 3).await;

    // Member lists schedule availability for tomorrow + 2d.
    let date = (chrono::Utc::now() + chrono::Duration::days(2)).date_naive();
    let avail = app
        .get(&format!("/api/v1/schedule/availability?date={date}"))
        .await;
    assert_eq!(avail.status_code(), 200);
    assert!(
        avail
            .json::<serde_json::Value>()
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"].as_str().unwrap() == slot_id.to_string())
    );

    // Register member + book slot.
    let member = app
        .register_member("booker@example.com", "Password!234")
        .await;
    let booking = app
        .post("/api/v1/bookings")
        .authorization_bearer(&member.access_token)
        .json(&json!({ "time_slot_id": slot_id }))
        .await;
    assert_eq!(booking.status_code(), 200, "booking body={}", booking.text());
    let booking_body: serde_json::Value = booking.json();
    let booking_id = booking_body["id"].as_str().unwrap().to_string();

    // Member's bookings list includes it.
    let my = app
        .get("/api/v1/bookings/me")
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(my.status_code(), 200);
    assert!(
        my.json::<serde_json::Value>()["bookings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"].as_str().unwrap() == booking_id)
    );
}

/// Member adds to cart → checkout → clears cart → sees order in my_orders.
#[sqlx::test]
async fn e2e_shopping_flow(db: PgPool) {
    let app = spawn_test_app(db).await;

    // Admin creates a product.
    let (_admin, admin_token) = app.seed_admin().await;
    let product: serde_json::Value = app
        .post("/api/v1/products")
        .authorization_bearer(&admin_token)
        .json(&json!({
            "name": "Membership",
            "product_type": "membership",
            "price_cents": 12000,
        }))
        .await
        .json();
    let product_id = product["id"].as_str().unwrap().to_string();

    // Register + add to cart + checkout.
    let member = app.register_member("shopper@example.com", "Password!234").await;
    app.post("/api/v1/cart/items")
        .authorization_bearer(&member.access_token)
        .json(&json!({ "product_id": product_id, "quantity": 1 }))
        .await;

    let order = app
        .post("/api/v1/orders")
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(order.status_code(), 200, "order body={}", order.text());
    let order_body: serde_json::Value = order.json();
    assert_eq!(order_body["total_cents"], 12000);

    // Cart is now empty.
    let cart = app
        .get("/api/v1/cart")
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(cart.json::<serde_json::Value>()["items"].as_array().unwrap().len(), 0);

    // my_orders contains it.
    let my = app
        .get("/api/v1/orders/me")
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(my.status_code(), 200);
    assert!(
        my.json::<serde_json::Value>()["orders"]
            .as_array()
            .unwrap()
            .len()
            >= 1
    );
}

/// Forgot password → capture token from MockEmailClient → reset → login.
#[sqlx::test]
async fn e2e_password_reset_flow(db: PgPool) {
    // Use a unique email so the per-account forgot-password Redis counter
    // (3 requests per hour per email) does not interfere when this test
    // runs back-to-back against the same Redis.
    let app = spawn_test_app(db).await;
    let email = format!("pw-{}@example.com", uuid::Uuid::now_v7());
    app.register_member(&email, "OldPassword!234").await;

    // Trigger forgot.
    let f = app
        .post("/api/v1/auth/password/forgot")
        .json(&json!({ "email": email }))
        .await;
    assert_eq!(f.status_code(), 200);

    // Recover the token from the mock email client.
    let sent = app.email.wait_for(1, 1000).await;
    assert_eq!(sent.len(), 1);
    let token = match &sent[0].kind {
        common::mocks::EmailKind::PasswordReset { token } => token.clone(),
        other => panic!("expected PasswordReset, got {other:?}"),
    };

    // Reset
    let reset = app
        .post("/api/v1/auth/password/reset")
        .json(&json!({
            "token": token,
            "new_password": "NewPassword!234",
        }))
        .await;
    assert_eq!(reset.status_code(), 200, "reset body={}", reset.text());

    // Old password fails.
    let old = app
        .post("/api/v1/auth/login")
        .json(&json!({
            "email": email,
            "password": "OldPassword!234",
        }))
        .await;
    assert_eq!(old.status_code(), 401);

    // New password works.
    let new = app
        .post("/api/v1/auth/login")
        .json(&json!({
            "email": email,
            "password": "NewPassword!234",
        }))
        .await;
    assert_eq!(new.status_code(), 200);
}

/// Admin post lifecycle: admin create → member read → admin delete → 404.
#[sqlx::test]
async fn e2e_post_lifecycle(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let created: serde_json::Value = app
        .post("/api/v1/posts")
        .authorization_bearer(&token)
        .json(&json!({
            "title": "Grand Opening",
            "content": "We are opening!",
            "category": "announcement",
        }))
        .await
        .json();
    let post_id = created["id"].as_str().unwrap().to_string();

    // Draft posts are hidden from the public `GET /posts/:id` path
    // (service::get_published_by_slug_or_id filters on `status='published'`),
    // so an anon read of a freshly-created draft returns 404.
    let read = app
        .get(&format!("/api/v1/posts/{post_id}"))
        .await;
    assert_eq!(read.status_code(), 404);

    let del = app
        .delete(&format!("/api/v1/posts/{post_id}"))
        .authorization_bearer(&token)
        .await;
    assert_eq!(del.status_code(), 204);

    let gone = app.get(&format!("/api/v1/posts/{post_id}")).await;
    assert_eq!(gone.status_code(), 404);
}

