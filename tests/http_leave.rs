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
//! The makeup seat model's scenarios live in `tests/service_seats.rs`
//! (`courses::seats` owns the formula), and `leave::rules`' pure checks are
//! unit-tested in `src/modules/leave/rules.rs` — this file keeps one HTTP
//! representative per status code.

mod common;

use chrono::{Duration, NaiveTime, Utc};
use common::fixtures::{
    seed_attendance, seed_coach, seed_course, seed_course_session, seed_course_with_capacity,
    seed_enrolment, seed_leave_request, seed_leave_scene,
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
    let scene = seed_leave_scene(&app.db, user.user_id, course_id, "pending", None).await;

    let resp = app
        .post("/api/v1/leave-requests")
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": scene.session}))
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
    let scene = seed_leave_scene(&app.db, user.user_id, course_id, "cancelled", None).await;

    let resp = app
        .post("/api/v1/leave-requests")
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": scene.session}))
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
    let makeup_session_id =
        seed_course_session(&app.db, course_id, makeup_date, t(14, 0), t(15, 0)).await;
    let scene =
        seed_leave_scene(&app.db, user.user_id, course_id, "approved", Some(makeup_session_id))
            .await;
    let leave_id = scene.leave;

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
    let scene = seed_leave_scene(&app.db, user.user_id, course_id, "pending", None).await;

    let resp = app
        .delete(&format!("/api/v1/leave-requests/{}", scene.leave))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 204, "body={}", resp.text());

    let status: String =
        sqlx::query_scalar("SELECT status::text FROM leave_requests WHERE id = $1")
            .bind(scene.leave)
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
    let scene = seed_leave_scene(&app.db, user.user_id, course_id, "approved", None).await;

    let resp = app
        .delete(&format!("/api/v1/leave-requests/{}", scene.leave))
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
    let scene = seed_leave_scene(&app.db, owner.user_id, course_id, "pending", None).await;

    let resp = app
        .delete(&format!("/api/v1/leave-requests/{}", scene.leave))
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
    let user_a = app.register_member("leave-list-a@example.com", "Password!234").await;
    let user_b = app.register_member("leave-list-b@example.com", "Password!234").await;
    seed_leave_scene(&app.db, user_a.user_id, course_a, "pending", None).await;
    seed_leave_scene(&app.db, user_b.user_id, course_b, "approved", None).await;

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

