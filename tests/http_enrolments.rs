//! HTTP integration tests for `/enrolments/*` endpoints.

mod common;

use chrono::{Duration, Utc};
use common::fixtures::{seed_course_with_capacity, seed_enrolment};
use common::http::spawn_test_app;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn me_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/enrolments/me").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn me_returns_only_callers_enrolments_with_course_fields_newest_first(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user_a = app.register_member("enr-me-a@example.com", "Password!234").await;
    let user_b = app.register_member("enr-me-b@example.com", "Password!234").await;

    // Two distinct courses so user_a can hold two *active* enrolments
    // without tripping the partial unique index (one active row per
    // user+course).
    let course_a = seed_course_with_capacity(&app.db, "HTTP Me Course A", None, 10).await;
    let course_b = seed_course_with_capacity(&app.db, "HTTP Me Course B", None, 10).await;

    // Someone else's enrolment must not leak into user_a's list.
    seed_enrolment(&app.db, user_b.user_id, course_a, "active", Utc::now()).await;

    let older_id = seed_enrolment(
        &app.db,
        user_a.user_id,
        course_a,
        "active",
        Utc::now() - Duration::days(2),
    )
    .await;
    let newer_id =
        seed_enrolment(&app.db, user_a.user_id, course_b, "active", Utc::now()).await;

    let resp = app
        .get("/api/v1/enrolments/me")
        .authorization_bearer(&user_a.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array, not an envelope");
    assert_eq!(arr.len(), 2, "must not include other users' enrolments");
    assert_eq!(arr[0]["id"], newer_id.to_string(), "newest first");
    assert_eq!(arr[1]["id"], older_id.to_string());

    let first = &arr[0];
    assert_eq!(first["course_id"], course_b.to_string());
    assert_eq!(first["course_name"], "HTTP Me Course B");
    assert_eq!(first["course_level"], "beginner");
    assert_eq!(first["schedule_text"], "Mon/Wed 19:00");
    assert_eq!(first["status"], "active");
    assert!(first["enrolled_at"].as_str().is_some());
}

#[sqlx::test]
async fn cancel_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .patch(&format!("/api/v1/enrolments/{}/cancel", Uuid::now_v7()))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn cancel_owner_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("enr-cancel-owner@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Cancel Owner Course", None, 10).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .patch(&format!("/api/v1/enrolments/{enrolment_id}/cancel"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["status"], "cancelled");
    assert_eq!(body["id"], enrolment_id.to_string());
}

#[sqlx::test]
async fn cancel_as_non_owner_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let owner = app.register_member("enr-cancel-owner2@example.com", "Password!234").await;
    let other = app.register_member("enr-cancel-other@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Cancel Other Course", None, 10).await;
    let enrolment_id =
        seed_enrolment(&app.db, owner.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .patch(&format!("/api/v1/enrolments/{enrolment_id}/cancel"))
        .authorization_bearer(&other.access_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn cancel_as_admin_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;
    let owner = app.register_member("enr-cancel-owner3@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Cancel Admin Course", None, 10).await;
    let enrolment_id =
        seed_enrolment(&app.db, owner.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .patch(&format!("/api/v1/enrolments/{enrolment_id}/cancel"))
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    assert_eq!(resp.json::<serde_json::Value>()["status"], "cancelled");
}

#[sqlx::test]
async fn cancel_already_cancelled_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("enr-cancel-twice@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Cancel Twice Course", None, 10).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "cancelled", Utc::now()).await;

    let resp = app
        .patch(&format!("/api/v1/enrolments/{enrolment_id}/cancel"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

#[sqlx::test]
async fn cancel_nonexistent_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("enr-cancel-404@example.com", "Password!234").await;

    let resp = app
        .patch(&format!("/api/v1/enrolments/{}/cancel", Uuid::now_v7()))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 404, "body={}", resp.text());
}
