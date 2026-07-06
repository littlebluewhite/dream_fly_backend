//! HTTP integration tests for `/courses/*` endpoints.

mod common;

use common::fixtures::{seed_course, seed_course_schedule_slot};
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn list_courses_is_public_and_empty_initially(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/courses").await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    assert!(body["courses"].as_array().unwrap().is_empty());
    assert_eq!(body["total"], 0);
}

#[sqlx::test]
async fn list_courses_returns_seeded(db: PgPool) {
    let app = spawn_test_app(db).await;
    seed_course(&app.db, "Intro Flow", None).await;

    let resp = app.get("/api/v1/courses").await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    assert_eq!(body["courses"].as_array().unwrap().len(), 1);
    assert_eq!(body["courses"][0]["name"], "Intro Flow");
    assert_eq!(body["total"], 1);
    // No enrolments/waitlist_entries rows exist yet (tables land empty until
    // Task 9 wires checkout), so the computed counts must both be 0.
    assert_eq!(body["courses"][0]["enrolled_count"], 0);
    assert_eq!(body["courses"][0]["waitlist_count"], 0);
    assert!(body["courses"][0]["category"].is_null());
    assert!(body["courses"][0]["schedule_text"].is_null());
    assert_eq!(body["courses"][0]["is_highlighted"], false);
}

#[sqlx::test]
async fn get_course_by_slug_returns_detail(db: PgPool) {
    let app = spawn_test_app(db).await;
    let id = seed_course(&app.db, "Intro Flow", None).await;

    // Lookup by UUID.
    let resp = app.get(&format!("/api/v1/courses/{id}")).await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    assert_eq!(body["id"].as_str().unwrap(), id.to_string());
    assert_eq!(body["enrolled_count"], 0);
    assert_eq!(body["waitlist_count"], 0);
}

#[sqlx::test]
async fn get_course_unknown_slug_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/courses/no-such-slug").await;
    assert_eq!(resp.status_code(), 404);
}

