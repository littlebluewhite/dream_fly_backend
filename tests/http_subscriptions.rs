//! HTTP integration tests for `/subscriptions` endpoints.

mod common;

use chrono::{Duration, Utc};
use common::fixtures::{seed_entitlement_product, seed_subscription};
use common::http::spawn_test_app;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn me_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/subscriptions/me").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn me_returns_only_callers_subscriptions(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user_a = app.register_member("sub-me-a@example.com", "Password!234").await;
    let user_b = app.register_member("sub-me-b@example.com", "Password!234").await;

    let product_id =
        seed_entitlement_product(&app.db, "ticket-me-a", "ticket", 5_000, None, Some(5)).await;
    seed_subscription(
        &app.db, user_a.user_id, product_id, "active", None, Some(5), Some(5), 5_000, Utc::now(),
    )
    .await;
    seed_subscription(
        &app.db, user_b.user_id, product_id, "active", None, Some(5), Some(5), 5_000, Utc::now(),
    )
    .await;

    let resp = app
        .get("/api/v1/subscriptions/me")
        .authorization_bearer(&user_a.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("array response");
    assert_eq!(arr.len(), 1, "must not include other users' subscriptions");
}

#[sqlx::test]
async fn me_orders_newest_first_with_full_response_shape(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("sub-me-c@example.com", "Password!234").await;
    let product_id =
        seed_entitlement_product(&app.db, "ticket-me-c", "ticket", 7_500, None, Some(8)).await;

    let older_id = seed_subscription(
        &app.db,
        user.user_id,
        product_id,
        "active",
        None,
        Some(8),
        Some(8),
        7_500,
        Utc::now() - Duration::days(2),
    )
    .await;
    let newer_id = seed_subscription(
        &app.db, user.user_id, product_id, "active", None, Some(8), Some(8), 7_500, Utc::now(),
    )
    .await;

    let resp = app
        .get("/api/v1/subscriptions/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array, not an envelope");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["id"], newer_id.to_string(), "newest first");
    assert_eq!(arr[1]["id"], older_id.to_string());

    let first = &arr[0];
    assert_eq!(first["product_id"], product_id.to_string());
    assert!(first["product_name"].as_str().unwrap().contains("ticket-me-c"));
    assert_eq!(first["status"], "active");
    assert_eq!(first["total_sessions"], 8);
    assert_eq!(first["remaining_sessions"], 8);
    assert_eq!(first["price_cents"], 7500);
    assert!(first["expires_at"].is_null());
    assert!(first["started_at"].is_string());
}

#[sqlx::test]
async fn me_status_derives_expired_for_past_expiry(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("sub-me-d@example.com", "Password!234").await;
    let product_id =
        seed_entitlement_product(&app.db, "membership-me-d", "membership", 9_000, Some(30), None)
            .await;
    seed_subscription(
        &app.db,
        user.user_id,
        product_id,
        "active",
        Some(Utc::now() - Duration::days(1)),
        None,
        None,
        9_000,
        Utc::now(),
    )
    .await;

    let resp = app
        .get("/api/v1/subscriptions/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body[0]["status"], "expired");
}

#[sqlx::test]
async fn redeem_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post(&format!("/api/v1/subscriptions/{}/redeem", Uuid::now_v7()))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn redeem_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("sub-redeem-a@example.com", "Password!234").await;
    let product_id =
        seed_entitlement_product(&app.db, "ticket-redeem-a", "ticket", 5_000, None, Some(5)).await;
    let sub_id = seed_subscription(
        &app.db, user.user_id, product_id, "active", None, Some(5), Some(5), 5_000, Utc::now(),
    )
    .await;

    let resp = app
        .post(&format!("/api/v1/subscriptions/{sub_id}/redeem"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn redeem_as_admin_decrements_and_returns_200(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;
    let owner = app.register_member("sub-redeem-owner-a@example.com", "Password!234").await;
    let product_id =
        seed_entitlement_product(&app.db, "ticket-redeem-b", "ticket", 5_000, None, Some(5)).await;
    let sub_id = seed_subscription(
        &app.db, owner.user_id, product_id, "active", None, Some(5), Some(3), 5_000, Utc::now(),
    )
    .await;

    let resp = app
        .post(&format!("/api/v1/subscriptions/{sub_id}/redeem"))
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["remaining_sessions"], 2);
    assert_eq!(body["id"], sub_id.to_string());
}

#[sqlx::test]
async fn redeem_as_coach_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_coach_id, token) = app.seed_user_with_roles("sub-redeem-coach@example.com", &["coach"]).await;
    let owner = app.register_member("sub-redeem-owner-b@example.com", "Password!234").await;
    let product_id =
        seed_entitlement_product(&app.db, "ticket-redeem-c", "ticket", 5_000, None, Some(5)).await;
    let sub_id = seed_subscription(
        &app.db, owner.user_id, product_id, "active", None, Some(5), Some(1), 5_000, Utc::now(),
    )
    .await;

    let resp = app
        .post(&format!("/api/v1/subscriptions/{sub_id}/redeem"))
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["remaining_sessions"], 0);
    assert_eq!(body["status"], "expired", "hitting 0 sessions must derive as expired");
}

#[sqlx::test]
async fn redeem_nonexistent_id_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;

    let resp = app
        .post(&format!("/api/v1/subscriptions/{}/redeem", Uuid::now_v7()))
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 404);
}

#[sqlx::test]
async fn redeem_exhausted_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;
    let owner = app.register_member("sub-redeem-owner-c@example.com", "Password!234").await;
    let product_id =
        seed_entitlement_product(&app.db, "ticket-redeem-d", "ticket", 5_000, None, Some(5)).await;
    let sub_id = seed_subscription(
        &app.db, owner.user_id, product_id, "active", None, Some(5), Some(0), 5_000, Utc::now(),
    )
    .await;

    let resp = app
        .post(&format!("/api/v1/subscriptions/{sub_id}/redeem"))
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}
