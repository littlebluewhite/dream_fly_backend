//! HTTP integration tests for the leave module's endpoints:
//! `POST /leave-requests`, `GET /leave-requests/me`, `DELETE
//! /leave-requests/{id}`, `GET /leave-requests`, `PATCH /leave-requests/{id}`,
//! `POST /leave-requests/{id}/makeup`.
//!
//! The concurrent double-makeup race (`service::book_makeup` called twice for
//! the same leave request) lives in `tests/service_leave.rs` instead — it
//! needs direct `service::` access with `tokio::spawn`, mirroring
//! `service_enrolments.rs`/`service_bookings.rs`'s pattern for the same kind
//! of test, which this repo doesn't do through the HTTP/axum_test layer.

mod common;

use chrono::{Duration, NaiveTime, Utc};
use common::fixtures::{
    seed_coach, seed_course, seed_course_session, seed_course_with_capacity, seed_enrolment,
    seed_leave_request,
};
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

fn tomorrow() -> chrono::NaiveDate {
    (Utc::now() + Duration::days(1)).date_naive()
}

fn yesterday() -> chrono::NaiveDate {
    (Utc::now() - Duration::days(1)).date_naive()
}

// ---------------------------------------------------------------------------
// POST /leave-requests
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn create_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/leave-requests")
        .json(&json!({"session_id": Uuid::now_v7()}))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn create_success_for_future_session(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-create-ok@example.com", "Password!234").await;
    let course_id = seed_course(&app.db, "Leave Create Course", None).await;
    let session_id = seed_course_session(&app.db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .post("/api/v1/leave-requests")
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": session_id, "reason": "感冒"}))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["session_id"], session_id.to_string());
    assert_eq!(body["course_id"], course_id.to_string());
    assert_eq!(body["course_name"], "Leave Create Course");
    assert_eq!(body["status"], "pending");
    assert_eq!(body["reason"], "感冒");
    assert!(body["makeup_session_id"].is_null());
    assert!(body["id"].as_str().is_some());
}

#[sqlx::test]
async fn create_unknown_session_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-unknown-session@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/leave-requests")
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": Uuid::now_v7()}))
        .await;
    assert_eq!(resp.status_code(), 404, "body={}", resp.text());
}

#[sqlx::test]
async fn create_not_enrolled_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-not-enrolled@example.com", "Password!234").await;
    let course_id = seed_course(&app.db, "Leave Not Enrolled Course", None).await;
    let session_id = seed_course_session(&app.db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    // Deliberately no enrolment seeded for `user`.

    let resp = app
        .post("/api/v1/leave-requests")
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": session_id}))
        .await;
    assert_eq!(resp.status_code(), 404, "body={}", resp.text());
}

#[sqlx::test]
async fn create_session_already_started_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-started@example.com", "Password!234").await;
    let course_id = seed_course(&app.db, "Leave Started Course", None).await;
    let session_id = seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .post("/api/v1/leave-requests")
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": session_id}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

#[sqlx::test]
async fn create_duplicate_active_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-dup@example.com", "Password!234").await;
    let course_id = seed_course(&app.db, "Leave Dup Course", None).await;
    let session_id = seed_course_session(&app.db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;
    seed_leave_request(&app.db, enrolment_id, session_id, "pending").await;

    let resp = app
        .post("/api/v1/leave-requests")
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": session_id}))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

#[sqlx::test]
async fn create_after_prior_cancelled_request_succeeds(db: PgPool) {
    // The partial unique index only blocks `pending`/`approved` — a prior
    // `cancelled` request for the same (enrolment, session) must not block
    // re-applying.
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-recreate@example.com", "Password!234").await;
    let course_id = seed_course(&app.db, "Leave Recreate Course", None).await;
    let session_id = seed_course_session(&app.db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;
    seed_leave_request(&app.db, enrolment_id, session_id, "cancelled").await;

    let resp = app
        .post("/api/v1/leave-requests")
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": session_id}))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
}

// ---------------------------------------------------------------------------
// GET /leave-requests/me
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn me_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/leave-requests/me").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn me_returns_joined_fields_including_makeup(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-me@example.com", "Password!234").await;
    let course_id = seed_course(&app.db, "Leave Me Course", None).await;
    let session_date = tomorrow();
    let makeup_date = (Utc::now() + Duration::days(8)).date_naive();
    let session_id = seed_course_session(&app.db, course_id, session_date, t(9, 0), t(10, 0)).await;
    let makeup_session_id =
        seed_course_session(&app.db, course_id, makeup_date, t(14, 0), t(15, 0)).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "approved").await;
    sqlx::query("UPDATE leave_requests SET makeup_session_id = $2 WHERE id = $1")
        .bind(leave_id)
        .bind(makeup_session_id)
        .execute(&app.db)
        .await
        .expect("set makeup_session_id");

    let resp = app
        .get("/api/v1/leave-requests/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array, not an envelope");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], leave_id.to_string());
    assert_eq!(arr[0]["course_id"], course_id.to_string());
    assert_eq!(arr[0]["course_name"], "Leave Me Course");
    assert_eq!(arr[0]["session_date"], session_date.to_string());
    assert_eq!(arr[0]["start_time"], "09:00:00");
    assert_eq!(arr[0]["status"], "approved");
    assert_eq!(arr[0]["makeup_session_id"], makeup_session_id.to_string());
    assert_eq!(arr[0]["makeup_session_date"], makeup_date.to_string());
    assert_eq!(arr[0]["makeup_start_time"], "14:00:00");
}