#[sqlx::test]
async fn create_course_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;

    let resp = app
        .post("/api/v1/courses")
        .json(&json!({
            "name": "Advanced",
            "level": "advanced",
            "duration_minutes": 60,
            "price_cents": 100000,
            "max_students": 8,
        }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn create_course_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("mem-c@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/courses")
        .authorization_bearer(&user.access_token)
        .json(&json!({
            "name": "Advanced",
            "level": "advanced",
            "duration_minutes": 60,
            "price_cents": 100000,
            "max_students": 8,
        }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn create_course_as_admin_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/courses")
        .authorization_bearer(&admin_token)
        .json(&json!({
            "name": "Advanced",
            "level": "advanced",
            "duration_minutes": 60,
            "price_cents": 100000,
            "max_students": 8,
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["name"], "Advanced");
    assert_eq!(body["level"], "advanced");
}

#[sqlx::test]
async fn create_course_as_admin_can_set_category_schedule_and_highlight(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/courses")
        .authorization_bearer(&admin_token)
        .json(&json!({
            "name": "Tumbling Basics",
            "level": "beginner",
            "duration_minutes": 60,
            "price_cents": 80000,
            "max_students": 10,
            "category": "體操",
            "schedule_text": "週三 19:00-20:00",
            "is_highlighted": true,
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["category"], "體操");
    assert_eq!(body["schedule_text"], "週三 19:00-20:00");
    assert_eq!(body["is_highlighted"], true);
    assert_eq!(body["enrolled_count"], 0);
    assert_eq!(body["waitlist_count"], 0);
}

#[sqlx::test]
async fn update_course_as_admin_can_set_category_schedule_and_highlight(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let id = seed_course(&app.db, "Intro Flow", None).await;

    let resp = app
        .patch(&format!("/api/v1/courses/{id}"))
        .authorization_bearer(&admin_token)
        .json(&json!({
            "category": "跳床",
            "schedule_text": "週五 18:00-19:00",
            "is_highlighted": true,
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["category"], "跳床");
    assert_eq!(body["schedule_text"], "週五 18:00-19:00");
    assert_eq!(body["is_highlighted"], true);
}

#[sqlx::test]
async fn update_course_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let id = seed_course(&app.db, "Intro Flow", None).await;
    let user = app.register_member("m-upd-c@example.com", "Password!234").await;

    let resp = app
        .patch(&format!("/api/v1/courses/{id}"))
        .authorization_bearer(&user.access_token)
        .json(&json!({ "name": "Member Rename Attempt" }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn create_course_rejects_invalid_payload(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/courses")
        .authorization_bearer(&admin_token)
        .json(&json!({
            // name missing
            "level": "advanced",
            "duration_minutes": 0,          // below min
            "price_cents": -1,              // below min
            "max_students": 0,              // below min
        }))
        .await;
    // Validator or JSON deserialization error — either way, rejected.
    assert!(matches!(resp.status_code().as_u16(), 400 | 422));
}

// ---------------------------------------------------------------------------
// schedule_slots (Round 3 Task 1) — detail-only exposure, full-replace PATCH
// semantics, and the "field absent = untouched" contract.
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn create_course_with_schedule_slots_returns_detail_with_slots(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/courses")
        .authorization_bearer(&admin_token)
        .json(&json!({
            "name": "Slots Course",
            "level": "beginner",
            "duration_minutes": 60,
            "price_cents": 100000,
            "max_students": 10,
            "schedule_slots": [
                { "day_of_week": 2, "start_time": "16:00", "end_time": "17:00" },
                { "day_of_week": 4, "start_time": "16:00", "end_time": "17:00", "venue": "Floor Zone" }
            ]
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let slots = body["schedule_slots"].as_array().expect("schedule_slots array");
    assert_eq!(slots.len(), 2);
    assert_eq!(slots[0]["day_of_week"], 2);
    assert_eq!(slots[0]["start_time"], "16:00:00");
    assert_eq!(slots[0]["end_time"], "17:00:00");
    assert!(slots[0]["venue"].is_null());
    assert_eq!(slots[1]["day_of_week"], 4);
    assert_eq!(slots[1]["venue"], "Floor Zone");
    assert!(slots[0]["id"].as_str().is_some());
}

#[sqlx::test]
async fn get_course_detail_includes_schedule_slots(db: PgPool) {
    let app = spawn_test_app(db).await;
    let course_id = seed_course(&app.db, "Detail Slots Course", None).await;
    seed_course_schedule_slot(
        &app.db,
        course_id,
        6,
        chrono::NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
        chrono::NaiveTime::from_hms_opt(11, 30, 0).unwrap(),
    )
    .await;

    let resp = app.get(&format!("/api/v1/courses/{course_id}")).await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let slots = body["schedule_slots"].as_array().expect("schedule_slots array");
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0]["day_of_week"], 6);
    assert_eq!(slots[0]["start_time"], "10:00:00");
    assert_eq!(slots[0]["end_time"], "11:30:00");
}

#[sqlx::test]
async fn list_courses_does_not_include_schedule_slots(db: PgPool) {
    let app = spawn_test_app(db).await;
    let course_id = seed_course(&app.db, "List No Slots Course", None).await;
    seed_course_schedule_slot(
        &app.db,
        course_id,
        1,
        chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        chrono::NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
    )
    .await;

    let resp = app.get("/api/v1/courses").await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let course = &body["courses"][0];
    assert!(
        course.get("schedule_slots").is_none(),
        "list endpoint must not include schedule_slots (avoids N+1), got {course:?}"
    );
}

#[sqlx::test]
async fn patch_course_schedule_slots_replaces_entire_set(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Replace Slots Course", None).await;
    seed_course_schedule_slot(
        &app.db,
        course_id,
        1,
        chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        chrono::NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
    )
    .await;

    let resp = app
        .patch(&format!("/api/v1/courses/{course_id}"))
        .authorization_bearer(&admin_token)
        .json(&json!({
            "schedule_slots": [
                { "day_of_week": 5, "start_time": "18:00", "end_time": "19:00" }
            ]
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let slots = body["schedule_slots"].as_array().expect("schedule_slots array");
    assert_eq!(slots.len(), 1, "old slot must be gone, not merged with the new one");
    assert_eq!(slots[0]["day_of_week"], 5);
    assert_eq!(slots[0]["start_time"], "18:00:00");
}

#[sqlx::test]
async fn patch_course_without_schedule_slots_field_leaves_slots_untouched(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Untouched Slots Course", None).await;
    seed_course_schedule_slot(
        &app.db,
        course_id,
        3,
        chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        chrono::NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
    )
    .await;

    let resp = app
        .patch(&format!("/api/v1/courses/{course_id}"))
        .authorization_bearer(&admin_token)
        .json(&json!({ "name": "Renamed, No Slots Field" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let slots = body["schedule_slots"].as_array().expect("schedule_slots array");
    assert_eq!(slots.len(), 1, "omitting schedule_slots must leave existing slots untouched");
    assert_eq!(slots[0]["day_of_week"], 3);
}

#[sqlx::test]
async fn patch_course_schedule_slots_invalid_day_of_week_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Invalid Slot Course", None).await;

    let resp = app
        .patch(&format!("/api/v1/courses/{course_id}"))
        .authorization_bearer(&admin_token)
        .json(&json!({
            "schedule_slots": [
                { "day_of_week": 9, "start_time": "18:00", "end_time": "19:00" }
            ]
        }))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

#[sqlx::test]
async fn patch_course_schedule_slots_end_before_start_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Backwards Slot Course", None).await;

    let resp = app
        .patch(&format!("/api/v1/courses/{course_id}"))
        .authorization_bearer(&admin_token)
        .json(&json!({
            "schedule_slots": [
                { "day_of_week": 1, "start_time": "19:00", "end_time": "18:00" }
            ]
        }))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}
