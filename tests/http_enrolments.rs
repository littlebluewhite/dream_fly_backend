//! HTTP integration tests for `/enrolments/*` endpoints.

mod common;

use chrono::{Duration, NaiveTime, Utc};
use common::fixtures::{
    seed_attendance, seed_course_session, seed_course_with_capacity, seed_enrolment,
};
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

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

// ---------------------------------------------------------------------------
// GET /enrolments/me — attendance stats (attended/total)
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn me_attendance_stats_present_2_absent_1_gives_attended_2_total_3(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("enr-me-attend@example.com", "Password!234").await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course_with_capacity(&app.db, "Attendance Stats Course", None, 10).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;

    // Seeded in the past (-3/-2/-1 days): `PUT .../attendance` now gates on
    // the session having already started, so a `today`/future seed here
    // would 422 instead of marking successfully.
    let today = Utc::now().date_naive();
    let session_1 =
        seed_course_session(&app.db, course_id, today - Duration::days(3), t(9, 0), t(10, 0)).await;
    let session_2 =
        seed_course_session(&app.db, course_id, today - Duration::days(2), t(9, 0), t(10, 0)).await;
    let session_3 =
        seed_course_session(&app.db, course_id, today - Duration::days(1), t(9, 0), t(10, 0)).await;

    for (session_id, status) in
        [(session_1, "present"), (session_2, "present"), (session_3, "absent")]
    {
        let resp = app
            .put(&format!("/api/v1/sessions/{session_id}/attendance"))
            .authorization_bearer(&admin_token)
            .json(&json!({"records": [{"enrolment_id": enrolment_id, "status": status}]}))
            .await;
        assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    }

    let resp = app
        .get("/api/v1/enrolments/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array, not an envelope");
    let entry = arr
        .iter()
        .find(|e| e["id"] == enrolment_id.to_string())
        .expect("enrolment present in /enrolments/me");
    assert_eq!(entry["attended"], 2, "present-count must be 2, got {entry:?}");
    assert_eq!(entry["total"], 3, "marked-session-count must be 3, got {entry:?}");
}

#[sqlx::test]
async fn me_attendance_stats_with_no_marks_is_zero_zero(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("enr-me-noattend@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "No Attendance Course", None, 10).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .get("/api/v1/enrolments/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array, not an envelope");
    let entry = arr
        .iter()
        .find(|e| e["id"] == enrolment_id.to_string())
        .expect("enrolment present in /enrolments/me");
    assert_eq!(entry["attended"], 0);
    assert_eq!(entry["total"], 0);
}

/// `countable_attendance` view regression: `leave` must not inflate `total`.
/// Seeded straight via `seed_attendance` (bypasses `PUT .../attendance`
/// entirely, so the "session already started" gate never applies here) —
/// this test is about the read-side aggregation caliber, not the write-side
/// gate. Old semantics (`COUNT(attendance_records.id)`, no status filter)
/// would give `total=4`; the `countable_attendance` view excludes `leave`
/// from its membership, so `total` must be `3` (present+absent only).
#[sqlx::test]
async fn me_attendance_stats_present_2_absent_1_leave_1_excludes_leave_from_total(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("enr-me-leave-excl@example.com", "Password!234").await;
    let (admin_id, _admin_token) = app.seed_admin().await;
    let course_id =
        seed_course_with_capacity(&app.db, "Attendance Leave Excl Course", None, 10).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;

    let today = Utc::now().date_naive();
    let session_present_1 = seed_course_session(&app.db, course_id, today, t(9, 0), t(10, 0)).await;
    let session_present_2 =
        seed_course_session(&app.db, course_id, today + Duration::days(1), t(9, 0), t(10, 0)).await;
    let session_absent =
        seed_course_session(&app.db, course_id, today + Duration::days(2), t(9, 0), t(10, 0)).await;
    let session_leave =
        seed_course_session(&app.db, course_id, today + Duration::days(3), t(9, 0), t(10, 0)).await;

    seed_attendance(&app.db, session_present_1, enrolment_id, "present", admin_id).await;
    seed_attendance(&app.db, session_present_2, enrolment_id, "present", admin_id).await;
    seed_attendance(&app.db, session_absent, enrolment_id, "absent", admin_id).await;
    seed_attendance(&app.db, session_leave, enrolment_id, "leave", admin_id).await;

    let resp = app
        .get("/api/v1/enrolments/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array, not an envelope");
    let entry = arr
        .iter()
        .find(|e| e["id"] == enrolment_id.to_string())
        .expect("enrolment present in /enrolments/me");
    assert_eq!(entry["attended"], 2, "present-count must be 2, got {entry:?}");
    assert_eq!(
        entry["total"], 3,
        "countable total must be present+absent=3, excluding the leave record \
         (old attendance_records-count semantics would wrongly give 4), got {entry:?}"
    );
}

// ---------------------------------------------------------------------------
// GET /enrolments/{id}/attendance — member 逐堂出勤 (read-only)
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn attendance_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .get(&format!("/api/v1/enrolments/{}/attendance", Uuid::now_v7()))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn attendance_as_non_owner_member_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let owner = app.register_member("enr-att-owner@example.com", "Password!234").await;
    let other = app.register_member("enr-att-other@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Attendance Owner Course", None, 10).await;
    let enrolment_id =
        seed_enrolment(&app.db, owner.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .get(&format!("/api/v1/enrolments/{enrolment_id}/attendance"))
        .authorization_bearer(&other.access_token)
        .await;
    assert_eq!(
        resp.status_code(),
        404,
        "non-owner must get 404, not 403 — mustn't leak enrolment existence; body={}",
        resp.text()
    );
}

#[sqlx::test]
async fn attendance_unknown_enrolment_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("enr-att-404@example.com", "Password!234").await;

    let resp = app
        .get(&format!("/api/v1/enrolments/{}/attendance", Uuid::now_v7()))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 404, "body={}", resp.text());
}

