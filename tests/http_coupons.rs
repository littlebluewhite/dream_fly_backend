//! HTTP integration tests for `/coupons` endpoints.

mod common;

use chrono::{Duration, Utc};
use common::fixtures::seed_coupon;
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn validate_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/coupons/DREAMFLY100/validate").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn validate_valid_code_returns_200_with_exact_body(db: PgPool) {
    let app = spawn_test_app(db).await;
    seed_coupon(&app.db, "DREAMFLY100", 1500, true, None).await;
    let user = app.register_member("member-a@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/coupons/DREAMFLY100/validate")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body, json!({ "code": "DREAMFLY100", "discount_cents": 1500 }));
}

#[sqlx::test]
async fn validate_is_case_insensitive(db: PgPool) {
    let app = spawn_test_app(db).await;
    seed_coupon(&app.db, "DREAMFLY100", 1500, true, None).await;
    let user = app.register_member("member-b@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/coupons/dreamfly100/validate")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body, json!({ "code": "DREAMFLY100", "discount_cents": 1500 }));
}

#[sqlx::test]
async fn validate_expired_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    seed_coupon(&app.db, "EXPIRED10", 100, true, Some(Utc::now() - Duration::days(1))).await;
    let user = app.register_member("member-c@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/coupons/EXPIRED10/validate")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 404);
}

#[sqlx::test]
async fn validate_inactive_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    seed_coupon(&app.db, "DISABLED10", 100, false, None).await;
    let user = app.register_member("member-d@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/coupons/DISABLED10/validate")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 404);
}

#[sqlx::test]
async fn validate_nonexistent_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("member-e@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/coupons/NOSUCHCODE/validate")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 404);
}

#[sqlx::test]
async fn create_coupon_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/coupons")
        .json(&json!({ "code": "NEWCODE1", "discount_cents": 500 }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn create_coupon_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("member-f@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/coupons")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "code": "NEWCODE1", "discount_cents": 500 }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn create_coupon_as_admin_succeeds_and_normalizes(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/coupons")
        .authorization_bearer(&token)
        .json(&json!({ "code": "  newcode2  ", "discount_cents": 750 }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["code"], "NEWCODE2");
    assert_eq!(body["discount_cents"], 750);
    assert_eq!(body["is_active"], true);
    assert!(body["expires_at"].is_null());
    assert!(body["id"].is_string());
    assert!(body["created_at"].is_string());
}

#[sqlx::test]
async fn create_coupon_rejects_empty_code(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/coupons")
        .authorization_bearer(&token)
        .json(&json!({ "code": "", "discount_cents": 500 }))
        .await;
    assert_eq!(resp.status_code(), 422);
}

#[sqlx::test]
async fn create_coupon_rejects_non_positive_discount(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/coupons")
        .authorization_bearer(&token)
        .json(&json!({ "code": "ZERODISC", "discount_cents": 0 }))
        .await;
    assert_eq!(resp.status_code(), 422);
}

#[sqlx::test]
async fn create_coupon_duplicate_code_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;

    app.post("/api/v1/coupons")
        .authorization_bearer(&token)
        .json(&json!({ "code": "DUPCODE1", "discount_cents": 500 }))
        .await
        .assert_status_ok();

    let resp = app
        .post("/api/v1/coupons")
        .authorization_bearer(&token)
        .json(&json!({ "code": "dupcode1", "discount_cents": 999 }))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

#[sqlx::test]
async fn list_coupons_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/coupons").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn list_coupons_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("member-g@example.com", "Password!234").await;
    let resp = app
        .get("/api/v1/coupons")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn list_coupons_as_admin_paginates(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;

    for i in 0..3 {
        seed_coupon(&app.db, &format!("LISTCODE{i}"), 100, true, None).await;
    }

    let resp = app
        .get("/api/v1/coupons?page=1&per_page=2")
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["coupons"].as_array().unwrap().len(), 2);
    assert_eq!(body["total"], 3);
    assert_eq!(body["page"], 1);
    assert_eq!(body["per_page"], 2);
}
