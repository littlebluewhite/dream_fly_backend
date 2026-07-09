//! HTTP integration tests for `/bookings/*` endpoints.

mod common;

use common::fixtures::seed_time_slot_full;
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn create_booking_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/bookings")
        .json(&json!({ "time_slot_id": Uuid::now_v7() }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn create_booking_happy_path(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("bmem@example.com", "Password!234").await;
    let slot = seed_time_slot_full(&app.db, None, None, 5).await;

    let resp = app
        .post("/api/v1/bookings")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "time_slot_id": slot }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["status"], "confirmed");
    assert_eq!(body["user_id"].as_str().unwrap(), user.user_id.to_string());
}

/// Task P4-B2: the booking response carries the slot's price snapshot.
#[sqlx::test]
async fn create_booking_response_includes_price_cents(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("price@example.com", "Password!234").await;
    let slot = seed_time_slot_full(&app.db, None, None, 5).await;
    sqlx::query("UPDATE time_slots SET price_cents = $2 WHERE id = $1")
        .bind(slot)
        .bind(30_000_i64)
        .execute(&app.db)
        .await
        .expect("bump slot price");

    let resp = app
        .post("/api/v1/bookings")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "time_slot_id": slot }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["price_cents"], 30000);
}

#[sqlx::test]
async fn create_booking_duplicate_returns_conflict(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("bmem2@example.com", "Password!234").await;
    let slot = seed_time_slot_full(&app.db, None, None, 5).await;

    let payload = json!({ "time_slot_id": slot });
    app.post("/api/v1/bookings")
        .authorization_bearer(&user.access_token)
        .json(&payload)
        .await;
    let resp = app
        .post("/api/v1/bookings")
        .authorization_bearer(&user.access_token)
        .json(&payload)
        .await;
    assert_eq!(resp.status_code(), 409);
}

#[sqlx::test]
async fn my_bookings_returns_only_mine(db: PgPool) {
    let app = spawn_test_app(db).await;
    let alice = app.register_member("alice@example.com", "Password!234").await;
    let bob = app.register_member("bob@example.com", "Password!234").await;
    let slot_a = seed_time_slot_full(&app.db, None, None, 5).await;
    let slot_b = seed_time_slot_full(&app.db, None, None, 5).await;

    app.post("/api/v1/bookings")
        .authorization_bearer(&alice.access_token)
        .json(&json!({ "time_slot_id": slot_a }))
        .await;
    app.post("/api/v1/bookings")
        .authorization_bearer(&bob.access_token)
        .json(&json!({ "time_slot_id": slot_b }))
        .await;

    let mine = app
        .get("/api/v1/bookings/me")
        .authorization_bearer(&alice.access_token)
        .await;
    assert_eq!(mine.status_code(), 200);
    let body: serde_json::Value = mine.json();
    let bookings = body["bookings"].as_array().unwrap();
    assert_eq!(bookings.len(), 1);
    assert_eq!(
        bookings[0]["user_id"].as_str().unwrap(),
        alice.user_id.to_string()
    );
}

#[sqlx::test]
async fn cancel_booking_happy_path(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("cm@example.com", "Password!234").await;
    let slot = seed_time_slot_full(&app.db, None, None, 5).await;

    let created: serde_json::Value = app
        .post("/api/v1/bookings")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "time_slot_id": slot }))
        .await
        .json();
    let booking_id = created["id"].as_str().unwrap();

    let resp = app
        .patch(&format!("/api/v1/bookings/{booking_id}/cancel"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    assert_eq!(resp.json::<serde_json::Value>()["status"], "cancelled");
}

#[sqlx::test]
async fn list_all_bookings_as_member_returns_403(db: PgPool) {
    // GET /bookings (list_all) is an admin-only overview — members must
    // use /bookings/me instead. The handler enforces this, so members get
    // a 403 even with a valid access token.
    let app = spawn_test_app(db).await;
    let user = app.register_member("la@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/bookings")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn list_all_bookings_as_admin_sees_everything(db: PgPool) {
    let app = spawn_test_app(db).await;
    let alice = app.register_member("alice-b@example.com", "Password!234").await;
    let slot = seed_time_slot_full(&app.db, None, None, 5).await;
    app.post("/api/v1/bookings")
        .authorization_bearer(&alice.access_token)
        .json(&json!({ "time_slot_id": slot }))
        .await;

    let (_admin, token) = app.seed_admin().await;
    let resp = app
        .get("/api/v1/bookings")
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["bookings"].as_array().unwrap().len() >= 1);
}