// ---------------------------------------------------------------------------
// DELETE /leave-requests/{id}
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn cancel_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.delete(&format!("/api/v1/leave-requests/{}", Uuid::now_v7())).await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn cancel_pending_by_owner_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-cancel-ok@example.com", "Password!234").await;
    let course_id = seed_course(&app.db, "Leave Cancel Course", None).await;
    let session_id = seed_course_session(&app.db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "pending").await;

    let resp = app
        .delete(&format!("/api/v1/leave-requests/{leave_id}"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 204, "body={}", resp.text());

    let status: String =
        sqlx::query_scalar("SELECT status::text FROM leave_requests WHERE id = $1")
            .bind(leave_id)
            .fetch_one(&app.db)
            .await
            .expect("fetch status");
    assert_eq!(status, "cancelled");
}

#[sqlx::test]
async fn cancel_non_pending_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-cancel-409@example.com", "Password!234").await;
    let course_id = seed_course(&app.db, "Leave Cancel 409 Course", None).await;
    let session_id = seed_course_session(&app.db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "approved").await;

    let resp = app
        .delete(&format!("/api/v1/leave-requests/{leave_id}"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

#[sqlx::test]
async fn cancel_by_non_owner_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let owner = app.register_member("leave-cancel-owner@example.com", "Password!234").await;
    let other = app.register_member("leave-cancel-other@example.com", "Password!234").await;
    let course_id = seed_course(&app.db, "Leave Cancel Owner Course", None).await;
    let session_id = seed_course_session(&app.db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    let enrolment_id =
        seed_enrolment(&app.db, owner.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "pending").await;

    let resp = app
        .delete(&format!("/api/v1/leave-requests/{leave_id}"))
        .authorization_bearer(&other.access_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

// ---------------------------------------------------------------------------
// GET /leave-requests?status=&course_id= (coach/admin)
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn list_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/leave-requests").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn list_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-list-member@example.com", "Password!234").await;
    let resp = app
        .get("/api/v1/leave-requests")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn list_as_admin_returns_all_and_supports_filters(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let course_a = seed_course(&app.db, "Leave List Course A", None).await;
    let course_b = seed_course(&app.db, "Leave List Course B", None).await;
    let session_a = seed_course_session(&app.db, course_a, tomorrow(), t(9, 0), t(10, 0)).await;
    let session_b = seed_course_session(&app.db, course_b, tomorrow(), t(9, 0), t(10, 0)).await;

    let user_a = app.register_member("leave-list-a@example.com", "Password!234").await;
    let user_b = app.register_member("leave-list-b@example.com", "Password!234").await;
    let enrolment_a =
        seed_enrolment(&app.db, user_a.user_id, course_a, "active", Utc::now()).await;
    let enrolment_b =
        seed_enrolment(&app.db, user_b.user_id, course_b, "active", Utc::now()).await;
    seed_leave_request(&app.db, enrolment_a, session_a, "pending").await;
    seed_leave_request(&app.db, enrolment_b, session_b, "approved").await;

    // No filter: admin sees both.
    let resp = app
        .get("/api/v1/leave-requests")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["total"], 2);
    assert_eq!(body["page"], 1);
    assert!(body["per_page"].as_u64().unwrap() >= 2);
    assert_eq!(body["leave_requests"].as_array().unwrap().len(), 2);

    // status filter narrows to one.
    let resp = app
        .get("/api/v1/leave-requests?status=approved")
        .authorization_bearer(&admin_token)
        .await;
    let body: serde_json::Value = resp.json();
    let arr = body["leave_requests"].as_array().unwrap();
    assert_eq!(arr.len(), 1, "status filter must narrow to the approved one");
    assert_eq!(arr[0]["course_id"], course_b.to_string());
    assert_eq!(arr[0]["user_name"], "Test Member");

    // course_id filter narrows to the other one.
    let resp = app
        .get(&format!("/api/v1/leave-requests?course_id={course_a}"))
        .authorization_bearer(&admin_token)
        .await;
    let body: serde_json::Value = resp.json();
    let arr = body["leave_requests"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["course_id"], course_a.to_string());
}

#[sqlx::test]
async fn list_as_coach_scoped_to_own_courses(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_a_user, coach_a_token) =
        app.seed_user_with_roles("leave-list-coach-a@example.com", &["coach"]).await;
    let coach_a_id = seed_coach(&app.db, coach_a_user, "Coach A").await;
    let coach_b_user =
        common::seed_member(&app.db, "leave-list-coach-b@example.com", "Password!234").await;
    let coach_b_id = seed_coach(&app.db, coach_b_user, "Coach B").await;

    let course_a = seed_course(&app.db, "Leave List Coach Course A", Some(coach_a_id)).await;
    let course_b = seed_course(&app.db, "Leave List Coach Course B", Some(coach_b_id)).await;
    let session_a = seed_course_session(&app.db, course_a, tomorrow(), t(9, 0), t(10, 0)).await;
    let session_b = seed_course_session(&app.db, course_b, tomorrow(), t(9, 0), t(10, 0)).await;

    let user_a = app.register_member("leave-list-student-a@example.com", "Password!234").await;
    let user_b = app.register_member("leave-list-student-b@example.com", "Password!234").await;
    let enrolment_a =
        seed_enrolment(&app.db, user_a.user_id, course_a, "active", Utc::now()).await;
    let enrolment_b =
        seed_enrolment(&app.db, user_b.user_id, course_b, "active", Utc::now()).await;
    seed_leave_request(&app.db, enrolment_a, session_a, "pending").await;
    seed_leave_request(&app.db, enrolment_b, session_b, "pending").await;

    let resp = app
        .get("/api/v1/leave-requests")
        .authorization_bearer(&coach_a_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body["leave_requests"].as_array().unwrap();
    assert_eq!(arr.len(), 1, "coach must only see their own course's requests");
    assert_eq!(arr[0]["course_id"], course_a.to_string());
    assert_eq!(body["total"], 1);
}

// ---------------------------------------------------------------------------
// PATCH /leave-requests/{id}
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn decide_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .patch(&format!("/api/v1/leave-requests/{}", Uuid::now_v7()))
        .json(&json!({"status": "approved"}))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn decide_approve_writes_attendance_leave_and_notification(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) =
        app.seed_user_with_roles("leave-decide-coach@example.com", &["coach"]).await;
    let coach_id = seed_coach(&app.db, coach_user_id, "Decide Coach").await;
    let course_id = seed_course(&app.db, "Leave Decide Course", Some(coach_id)).await;
    let session_id = seed_course_session(&app.db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    let member = app.register_member("leave-decide-member@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "pending").await;

    let resp = app
        .patch(&format!("/api/v1/leave-requests/{leave_id}"))
        .authorization_bearer(&coach_token)
        .json(&json!({"status": "approved"}))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["status"], "approved");
    assert!(body["decided_at"].as_str().is_some());

    // leave_requests row updated.
    let status: String =
        sqlx::query_scalar("SELECT status::text FROM leave_requests WHERE id = $1")
            .bind(leave_id)
            .fetch_one(&app.db)
            .await
            .expect("fetch status");
    assert_eq!(status, "approved");

    // attendance_records row written with status = 'leave'.
    let att_status: String = sqlx::query_scalar(
        "SELECT status::text FROM attendance_records WHERE session_id = $1 AND enrolment_id = $2",
    )
    .bind(session_id)
    .bind(enrolment_id)
    .fetch_one(&app.db)
    .await
    .expect("fetch attendance status");
    assert_eq!(att_status, "leave");

    // Notification written to the member. `register_member` itself already
    // wrote a "welcome" notification, so order by `created_at DESC` to grab
    // the one this decide call just wrote, not that earlier one.
    let message: String = sqlx::query_scalar(
        "SELECT message FROM notifications WHERE user_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(member.user_id)
    .fetch_one(&app.db)
    .await
    .expect("fetch notification");
    assert!(message.contains("已核准"), "message was: {message}");
}

#[sqlx::test]
async fn decide_reject_does_not_write_attendance(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Leave Reject Course", None).await;
    let session_id = seed_course_session(&app.db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    let member = app.register_member("leave-reject-member@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "pending").await;

    let resp = app
        .patch(&format!("/api/v1/leave-requests/{leave_id}"))
        .authorization_bearer(&admin_token)
        .json(&json!({"status": "rejected"}))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["status"], "rejected");

    let att_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM attendance_records WHERE session_id = $1 AND enrolment_id = $2",
    )
    .bind(session_id)
    .bind(enrolment_id)
    .fetch_one(&app.db)
    .await
    .expect("count attendance");
    assert_eq!(att_count, 0, "reject must not write an attendance row");

    let message: String = sqlx::query_scalar(
        "SELECT message FROM notifications WHERE user_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(member.user_id)
    .fetch_one(&app.db)
    .await
    .expect("fetch notification");
    assert!(message.contains("已婉拒"), "message was: {message}");
}

#[sqlx::test]
async fn decide_by_non_owning_coach_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (other_coach_user, other_coach_token) =
        app.seed_user_with_roles("leave-decide-other-coach@example.com", &["coach"]).await;
    seed_coach(&app.db, other_coach_user, "Other Coach").await;

    // Course has no coach assigned at all (distinct from `other_coach`).
    let course_id = seed_course(&app.db, "Leave Decide Unowned Course", None).await;
    let session_id = seed_course_session(&app.db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    let member = app.register_member("leave-decide-member2@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "pending").await;

    let resp = app
        .patch(&format!("/api/v1/leave-requests/{leave_id}"))
        .authorization_bearer(&other_coach_token)
        .json(&json!({"status": "approved"}))
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn decide_non_pending_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Leave Decide 409 Course", None).await;
    let session_id = seed_course_session(&app.db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    let member = app.register_member("leave-decide-409@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "approved").await;

    let resp = app
        .patch(&format!("/api/v1/leave-requests/{leave_id}"))
        .authorization_bearer(&admin_token)
        .json(&json!({"status": "rejected"}))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

#[sqlx::test]
async fn decide_invalid_status_value_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Leave Decide 422 Course", None).await;
    let session_id = seed_course_session(&app.db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    let member = app.register_member("leave-decide-422@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "pending").await;

    // "pending" is a valid LeaveStatus value but not one PATCH accepts.
    let resp = app
        .patch(&format!("/api/v1/leave-requests/{leave_id}"))
        .authorization_bearer(&admin_token)
        .json(&json!({"status": "pending"}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

// ---------------------------------------------------------------------------
// POST /leave-requests/{id}/makeup
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn makeup_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post(&format!("/api/v1/leave-requests/{}/makeup", Uuid::now_v7()))
        .json(&json!({"session_id": Uuid::now_v7()}))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn makeup_same_course_future_session_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-makeup-ok@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Leave Makeup Course", None, 10).await;
    let session_id = seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session_id =
        seed_course_session(&app.db, course_id, target_date, t(14, 0), t(15, 0)).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "approved").await;

    let resp = app
        .post(&format!("/api/v1/leave-requests/{leave_id}/makeup"))
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": target_session_id}))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["makeup_session_id"], target_session_id.to_string());
    assert_eq!(body["makeup_session_date"], target_date.to_string());
    assert_eq!(body["makeup_start_time"], "14:00:00");

    let makeup: Option<Uuid> =
        sqlx::query_scalar("SELECT makeup_session_id FROM leave_requests WHERE id = $1")
            .bind(leave_id)
            .fetch_one(&app.db)
            .await
            .expect("fetch makeup_session_id");
    assert_eq!(makeup, Some(target_session_id));
}

#[sqlx::test]
async fn makeup_target_session_different_course_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-makeup-cross@example.com", "Password!234").await;
    let course_a = seed_course(&app.db, "Leave Makeup Course A", None).await;
    let course_b = seed_course(&app.db, "Leave Makeup Course B", None).await;
    let session_id = seed_course_session(&app.db, course_a, yesterday(), t(9, 0), t(10, 0)).await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session_id =
        seed_course_session(&app.db, course_b, target_date, t(14, 0), t(15, 0)).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_a, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "approved").await;

    let resp = app
        .post(&format!("/api/v1/leave-requests/{leave_id}/makeup"))
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": target_session_id}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

#[sqlx::test]
async fn makeup_target_session_already_started_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-makeup-started@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Leave Makeup Started Course", None, 10).await;
    let session_id = seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let target_session_id =
        seed_course_session(&app.db, course_id, yesterday(), t(14, 0), t(15, 0)).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "approved").await;

    let resp = app
        .post(&format!("/api/v1/leave-requests/{leave_id}/makeup"))
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": target_session_id}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

#[sqlx::test]
async fn makeup_requires_approved_status_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-makeup-pending@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Leave Makeup Pending Course", None, 10).await;
    let session_id = seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session_id =
        seed_course_session(&app.db, course_id, target_date, t(14, 0), t(15, 0)).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "pending").await;

    let resp = app
        .post(&format!("/api/v1/leave-requests/{leave_id}/makeup"))
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": target_session_id}))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

#[sqlx::test]
async fn makeup_already_booked_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-makeup-twice@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Leave Makeup Twice Course", None, 10).await;
    let session_id = seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let first_target = (Utc::now() + Duration::days(3)).date_naive();
    let first_target_id =
        seed_course_session(&app.db, course_id, first_target, t(14, 0), t(15, 0)).await;
    let second_target = (Utc::now() + Duration::days(4)).date_naive();
    let second_target_id =
        seed_course_session(&app.db, course_id, second_target, t(14, 0), t(15, 0)).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "approved").await;
    sqlx::query("UPDATE leave_requests SET makeup_session_id = $2 WHERE id = $1")
        .bind(leave_id)
        .bind(first_target_id)
        .execute(&app.db)
        .await
        .expect("preset makeup_session_id");

    let resp = app
        .post(&format!("/api/v1/leave-requests/{leave_id}/makeup"))
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": second_target_id}))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

#[sqlx::test]
async fn makeup_by_non_owner_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let owner = app.register_member("leave-makeup-owner@example.com", "Password!234").await;
    let other = app.register_member("leave-makeup-other@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Leave Makeup Owner Course", None, 10).await;
    let session_id = seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session_id =
        seed_course_session(&app.db, course_id, target_date, t(14, 0), t(15, 0)).await;
    let enrolment_id =
        seed_enrolment(&app.db, owner.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "approved").await;

    let resp = app
        .post(&format!("/api/v1/leave-requests/{leave_id}/makeup"))
        .authorization_bearer(&other.access_token)
        .json(&json!({"session_id": target_session_id}))
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn makeup_capacity_full_returns_409(db: PgPool) {
    // max_students = 1 and only the requesting student's own active
    // enrolment counts toward `active_count` → remaining = 1 - 1 + 0 - 0 = 0,
    // which fails the seat check's strict `> 0` requirement.
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-makeup-full@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Leave Makeup Full Course", None, 1).await;
    let session_id = seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session_id =
        seed_course_session(&app.db, course_id, target_date, t(14, 0), t(15, 0)).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "approved").await;

    let resp = app
        .post(&format!("/api/v1/leave-requests/{leave_id}/makeup"))
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": target_session_id}))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

// ---------------------------------------------------------------------------
// Makeup capacity — physical seat model (controller ruling 2026-07-06):
// remaining = max_students - active_count + approved_leave_for_target
//           - makeups_into_target, counting only still-active enrolments.
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn makeup_into_full_class_allowed_when_leave_frees_seats(db: PgPool) {
    // Controller regression (a): max=10, 10 active enrolments (full class),
    // 3 of them have APPROVED LEAVE for the target session, 0 makeups →
    // remaining = 10 - 10 + 3 - 0 = 3 → must ALLOW. (The pre-ruling formula
    // computed 10 - 10 - 3 + 0 = -3 and wrongly 409'd.)
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-seatmodel-a@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Seat Model Course A", None, 10).await;
    let original_session =
        seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session =
        seed_course_session(&app.db, course_id, target_date, t(14, 0), t(15, 0)).await;

    let my_enrolment = seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, my_enrolment, original_session, "approved").await;

    // Fill the class to exactly max_students = 10 (requester + 9 others);
    // 3 of the others take approved leave FOR THE TARGET session.
    for i in 0..9 {
        let other = common::seed_member(
            &app.db,
            &format!("seatmodel-a-{i}@example.com"),
            "Password!234",
        )
        .await;
        let other_enrolment =
            seed_enrolment(&app.db, other, course_id, "active", Utc::now()).await;
        if i < 3 {
            seed_leave_request(&app.db, other_enrolment, target_session, "approved").await;
        }
    }

    let resp = app
        .post(&format!("/api/v1/leave-requests/{leave_id}/makeup"))
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": target_session}))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
}

#[sqlx::test]
async fn makeup_rejected_when_prior_makeups_fill_remaining_seats(db: PgPool) {
    // Controller regression (b): max=10, 8 active enrolments, 0 leave for
    // the target, but 2 makeups already booked into it → remaining =
    // 10 - 8 + 0 - 2 = 0 → must 409. (The pre-ruling formula computed
    // 10 - 8 - 0 + 2 = 4 and would have overbooked an 11th seat.)
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-seatmodel-b@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Seat Model Course B", None, 10).await;
    let original_session =
        seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session =
        seed_course_session(&app.db, course_id, target_date, t(14, 0), t(15, 0)).await;

    let my_enrolment = seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, my_enrolment, original_session, "approved").await;

    // 8 active enrolments total (requester + 7 others); 2 of the others
    // already booked makeups INTO the target session.
    for i in 0..7 {
        let other = common::seed_member(
            &app.db,
            &format!("seatmodel-b-{i}@example.com"),
            "Password!234",
        )
        .await;
        let other_enrolment =
            seed_enrolment(&app.db, other, course_id, "active", Utc::now()).await;
        if i < 2 {
            let other_leave =
                seed_leave_request(&app.db, other_enrolment, original_session, "approved").await;
            sqlx::query("UPDATE leave_requests SET makeup_session_id = $2 WHERE id = $1")
                .bind(other_leave)
                .bind(target_session)
                .execute(&app.db)
                .await
                .expect("preset makeup_session_id");
        }
    }

    let resp = app
        .post(&format!("/api/v1/leave-requests/{leave_id}/makeup"))
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": target_session}))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

#[sqlx::test]
async fn makeup_leave_by_cancelled_enrolment_frees_no_ghost_seat(db: PgPool) {
    // Controller ruling: both seat counts only consider still-ACTIVE
    // enrolments. A leave-taker who has since cancelled their enrolment
    // must not free a ghost seat: max=1, requester is the only active
    // enrolment; a CANCELLED enrolment holds an approved leave for the
    // target → remaining = 1 - 1 + 0 - 0 = 0 → 409 (counting the cancelled
    // enrolment's leave would wrongly yield 1 and allow overbooking).
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-ghost-seat@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Ghost Seat Course", None, 1).await;
    let original_session =
        seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session =
        seed_course_session(&app.db, course_id, target_date, t(14, 0), t(15, 0)).await;

    let my_enrolment = seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, my_enrolment, original_session, "approved").await;

    let quitter =
        common::seed_member(&app.db, "ghost-seat-quitter@example.com", "Password!234").await;
    let quitter_enrolment =
        seed_enrolment(&app.db, quitter, course_id, "cancelled", Utc::now()).await;
    seed_leave_request(&app.db, quitter_enrolment, target_session, "approved").await;

    let resp = app
        .post(&format!("/api/v1/leave-requests/{leave_id}/makeup"))
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": target_session}))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

#[sqlx::test]
async fn makeup_booked_by_cancelled_enrolment_occupies_no_seat(db: PgPool) {
    // Symmetric active-only regression: a makeup booked by a since-
    // cancelled enrolment must not keep occupying a seat: max=2, requester
    // is the only active enrolment; a CANCELLED enrolment has a makeup
    // booked into the target → remaining = 2 - 1 + 0 - 0 = 1 → allowed
    // (counting the cancelled enrolment's makeup would wrongly yield 0
    // and block a genuinely free seat).
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-freed-seat@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Freed Seat Course", None, 2).await;
    let original_session =
        seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session =
        seed_course_session(&app.db, course_id, target_date, t(14, 0), t(15, 0)).await;

    let my_enrolment = seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, my_enrolment, original_session, "approved").await;

    let quitter =
        common::seed_member(&app.db, "freed-seat-quitter@example.com", "Password!234").await;
    let quitter_enrolment =
        seed_enrolment(&app.db, quitter, course_id, "cancelled", Utc::now()).await;
    let quitter_leave =
        seed_leave_request(&app.db, quitter_enrolment, original_session, "approved").await;
    sqlx::query("UPDATE leave_requests SET makeup_session_id = $2 WHERE id = $1")
        .bind(quitter_leave)
        .bind(target_session)
        .execute(&app.db)
        .await
        .expect("preset makeup_session_id");

    let resp = app
        .post(&format!("/api/v1/leave-requests/{leave_id}/makeup"))
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": target_session}))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
}
