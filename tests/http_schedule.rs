//! HTTP integration tests for `/schedule/*` endpoints.

mod common;

use chrono::{Datelike, Duration, Utc};
use common::fixtures::{seed_time_slot_full, seed_venue};
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn get_monthly_schedule_public(db: PgPool) {
    let app = spawn_test_app(db).await;
    let now = Utc::now();
    let resp = app
        .get(&format!(
            "/api/v1/schedule?year={}&month={}",
            now.year(),
            now.month()
        ))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    assert!(resp.json::<serde_json::Value>().is_array());
}

#[sqlx::test]
async fn get_monthly_schedule_missing_params_returns_400(db: PgPool) {
    let app = spawn_test_app(db).await;
    // Missing `year` and `month` → axum Query rejection is a 400.
    let resp = app.get("/api/v1/schedule").await;
    assert_eq!(resp.status_code(), 400);
}

#[sqlx::test]
async fn get_availability_returns_seeded_slot(db: PgPool) {
    let app = spawn_test_app(db).await;
    let id = seed_time_slot_full(&app.db, None, None, 10).await;

    let date = (Utc::now() + Duration::days(2)).date_naive();
    let resp = app
        .get(&format!("/api/v1/schedule/availability?date={date}"))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().unwrap();
    assert!(arr.iter().any(|s| s["id"].as_str().unwrap() == id.to_string()));
}

#[sqlx::test]
async fn create_slots_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/schedule/slots")
        .json(&json!({ "slots": [] }))
        .await;
    // Auth extractor runs before validation → 401.
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn create_slots_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("smem@example.com", "Password!234").await;
    let resp = app
        .post("/api/v1/schedule/slots")
        .authorization_bearer(&user.access_token)
        .json(&json!({
            "slots": [{
                "date": "2030-01-01",
                "start_time": "09:00",
                "end_time": "10:00",
                "capacity": 10,
            }]
        }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn create_slots_as_admin_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/schedule/slots")
        .authorization_bearer(&token)
        .json(&json!({
            "slots": [{
                "date": "2030-01-01",
                "start_time": "09:00",
                "end_time": "10:00",
                "capacity": 10,
            }]
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body.as_array().unwrap().len(), 1);
}

/// Task P4-B2: admin can specify a venue-rental `price_cents` when creating
/// a slot; omitting it (covered by `create_slots_as_admin_succeeds` above)
/// defaults to `0`.
#[sqlx::test]
async fn create_slots_as_admin_with_price_cents_persists_it(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/schedule/slots")
        .authorization_bearer(&token)
        .json(&json!({
            "slots": [{
                "date": "2030-01-01",
                "start_time": "09:00",
                "end_time": "10:00",
                "capacity": 10,
                "price_cents": 50000,
            }]
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body[0]["price_cents"], 50000);
}

/// Phase 1 (EXCLUDE → 409): a new slot on the same venue, same date, with a
/// time range overlapping an already-existing slot violates
/// `time_slots_venue_no_overlap` (SQLSTATE 23P01). Before the
/// `IntoResponse`/`conflict_on_exclusion` fix this surfaced as a bare 500.
#[sqlx::test]
async fn create_slots_overlapping_existing_venue_slot_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let venue_id = seed_venue(&app.db, "Overlap Venue", None).await;
    // `seed_time_slot_full` always seeds today + 2 days, 10:00-11:00.
    seed_time_slot_full(&app.db, None, Some(venue_id), 10).await;
    let date = (Utc::now() + Duration::days(2)).date_naive();

    let resp = app
        .post("/api/v1/schedule/slots")
        .authorization_bearer(&token)
        .json(&json!({
            "slots": [{
                "date": date.to_string(),
                "start_time": "10:30",
                "end_time": "11:30",
                "venue_id": venue_id,
                "capacity": 10,
            }]
        }))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

// ---------------------------------------------------------------------
// Step 8: SlotStatus 讀時推導 + closed gate
// ---------------------------------------------------------------------

/// Admin sets `is_closed=true` via `PATCH /schedule/slots/{id}` — the
/// monthly schedule immediately reads it back as `status="closed"` (derived
/// at read time, no separate stored-status sync step to forget).
#[sqlx::test]
async fn admin_closes_slot_then_monthly_shows_closed_status(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    // `seed_time_slot_full` always seeds today + 2 days, 10:00-11:00.
    let slot_id = seed_time_slot_full(&app.db, None, None, 10).await;

    let patch_resp = app
        .patch(&format!("/api/v1/schedule/slots/{slot_id}"))
        .authorization_bearer(&token)
        .json(&json!({ "is_closed": true }))
        .await;
    assert_eq!(patch_resp.status_code(), 200, "body={}", patch_resp.text());
    assert_eq!(patch_resp.json::<serde_json::Value>()["status"], "closed");

    let target_date = (Utc::now() + Duration::days(2)).date_naive();
    let resp = app
        .get(&format!(
            "/api/v1/schedule?year={}&month={}",
            target_date.year(),
            target_date.month()
        ))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let slot = body
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|d| d["slots"].as_array().unwrap())
        .find(|s| s["id"].as_str().unwrap() == slot_id.to_string())
        .expect("closed slot present in monthly schedule");
    assert_eq!(slot["status"], "closed");
}

/// Non-admin members can't flip the closed flag — same `admin_router()` gate
/// as `create_slots_as_member_returns_403` above.
#[sqlx::test]
async fn update_slot_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("smem2@example.com", "Password!234").await;
    let slot_id = seed_time_slot_full(&app.db, None, None, 10).await;

    let resp = app
        .patch(&format!("/api/v1/schedule/slots/{slot_id}"))
        .authorization_bearer(&user.access_token)
        .json(&json!({ "is_closed": true }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