/// Case policy: `status` is one of the fields the wire is case-sensitive
/// about — a legal value in the wrong case is rejected, not silently
/// accepted.
#[sqlx::test]
async fn list_mixed_case_status_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/leave-requests?status=Pending")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
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

    let user_a = app.register_member("leave-list-student-a@example.com", "Password!234").await;
    let user_b = app.register_member("leave-list-student-b@example.com", "Password!234").await;
    seed_leave_scene(&app.db, user_a.user_id, course_a, "pending", None).await;
    seed_leave_scene(&app.db, user_b.user_id, course_b, "pending", None).await;

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
async fn decide_as_member_returns_403(db: PgPool) {
    // staff gate (admin-or-coach) parity: a plain member is rejected even
    // when the body is malformed. `status: ""` fails
    // `DecideLeaveRequestRequest`'s `length(min = 1)` validator, so under
    // the old handler-first-line gate this would have 422'd out of
    // `ValidatedJson` before the role check ever ran. The route-layer gate
    // now runs ahead of extraction, so the member still gets 403.
    let app = spawn_test_app(db).await;
    let user = app.register_member("leave-decide-member-403@example.com", "Password!234").await;
    let resp = app
        .patch(&format!("/api/v1/leave-requests/{}", Uuid::now_v7()))
        .authorization_bearer(&user.access_token)
        .json(&json!({"status": ""}))
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn decide_approve_writes_attendance_leave_and_notification(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) =
        app.seed_user_with_roles("leave-decide-coach@example.com", &["coach"]).await;
    let coach_id = seed_coach(&app.db, coach_user_id, "Decide Coach").await;
    let course_id = seed_course(&app.db, "Leave Decide Course", Some(coach_id)).await;
    let member = app.register_member("leave-decide-member@example.com", "Password!234").await;
    let scene = seed_leave_scene(&app.db, member.user_id, course_id, "pending", None).await;

    let resp = app
        .patch(&format!("/api/v1/leave-requests/{}", scene.leave))
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
            .bind(scene.leave)
            .fetch_one(&app.db)
            .await
            .expect("fetch status");
    assert_eq!(status, "approved");

    // attendance_records row written with status = 'leave'.
    let att_status: String = sqlx::query_scalar(
        "SELECT status::text FROM attendance_records WHERE session_id = $1 AND enrolment_id = $2",
    )
    .bind(scene.session)
    .bind(scene.enrolment)
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

/// 核准恆勝 (ADR-0008): approving a leave request overwrites an already-marked
/// `present` attendance with `leave`, even for an already-started (here:
/// yesterday) session — a late approval is a legitimate ruling, not a race to
/// lose. The upsert guard's first branch (`EXCLUDED.status = 'leave'`) lets the
/// approval win over any prior mark. Sibling of
/// `attendance_put_present_over_approved_leave_rejects_whole_batch` (the
/// reverse direction, which the guard *blocks*) in `http_attendance.rs`.
#[sqlx::test]
async fn decide_approve_overwrites_existing_present_attendance(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) =
        app.seed_user_with_roles("leave-approve-over-present-coach@example.com", &["coach"]).await;
    let coach_id = seed_coach(&app.db, coach_user_id, "Approve Over Present Coach").await;
    let course_id = seed_course(&app.db, "Approve Over Present Course", Some(coach_id)).await;
    let member =
        app.register_member("leave-approve-over-present-member@example.com", "Password!234").await;
    // Already-started session so the prior `present` mark is realistic; decide
    // has no time gate, so a late approval still applies.
    let session_id = seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;
    seed_attendance(&app.db, session_id, enrolment_id, "present", coach_user_id).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "pending").await;

    let resp = app
        .patch(&format!("/api/v1/leave-requests/{leave_id}"))
        .authorization_bearer(&coach_token)
        .json(&json!({"status": "approved"}))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());

    let att_status: String = sqlx::query_scalar(
        "SELECT status::text FROM attendance_records WHERE session_id = $1 AND enrolment_id = $2",
    )
    .bind(session_id)
    .bind(enrolment_id)
    .fetch_one(&app.db)
    .await
    .expect("fetch attendance status");
    assert_eq!(att_status, "leave", "approval must overwrite the prior present with leave");
}

