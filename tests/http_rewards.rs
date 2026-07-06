//! HTTP integration tests for `/rewards/*` endpoints.

mod common;

use common::fixtures::{seed_reward, set_points_balance};
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// GET /rewards
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn list_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/rewards").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn list_member_sees_only_active_sorted_by_display_order(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("rewards-list-member@example.com", "Password!234").await;

    seed_reward(&app.db, "Second", 20, None, true, 1).await;
    seed_reward(&app.db, "First", 10, None, true, 0).await;
    seed_reward(&app.db, "Hidden", 5, None, false, 2).await;

    let resp = app
        .get("/api/v1/rewards")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let rewards = body["rewards"].as_array().unwrap();
    assert_eq!(rewards.len(), 2, "inactive reward must be excluded");
    assert_eq!(rewards[0]["name"], "First");
    assert_eq!(rewards[1]["name"], "Second");
}

#[sqlx::test]
async fn list_all_true_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("rewards-list-all-member@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/rewards?all=true")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn list_all_true_as_admin_includes_inactive(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;

    seed_reward(&app.db, "Active One", 10, None, true, 0).await;
    seed_reward(&app.db, "Inactive One", 20, None, false, 1).await;

    let resp = app
        .get("/api/v1/rewards?all=true")
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["rewards"].as_array().unwrap().len(), 2);
}

// ---------------------------------------------------------------------------
// POST /rewards/{id}/redeem
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn redeem_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let reward_id = seed_reward(&app.db, "Any Reward", 10, None, true, 0).await;
    let resp = app.post(&format!("/api/v1/rewards/{reward_id}/redeem")).await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn redeem_success_returns_expected_shape(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("rewards-redeem-ok@example.com", "Password!234").await;
    set_points_balance(&app.db, user.user_id, 100).await;
    let reward_id = seed_reward(&app.db, "Mug", 40, Some(5), true, 0).await;

    let resp = app
        .post(&format!("/api/v1/rewards/{reward_id}/redeem"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["redemption_id"].as_str().is_some());
    assert_eq!(body["points_spent"], 40);
    assert_eq!(body["balance_after"], 60);
}

#[sqlx::test]
async fn redeem_unknown_reward_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("rewards-redeem-404@example.com", "Password!234").await;

    let resp = app
        .post(&format!("/api/v1/rewards/{}/redeem", Uuid::now_v7()))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 404);
}

#[sqlx::test]
async fn redeem_insufficient_points_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("rewards-redeem-poor@example.com", "Password!234").await;
    set_points_balance(&app.db, user.user_id, 1).await;
    let reward_id = seed_reward(&app.db, "Expensive", 999, None, true, 0).await;

    let resp = app
        .post(&format!("/api/v1/rewards/{reward_id}/redeem"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["error"], "點數不足");
}

#[sqlx::test]
async fn redeem_zero_stock_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("rewards-redeem-oos@example.com", "Password!234").await;
    set_points_balance(&app.db, user.user_id, 1000).await;
    let reward_id = seed_reward(&app.db, "Sold Out", 10, Some(0), true, 0).await;

    let resp = app
        .post(&format!("/api/v1/rewards/{reward_id}/redeem"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["error"], "已兌換完畢");
}

// ---------------------------------------------------------------------------
// GET /rewards/redemptions/me
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn my_redemptions_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/rewards/redemptions/me").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn my_redemptions_lists_and_paginates(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("rewards-history@example.com", "Password!234").await;
    set_points_balance(&app.db, user.user_id, 1000).await;
    let reward_id = seed_reward(&app.db, "Repeatable", 10, None, true, 0).await;

    for _ in 0..3 {
        app.post(&format!("/api/v1/rewards/{reward_id}/redeem"))
            .authorization_bearer(&user.access_token)
            .await
            .assert_status_ok();
    }

    let resp = app
        .get("/api/v1/rewards/redemptions/me?page=1&per_page=2")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["redemptions"].as_array().unwrap().len(), 2);
    assert_eq!(body["total"], 3);
    assert_eq!(body["page"], 1);
    assert_eq!(body["per_page"], 2);
    assert_eq!(body["redemptions"][0]["reward_name"], "Repeatable");
}

// ---------------------------------------------------------------------------
// Admin CRUD: POST /rewards, PATCH /rewards/{id}
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn create_reward_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/rewards")
        .json(&json!({ "name": "New Reward", "points_cost": 10 }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn create_reward_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("rewards-create-member@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/rewards")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "name": "New Reward", "points_cost": 10 }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn create_reward_as_admin_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/rewards")
        .authorization_bearer(&token)
        .json(&json!({
            "name": "New Reward",
            "description": "A shiny thing",
            "points_cost": 25,
            "stock": 10,
            "display_order": 3
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["name"], "New Reward");
    assert_eq!(body["description"], "A shiny thing");
    assert_eq!(body["points_cost"], 25);
    assert_eq!(body["stock"], 10);
    assert_eq!(body["display_order"], 3);
    assert_eq!(body["is_active"], true);
    assert!(body["id"].as_str().is_some());
}

#[sqlx::test]
async fn create_reward_rejects_non_positive_points_cost(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/rewards")
        .authorization_bearer(&token)
        .json(&json!({ "name": "Bad Reward", "points_cost": 0 }))
        .await;
    assert_eq!(resp.status_code(), 422);
}

#[sqlx::test]
async fn update_reward_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let reward_id = seed_reward(&app.db, "Editable", 10, None, true, 0).await;
    let resp = app
        .patch(&format!("/api/v1/rewards/{reward_id}"))
        .json(&json!({ "name": "Renamed" }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn update_reward_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("rewards-update-member@example.com", "Password!234").await;
    let reward_id = seed_reward(&app.db, "Editable", 10, None, true, 0).await;

    let resp = app
        .patch(&format!("/api/v1/rewards/{reward_id}"))
        .authorization_bearer(&user.access_token)
        .json(&json!({ "name": "Renamed" }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn update_reward_as_admin_partial_update_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;
    let reward_id = seed_reward(&app.db, "Original Name", 10, Some(5), true, 0).await;

    let resp = app
        .patch(&format!("/api/v1/rewards/{reward_id}"))
        .authorization_bearer(&token)
        .json(&json!({ "is_active": false, "stock": 99 }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    // Untouched fields survive the partial update.
    assert_eq!(body["name"], "Original Name");
    assert_eq!(body["points_cost"], 10);
    // Touched fields are updated.
    assert_eq!(body["is_active"], false);
    assert_eq!(body["stock"], 99);
}

#[sqlx::test]
async fn update_reward_clears_stock_to_unlimited_via_explicit_null(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;
    let reward_id = seed_reward(&app.db, "Capped", 10, Some(5), true, 0).await;

    let resp = app
        .patch(&format!("/api/v1/rewards/{reward_id}"))
        .authorization_bearer(&token)
        .json(&json!({ "stock": null }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["stock"].is_null());
}

#[sqlx::test]
async fn update_reward_unknown_id_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;

    let resp = app
        .patch(&format!("/api/v1/rewards/{}", Uuid::now_v7()))
        .authorization_bearer(&token)
        .json(&json!({ "name": "Nope" }))
        .await;
    assert_eq!(resp.status_code(), 404);
}