#[sqlx::test]
async fn attendance_owner_with_no_marks_returns_200_empty_array(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("enr-att-empty@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Attendance Empty Course", None, 10).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .get(&format!("/api/v1/enrolments/{enrolment_id}/attendance"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body.as_array().expect("plain array, not an envelope").len(), 0);
}

#[sqlx::test]
async fn attendance_owner_sees_marked_sessions_oldest_to_newest_with_full_fields(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("enr-att-owner2@example.com", "Password!234").await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id =
        seed_course_with_capacity(&app.db, "Attendance Timeline Course", None, 10).await;
    let enrolment_id =
        seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;

    let today = Utc::now().date_naive();
    // Seed/mark out of chronological order to prove the endpoint sorts by
    // session date rather than echoing insertion or marking order. Marked
    // sessions are seeded in the past (-3/-2/-1 days) so `PUT
    // .../attendance`'s "session already started" gate doesn't reject the
    // marking calls below; the unmarked session stays in the future to
    // prove exclusion still works (it's never PUT, so the gate never
    // applies to it).
    let session_newest =
        seed_course_session(&app.db, course_id, today - Duration::days(1), t(9, 0), t(10, 0))
            .await;
    let session_oldest =
        seed_course_session(&app.db, course_id, today - Duration::days(3), t(9, 0), t(10, 0))
            .await;
    let session_middle =
        seed_course_session(&app.db, course_id, today - Duration::days(2), t(9, 0), t(10, 0))
            .await;
    // An unmarked session for the same enrolment — must not appear.
    seed_course_session(&app.db, course_id, today + Duration::days(3), t(9, 0), t(10, 0)).await;

    for (session_id, status) in
        [(session_newest, "present"), (session_oldest, "absent"), (session_middle, "leave")]
    {
        let resp = app
            .put(&format!("/api/v1/sessions/{session_id}/attendance"))
            .authorization_bearer(&admin_token)
            .json(&json!({"records": [{"enrolment_id": enrolment_id, "status": status}]}))
            .await;
        assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    }

    let resp = app
        .get(&format!("/api/v1/enrolments/{enrolment_id}/attendance"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array, not an envelope");
    assert_eq!(arr.len(), 3, "only marked sessions must appear, got {arr:?}");

    // Oldest to newest.
    assert_eq!(arr[0]["session_date"], (today - Duration::days(3)).to_string());
    assert_eq!(arr[0]["status"], "absent");
    assert_eq!(arr[1]["session_date"], (today - Duration::days(2)).to_string());
    assert_eq!(arr[1]["status"], "leave");
    assert_eq!(arr[2]["session_date"], (today - Duration::days(1)).to_string());
    assert_eq!(arr[2]["status"], "present");

    // Field completeness on one entry.
    let first = &arr[0];
    assert_eq!(first["start_time"], "09:00:00");
    assert_eq!(first["end_time"], "10:00:00");
    assert!(first["marked_at"].as_str().is_some(), "marked_at present, got {first:?}");
}

#[sqlx::test]
async fn attendance_as_admin_can_view_any_enrolment(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let owner = app.register_member("enr-att-admin-owner@example.com", "Password!234").await;
    let course_id = seed_course_with_capacity(&app.db, "Attendance Admin Course", None, 10).await;
    let enrolment_id =
        seed_enrolment(&app.db, owner.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .get(&format!("/api/v1/enrolments/{enrolment_id}/attendance"))
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    assert_eq!(resp.json::<serde_json::Value>().as_array().unwrap().len(), 0);
}