#[sqlx::test]
async fn decide_reject_does_not_write_attendance(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Leave Reject Course", None).await;
    let member = app.register_member("leave-reject-member@example.com", "Password!234").await;
    let scene = seed_leave_scene(&app.db, member.user_id, course_id, "pending", None).await;

    let resp = app
        .patch(&format!("/api/v1/leave-requests/{}", scene.leave))
        .authorization_bearer(&admin_token)
        .json(&json!({"status": "rejected"}))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["status"], "rejected");

    let att_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM attendance_records WHERE session_id = $1 AND enrolment_id = $2",
    )
    .bind(scene.session)
    .bind(scene.enrolment)
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
    let member = app.register_member("leave-decide-member2@example.com", "Password!234").await;
    let scene = seed_leave_scene(&app.db, member.user_id, course_id, "pending", None).await;

    let resp = app
        .patch(&format!("/api/v1/leave-requests/{}", scene.leave))
        .authorization_bearer(&other_coach_token)
        .json(&json!({"status": "approved"}))
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn decide_invalid_status_value_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Leave Decide 422 Course", None).await;
    let member = app.register_member("leave-decide-422@example.com", "Password!234").await;
    let scene = seed_leave_scene(&app.db, member.user_id, course_id, "pending", None).await;

    // "pending" is a valid LeaveStatus value but not one PATCH accepts.
    let resp = app
        .patch(&format!("/api/v1/leave-requests/{}", scene.leave))
        .authorization_bearer(&admin_token)
        .json(&json!({"status": "pending"}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

/// B5: cancelling the enrolment (through the real cancel route) cancels its
/// still-pending leave request in the same tx, so a later approve hits the
/// not-pending 409 — the enrolment-cancelled 409 is only a backstop now.
#[sqlx::test]
async fn decide_approve_after_enrolment_cancel_is_409_not_pending(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("leave-decide-cancelled-coach@example.com", &["coach"])
        .await;
    let coach_id = seed_coach(&app.db, coach_user_id, "Cancelled Enrolment Coach").await;
    let course_id = seed_course(&app.db, "Leave Decide Cancelled Course", Some(coach_id)).await;
    let member = app
        .register_member("leave-decide-cancelled-member@example.com", "Password!234")
        .await;
    let scene = seed_leave_scene(&app.db, member.user_id, course_id, "pending", None).await;

    let cancel_resp = app
        .patch(&format!("/api/v1/enrolments/{}/cancel", scene.enrolment))
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(
        cancel_resp.status_code(),
        200,
        "body={}",
        cancel_resp.text()
    );

    let resp = app
        .patch(&format!("/api/v1/leave-requests/{}", scene.leave))
        .authorization_bearer(&coach_token)
        .json(&json!({"status": "approved"}))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "僅待審核假單可審核"
    );
}

/// B5 counterpart: rejecting after the enrolment cancel is now also a
/// not-pending 409 (it used to be 200) — the request was already cancelled
/// along with its enrolment, so it never sits in the coach's pending queue.
#[sqlx::test]
async fn decide_reject_after_enrolment_cancel_is_409_not_pending(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Leave Decide Cancelled Reject Course", None).await;
    let member = app
        .register_member("leave-decide-cancelled-reject@example.com", "Password!234")
        .await;
    let scene = seed_leave_scene(&app.db, member.user_id, course_id, "pending", None).await;

    let cancel_resp = app
        .patch(&format!("/api/v1/enrolments/{}/cancel", scene.enrolment))
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(
        cancel_resp.status_code(),
        200,
        "body={}",
        cancel_resp.text()
    );

    let resp = app
        .patch(&format!("/api/v1/leave-requests/{}", scene.leave))
        .authorization_bearer(&admin_token)
        .json(&json!({"status": "rejected"}))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "僅待審核假單可審核"
    );
}

/// B5 member-side counterpart: once the enrolment cancel has cancelled the
/// pending leave in the same tx, the member's own `DELETE` on it is the
/// not-pending 409.
#[sqlx::test]
async fn cancel_leave_after_enrolment_cancel_is_409_not_pending(db: PgPool) {
    let app = spawn_test_app(db).await;
    let course_id = seed_course(&app.db, "Leave Cancel After Enrolment Cancel Course", None).await;
    let member = app
        .register_member(
            "leave-cancel-after-enrolment-cancel@example.com",
            "Password!234",
        )
        .await;
    let scene = seed_leave_scene(&app.db, member.user_id, course_id, "pending", None).await;

    let cancel_resp = app
        .patch(&format!("/api/v1/enrolments/{}/cancel", scene.enrolment))
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(
        cancel_resp.status_code(),
        200,
        "body={}",
        cancel_resp.text()
    );

    let resp = app
        .delete(&format!("/api/v1/leave-requests/{}", scene.leave))
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "僅待審核假單可取消"
    );
}

/// Backstop: a pending leave on an already-cancelled enrolment (a row inserted
/// after the cancel committed, or pre-B5 legacy data — built here straight by
/// fixture) still can't be approved.
#[sqlx::test]
async fn decide_approve_pending_on_cancelled_enrolment_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Leave Backstop Approve Course", None).await;
    let member = app
        .register_member("leave-backstop-approve@example.com", "Password!234")
        .await;
    let session = seed_course_session(&app.db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    let enrolment =
        seed_enrolment(&app.db, member.user_id, course_id, "cancelled", Utc::now()).await;
    let leave = seed_leave_request(&app.db, enrolment, session, "pending").await;

    let resp = app
        .patch(&format!("/api/v1/leave-requests/{leave}"))
        .authorization_bearer(&admin_token)
        .json(&json!({"status": "approved"}))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "報名已取消，無法核准請假"
    );
}

