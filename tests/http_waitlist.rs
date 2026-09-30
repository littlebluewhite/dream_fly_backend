//! HTTP integration tests for `/waitlist*` endpoints.

mod common;

use chrono::{Duration, Utc};
use common::fixtures::{CourseSeed, seed_enrolment, seed_waitlist_entry};
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn join_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/waitlist")
        .json(&json!({ "course_id": Uuid::now_v7() }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn join_full_course_returns_200_with_waitlist_response(db: PgPool) {
    let app = spawn_test_app(db).await;
    let course_id = CourseSeed::new("HTTP Full Join Course").max_students(1).insert(&app.db).await;
    let filler = app
        .register_member("wl-http-filler@example.com", "Password!234")
        .await;
    seed_enrolment(&app.db, filler.user_id, course_id, "active", Utc::now()).await;

    let joiner = app
        .register_member("wl-http-joiner@example.com", "Password!234")
        .await;

    let resp = app
        .post("/api/v1/waitlist")
        .authorization_bearer(&joiner.access_token)
        .json(&json!({ "course_id": course_id }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["course_id"], course_id.to_string());
    assert_eq!(body["course_name"], "HTTP Full Join Course");
    assert_eq!(body["status"], "waiting");
    assert!(body["id"].as_str().is_some());
    assert!(body["created_at"].as_str().is_some());
}

#[sqlx::test]
async fn me_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/waitlist/me").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn me_returns_only_callers_entries_newest_first(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user_a = app
        .register_member("wl-me-a@example.com", "Password!234")
        .await;
    let user_b = app
        .register_member("wl-me-b@example.com", "Password!234")
        .await;

    let course_a = CourseSeed::new("HTTP WL Me Course A").max_students(10).insert(&app.db).await;
    let course_b = CourseSeed::new("HTTP WL Me Course B").max_students(10).insert(&app.db).await;

    // Someone else's entry must not leak into user_a's list.
    seed_waitlist_entry(&app.db, user_b.user_id, course_a, "waiting", Utc::now()).await;

    let older_id = seed_waitlist_entry(
        &app.db,
        user_a.user_id,
        course_a,
        "waiting",
        Utc::now() - Duration::days(2),
    )
    .await;
    let newer_id =
        seed_waitlist_entry(&app.db, user_a.user_id, course_b, "waiting", Utc::now()).await;

    let resp = app
        .get("/api/v1/waitlist/me")
        .authorization_bearer(&user_a.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array, not an envelope");
    assert_eq!(arr.len(), 2, "must not include other users' entries");
    assert_eq!(arr[0]["id"], newer_id.to_string(), "newest first");
    assert_eq!(arr[1]["id"], older_id.to_string());

    let first = &arr[0];
    assert_eq!(first["course_id"], course_b.to_string());
    assert_eq!(first["course_name"], "HTTP WL Me Course B");
    assert_eq!(first["status"], "waiting");
    assert!(first["created_at"].as_str().is_some());
}

#[sqlx::test]
async fn cancel_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .delete(&format!("/api/v1/waitlist/{}", Uuid::now_v7()))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn cancel_owner_succeeds_204(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("wl-cancel-owner@example.com", "Password!234")
        .await;
    let course_id =
        CourseSeed::new("WL Cancel Owner Course").max_students(10).insert(&app.db).await;
    let entry_id =
        seed_waitlist_entry(&app.db, user.user_id, course_id, "waiting", Utc::now()).await;

    let resp = app
        .delete(&format!("/api/v1/waitlist/{entry_id}"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 204, "body={}", resp.text());
}

#[sqlx::test]
async fn cancel_as_non_owner_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let owner = app
        .register_member("wl-cancel-owner2@example.com", "Password!234")
        .await;
    let other = app
        .register_member("wl-cancel-other@example.com", "Password!234")
        .await;
    let course_id =
        CourseSeed::new("WL Cancel Other Course").max_students(10).insert(&app.db).await;
    let entry_id =
        seed_waitlist_entry(&app.db, owner.user_id, course_id, "waiting", Utc::now()).await;

    let resp = app
        .delete(&format!("/api/v1/waitlist/{entry_id}"))
        .authorization_bearer(&other.access_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn cancel_as_admin_succeeds_204(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;
    let owner = app
        .register_member("wl-cancel-owner3@example.com", "Password!234")
        .await;
    let course_id =
        CourseSeed::new("WL Cancel Admin Course").max_students(10).insert(&app.db).await;
    let entry_id =
        seed_waitlist_entry(&app.db, owner.user_id, course_id, "waiting", Utc::now()).await;

    let resp = app
        .delete(&format!("/api/v1/waitlist/{entry_id}"))
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 204, "body={}", resp.text());
}

#[sqlx::test]
async fn cancel_already_cancelled_returns_404(db: PgPool) {
    // Deliberate deviation from enrolments (which returns 409 on a
    // double-cancel): a cancelled waitlist entry is treated as gone — it's
    // no longer addressable, and re-joining is the supported way back in
    // (the partial unique index only guards 'waiting' rows). See task plan.
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("wl-cancel-twice@example.com", "Password!234")
        .await;
    let course_id =
        CourseSeed::new("WL Cancel Twice Course").max_students(10).insert(&app.db).await;
    let entry_id =
        seed_waitlist_entry(&app.db, user.user_id, course_id, "cancelled", Utc::now()).await;

    let resp = app
        .delete(&format!("/api/v1/waitlist/{entry_id}"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 404, "body={}", resp.text());
}

#[sqlx::test]
async fn cancel_nonexistent_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("wl-cancel-404@example.com", "Password!234")
        .await;

    let resp = app
        .delete(&format!("/api/v1/waitlist/{}", Uuid::now_v7()))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 404, "body={}", resp.text());
}

#[sqlx::test]
async fn admin_list_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let course_id =
        CourseSeed::new("WL Admin No Auth Course").max_students(5).insert(&app.db).await;

    let resp = app
        .get(&format!("/api/v1/waitlist?course_id={course_id}"))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn admin_list_missing_course_id_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/waitlist")
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

#[sqlx::test]
async fn admin_list_as_non_admin_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("wl-admin-list-member@example.com", "Password!234")
        .await;
    let course_id = CourseSeed::new("WL Admin Member Course").max_students(5).insert(&app.db).await;

    let resp = app
        .get(&format!("/api/v1/waitlist?course_id={course_id}"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn admin_list_returns_waiting_only_oldest_first_for_course(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, token) = app.seed_admin().await;

    let course_x = CourseSeed::new("Admin Queue Course X").max_students(1).insert(&app.db).await;
    let course_y = CourseSeed::new("Admin Queue Course Y").max_students(1).insert(&app.db).await;

    let user_a = app
        .register_member("wl-admin-a@example.com", "Password!234")
        .await;
    let user_b = app
        .register_member("wl-admin-b@example.com", "Password!234")
        .await;
    let user_c = app
        .register_member("wl-admin-c@example.com", "Password!234")
        .await;
    let user_d = app
        .register_member("wl-admin-d@example.com", "Password!234")
        .await;

    // Oldest waiting entry for course_x.
    let older_id = seed_waitlist_entry(
        &app.db,
        user_a.user_id,
        course_x,
        "waiting",
        Utc::now() - Duration::hours(2),
    )
    .await;
    // Newer waiting entry for course_x.
    let newer_id = seed_waitlist_entry(
        &app.db,
        user_b.user_id,
        course_x,
        "waiting",
        Utc::now() - Duration::hours(1),
    )
    .await;
    // Cancelled entry for course_x — must be excluded.
    seed_waitlist_entry(&app.db, user_c.user_id, course_x, "cancelled", Utc::now()).await;
    // Waiting entry for a different course — must be excluded.
    seed_waitlist_entry(&app.db, user_d.user_id, course_y, "waiting", Utc::now()).await;

    let resp = app
        .get(&format!("/api/v1/waitlist?course_id={course_x}"))
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array");
    assert_eq!(
        arr.len(),
        2,
        "must only include waiting entries for course_x, got {arr:?}"
    );
    assert_eq!(
        arr[0]["id"],
        older_id.to_string(),
        "oldest first (queue order)"
    );
    assert_eq!(arr[1]["id"], newer_id.to_string());
}

// ---------------------------------------------------------------------------
// Pin: join response equals the read projection, field-for-field
// ---------------------------------------------------------------------------
//
// `POST /waitlist`'s response, the joiner's `GET /waitlist/me` row and the
// admin `GET /waitlist?course_id=` row are meant to be the exact same shape —
// `serde_json::Value` equality locks that before the read projection gets a
// single owner (mirrors `http_leave.rs`'s `*_response_matches_me_row` pins).

#[sqlx::test]
async fn join_response_matches_me_and_admin_rows(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = CourseSeed::new("WL Pin Course").max_students(1).insert(&app.db).await;
    let filler = app
        .register_member("wl-pin-filler@example.com", "Password!234")
        .await;
    seed_enrolment(&app.db, filler.user_id, course_id, "active", Utc::now()).await;
    let joiner = app
        .register_member("wl-pin-joiner@example.com", "Password!234")
        .await;

    let join_resp = app
        .post("/api/v1/waitlist")
        .authorization_bearer(&joiner.access_token)
        .json(&json!({ "course_id": course_id }))
        .await;
    assert_eq!(join_resp.status_code(), 200, "body={}", join_resp.text());
    let joined: serde_json::Value = join_resp.json();

    let me_rows: serde_json::Value = app
        .get("/api/v1/waitlist/me")
        .authorization_bearer(&joiner.access_token)
        .await
        .json();
    let me_row = me_rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == joined["id"])
        .expect("joined row present in /me");
    assert_eq!(&joined, me_row, "join response must equal its /me row");

    let admin_rows: serde_json::Value = app
        .get(&format!("/api/v1/waitlist?course_id={course_id}"))
        .authorization_bearer(&admin_token)
        .await
        .json();
    let admin_row = admin_rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == joined["id"])
        .expect("joined row present in admin list");
    assert_eq!(&joined, admin_row, "join response must equal its admin list row");
}
