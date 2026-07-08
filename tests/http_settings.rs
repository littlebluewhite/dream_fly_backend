//! HTTP integration tests for `/settings` endpoints (Round 4 Task B6) — a
//! flat, admin-only global key-value store backing the admin/mobile-admin
//! "system settings" pages.

mod common;

use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;

// ---------------------------------------------------------------------------
// GET /settings
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn get_settings_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/settings").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn get_settings_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("settings-member@example.com", "Password!234")
        .await;
    let resp = app
        .get("/api/v1/settings")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn get_settings_as_admin_on_empty_table_returns_empty_object(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/settings")
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["settings"], json!({}), "empty table must yield {{}}, not 500");
}

// ---------------------------------------------------------------------------
// PUT /settings
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn update_settings_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .put("/api/v1/settings")
        .json(&json!({ "settings": { "security": { "twoFA": true } } }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn update_settings_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("settings-member2@example.com", "Password!234")
        .await;
    let resp = app
        .put("/api/v1/settings")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "settings": { "security": { "twoFA": true } } }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn update_settings_as_admin_creates_new_key(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .put("/api/v1/settings")
        .authorization_bearer(&token)
        .json(&json!({
            "settings": {
                "studio_profile": {
                    "name": "Dream Fly",
                    "phone": "0912345678",
                    "address": "Taipei",
                    "default_ratio": "1:8",
                    "max_class_size": 12
                }
            }
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["settings"]["studio_profile"]["name"], "Dream Fly");
    assert_eq!(body["settings"]["studio_profile"]["max_class_size"], 12);
}

#[sqlx::test]
async fn update_settings_overwrites_existing_key_and_bumps_updated_at(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    app.put("/api/v1/settings")
        .authorization_bearer(&token)
        .json(&json!({ "settings": { "security": { "twoFA": false } } }))
        .await;

    let first_updated_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT updated_at FROM settings WHERE key = 'security'")
            .fetch_one(&app.db)
            .await
            .unwrap();

    // Visible clock tick between writes so `updated_at` provably advances,
    // not just coincidentally re-written with an equal timestamp.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let resp = app
        .put("/api/v1/settings")
        .authorization_bearer(&token)
        .json(&json!({ "settings": { "security": { "twoFA": true } } }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["settings"]["security"]["twoFA"], true);

    let second_updated_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT updated_at FROM settings WHERE key = 'security'")
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(
        second_updated_at > first_updated_at,
        "updated_at must advance on overwrite (first={first_updated_at}, second={second_updated_at})"
    );
}

#[sqlx::test]
async fn update_settings_partial_update_does_not_affect_other_keys(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    app.put("/api/v1/settings")
        .authorization_bearer(&token)
        .json(&json!({
            "settings": {
                "studio_profile": { "name": "A" },
                "notification_flags": { "email": true, "sms": false }
            }
        }))
        .await;

    // Second PUT only touches `studio_profile` — `notification_flags` must
    // survive untouched.
    let resp = app
        .put("/api/v1/settings")
        .authorization_bearer(&token)
        .json(&json!({ "settings": { "studio_profile": { "name": "B" } } }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["settings"]["studio_profile"]["name"], "B");
    assert_eq!(body["settings"]["notification_flags"]["email"], true);
    assert_eq!(body["settings"]["notification_flags"]["sms"], false);
}

#[sqlx::test]
async fn update_settings_nested_json_object_round_trips(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let nested = json!({
        "name": "夢飛體操",
        "contact": {
            "phone": "0912345678",
            "address": { "city": "Taipei", "district": "Da'an" }
        },
        "class_ratios": [1, 8]
    });

    let put_resp = app
        .put("/api/v1/settings")
        .authorization_bearer(&token)
        .json(&json!({ "settings": { "studio_profile": nested } }))
        .await;
    assert_eq!(put_resp.status_code(), 200, "body={}", put_resp.text());
    let put_body: serde_json::Value = put_resp.json();
    assert_eq!(put_body["settings"]["studio_profile"], nested);

    // Re-read via GET (separate read path) to prove it's really persisted,
    // not merely echoed back from the request body.
    let get_resp = app
        .get("/api/v1/settings")
        .authorization_bearer(&token)
        .await;
    let get_body: serde_json::Value = get_resp.json();
    assert_eq!(get_body["settings"]["studio_profile"], nested);
}

#[sqlx::test]
async fn update_settings_empty_map_is_noop_and_returns_current_state(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    app.put("/api/v1/settings")
        .authorization_bearer(&token)
        .json(&json!({ "settings": { "security": { "twoFA": true } } }))
        .await;

    // Empty `settings` map: no-op, not a 400 (see docs/api/integration-
    // contract.md §3.25 and modules/settings/service.rs doc comment for the
    // rationale) — existing keys must be returned unchanged.
    let resp = app
        .put("/api/v1/settings")
        .authorization_bearer(&token)
        .json(&json!({ "settings": {} }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(
        body["settings"]["security"]["twoFA"], true,
        "existing keys must survive an empty-map PUT"
    );
}