/// Backstop counterpart: such a leftover pending leave can still be rejected,
/// so a coach can clear it out of the pending queue.
#[sqlx::test]
async fn decide_reject_pending_on_cancelled_enrolment_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Leave Backstop Reject Course", None).await;
    let member = app
        .register_member("leave-backstop-reject@example.com", "Password!234")
        .await;
    let session = seed_course_session(&app.db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    let enrolment =
        seed_enrolment(&app.db, member.user_id, course_id, "cancelled", Utc::now()).await;
    let leave = seed_leave_request(&app.db, enrolment, session, "pending").await;

    let resp = app
        .patch(&format!("/api/v1/leave-requests/{leave}"))
        .authorization_bearer(&admin_token)
        .json(&json!({"status": "rejected"}))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    assert_eq!(resp.json::<serde_json::Value>()["status"], "rejected");
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

/// Task 3: once the member's own enrolment (the one this approved leave
/// belongs to) has been cancelled through the real cancel route, they may no
/// longer book a makeup from it — distinct from `service_seats.rs`'s
/// `leave_by_cancelled_enrolment_frees_no_ghost_seat`, which cancels a
/// *different* member's enrolment to test the seat formula, not this owner's
/// own request.
#[sqlx::test]
async fn makeup_cancelled_enrolment_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("leave-makeup-cancelled@example.com", "Password!234")
        .await;
    let course_id = seed_course(&app.db, "Makeup Cancelled Enrolment Course", None).await;
    let scene = seed_leave_scene(&app.db, user.user_id, course_id, "approved", None).await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session =
        seed_course_session(&app.db, course_id, target_date, t(14, 0), t(15, 0)).await;

    let cancel_resp = app
        .patch(&format!("/api/v1/enrolments/{}/cancel", scene.enrolment))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(
        cancel_resp.status_code(),
        200,
        "body={}",
        cancel_resp.text()
    );

    let resp = app
        .post(&format!("/api/v1/leave-requests/{}/makeup", scene.leave))
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": target_session}))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

// ---------------------------------------------------------------------------
// Pin: write responses equal the read projection, field-for-field (task 5a)
// ---------------------------------------------------------------------------
//
// `create`/`decide`/`makeup`'s responses and `GET /leave-requests/me`'s rows
// are meant to be the exact same shape — these lock that invariant with
// `serde_json::Value` equality before task 5b gives the projection a single
// owner, so the refactor can't quietly drift a write response away from the
// read side it's supposed to mirror.

#[sqlx::test]
async fn create_response_matches_me_row(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("leave-pin-create@example.com", "Password!234")
        .await;
    let course_id = seed_course(&app.db, "Leave Pin Create Course", None).await;
    let session_id = seed_course_session(&app.db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;

    let create_resp = app
        .post("/api/v1/leave-requests")
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": session_id, "reason": "感冒"}))
        .await;
    assert_eq!(
        create_resp.status_code(),
        200,
        "body={}",
        create_resp.text()
    );
    let created: serde_json::Value = create_resp.json();

    let me_resp = app
        .get("/api/v1/leave-requests/me")
        .authorization_bearer(&user.access_token)
        .await;
    let me_rows: serde_json::Value = me_resp.json();
    let me_row = me_rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == created["id"])
        .expect("created row present in /me");

    assert_eq!(&created, me_row, "create response must equal its /me row");
}

#[sqlx::test]
async fn decide_approve_response_matches_me_row(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("leave-pin-approve-coach@example.com", &["coach"])
        .await;
    let coach_id = seed_coach(&app.db, coach_user_id, "Pin Approve Coach").await;
    let course_id = seed_course(&app.db, "Leave Pin Approve Course", Some(coach_id)).await;
    let member = app
        .register_member("leave-pin-approve-member@example.com", "Password!234")
        .await;
    let scene = seed_leave_scene(&app.db, member.user_id, course_id, "pending", None).await;

    let decide_resp = app
        .patch(&format!("/api/v1/leave-requests/{}", scene.leave))
        .authorization_bearer(&coach_token)
        .json(&json!({"status": "approved"}))
        .await;
    assert_eq!(
        decide_resp.status_code(),
        200,
        "body={}",
        decide_resp.text()
    );
    let decided: serde_json::Value = decide_resp.json();

    let me_resp = app
        .get("/api/v1/leave-requests/me")
        .authorization_bearer(&member.access_token)
        .await;
    let me_rows: serde_json::Value = me_resp.json();
    let me_row = me_rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == decided["id"])
        .expect("decided row present in /me");

    assert_eq!(&decided, me_row, "approve response must equal its /me row");
}

#[sqlx::test]
async fn decide_reject_response_matches_me_row(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Leave Pin Reject Course", None).await;
    let member = app
        .register_member("leave-pin-reject-member@example.com", "Password!234")
        .await;
    let scene = seed_leave_scene(&app.db, member.user_id, course_id, "pending", None).await;

    let decide_resp = app
        .patch(&format!("/api/v1/leave-requests/{}", scene.leave))
        .authorization_bearer(&admin_token)
        .json(&json!({"status": "rejected"}))
        .await;
    assert_eq!(
        decide_resp.status_code(),
        200,
        "body={}",
        decide_resp.text()
    );
    let decided: serde_json::Value = decide_resp.json();

    let me_resp = app
        .get("/api/v1/leave-requests/me")
        .authorization_bearer(&member.access_token)
        .await;
    let me_rows: serde_json::Value = me_resp.json();
    let me_row = me_rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == decided["id"])
        .expect("decided row present in /me");

    assert_eq!(&decided, me_row, "reject response must equal its /me row");
}

#[sqlx::test]
async fn makeup_response_matches_me_row(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("leave-pin-makeup@example.com", "Password!234")
        .await;
    let course_id = seed_course_with_capacity(&app.db, "Leave Pin Makeup Course", None, 10).await;
    let session_id = seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session_id =
        seed_course_session(&app.db, course_id, target_date, t(14, 0), t(15, 0)).await;
    let enrolment_id = seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;
    let leave_id = seed_leave_request(&app.db, enrolment_id, session_id, "approved").await;

    let makeup_resp = app
        .post(&format!("/api/v1/leave-requests/{leave_id}/makeup"))
        .authorization_bearer(&user.access_token)
        .json(&json!({"session_id": target_session_id}))
        .await;
    assert_eq!(
        makeup_resp.status_code(),
        200,
        "body={}",
        makeup_resp.text()
    );
    let booked: serde_json::Value = makeup_resp.json();

    let me_resp = app
        .get("/api/v1/leave-requests/me")
        .authorization_bearer(&user.access_token)
        .await;
    let me_rows: serde_json::Value = me_resp.json();
    let me_row = me_rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == booked["id"])
        .expect("booked row present in /me");

    assert_eq!(&booked, me_row, "makeup response must equal its /me row");
}

#[sqlx::test]
async fn admin_list_row_matches_me_row_minus_user_fields(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let member = app
        .register_member("leave-pin-admin-list@example.com", "Password!234")
        .await;
    let course_id = seed_course(&app.db, "Leave Pin Admin List Course", None).await;
    let makeup_date = (Utc::now() + Duration::days(8)).date_naive();
    let makeup_session_id =
        seed_course_session(&app.db, course_id, makeup_date, t(14, 0), t(15, 0)).await;
    let scene = seed_leave_scene(
        &app.db,
        member.user_id,
        course_id,
        "approved",
        Some(makeup_session_id),
    )
    .await;

    let list_resp = app
        .get(&format!("/api/v1/leave-requests?course_id={course_id}"))
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(list_resp.status_code(), 200, "body={}", list_resp.text());
    let list_body: serde_json::Value = list_resp.json();
    let mut admin_row = list_body["leave_requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == scene.leave.to_string())
        .expect("row present in admin list")
        .clone();
    let admin_obj = admin_row.as_object_mut().unwrap();
    admin_obj.remove("user_id");
    admin_obj.remove("user_name");

    let me_resp = app
        .get("/api/v1/leave-requests/me")
        .authorization_bearer(&member.access_token)
        .await;
    let me_rows: serde_json::Value = me_resp.json();
    let me_row = me_rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == admin_row["id"])
        .expect("row present in /me");

    assert_eq!(
        &admin_row, me_row,
        "admin list row minus user_id/user_name must equal its /me row"
    );
}
