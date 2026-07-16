//! HTTP integration tests for the attendance module's endpoints:
//! `GET /sessions/{id}/roster`, `PUT /sessions/{id}/attendance`,
//! `GET /coaches/me/students`.

mod common;

use chrono::{Duration, NaiveTime, Utc};
use common::fixtures::{seed_coach, seed_course, seed_course_session, seed_enrolment};
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

fn yesterday() -> chrono::NaiveDate {
    (Utc::now() - Duration::days(1)).date_naive()
}

async fn attendance_records_count(db: &PgPool, session_id: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM attendance_records WHERE session_id = $1")
        .bind(session_id)
        .fetch_one(db)
        .await
        .expect("count attendance_records")
}

// ---------------------------------------------------------------------------
// GET /sessions/{id}/roster
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn roster_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .get(&format!("/api/v1/sessions/{}/roster", Uuid::now_v7()))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn roster_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("att-roster-member@example.com", "Password!234").await;

    let resp = app
        .get(&format!("/api/v1/sessions/{}/roster", Uuid::now_v7()))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn roster_unknown_session_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get(&format!("/api/v1/sessions/{}/roster", Uuid::now_v7()))
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 404, "body={}", resp.text());
}

#[sqlx::test]
async fn roster_as_non_course_coach_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (owner_user_id, _owner_token) = app
        .seed_user_with_roles("att-roster-owner@example.com", &["coach"])
        .await;
    let owner_coach_id = seed_coach(&app.db, owner_user_id, "Owner Coach").await;
    let course_id = seed_course(&app.db, "Roster Owned Course", Some(owner_coach_id)).await;
    let session_id =
        seed_course_session(&app.db, course_id, Utc::now().date_naive(), t(9, 0), t(10, 0)).await;

    let (other_user_id, other_token) = app
        .seed_user_with_roles("att-roster-other@example.com", &["coach"])
        .await;
    seed_coach(&app.db, other_user_id, "Other Coach").await;

    let resp = app
        .get(&format!("/api/v1/sessions/{session_id}/roster"))
        .authorization_bearer(&other_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn roster_as_course_coach_shows_active_enrolments_with_null_status(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("att-roster-coach@example.com", &["coach"])
        .await;
    let coach_id = seed_coach(&app.db, coach_user_id, "Roster Coach").await;
    let course_id = seed_course(&app.db, "Roster Course", Some(coach_id)).await;
    let session_id =
        seed_course_session(&app.db, course_id, Utc::now().date_naive(), t(9, 0), t(10, 0)).await;

    let member_a = app.register_member("att-roster-a@example.com", "Password!234").await;
    let member_b = app.register_member("att-roster-b@example.com", "Password!234").await;
    let member_cancelled =
        app.register_member("att-roster-cancelled@example.com", "Password!234").await;
    let enrolment_a =
        seed_enrolment(&app.db, member_a.user_id, course_id, "active", Utc::now()).await;
    let _enrolment_b =
        seed_enrolment(&app.db, member_b.user_id, course_id, "active", Utc::now()).await;
    seed_enrolment(&app.db, member_cancelled.user_id, course_id, "cancelled", Utc::now()).await;

    let resp = app
        .get(&format!("/api/v1/sessions/{session_id}/roster"))
        .authorization_bearer(&coach_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array, not an envelope");
    assert_eq!(arr.len(), 2, "cancelled enrolment must not appear in roster, got {arr:?}");

    let entry_a = arr
        .iter()
        .find(|e| e["enrolment_id"] == enrolment_a.to_string())
        .expect("member_a roster entry present");
    assert_eq!(entry_a["user_id"], member_a.user_id.to_string());
    assert_eq!(entry_a["user_name"], "Test Member");
    assert!(
        entry_a["attendance_status"].is_null(),
        "unmarked entry must be null, got {entry_a:?}"
    );
}

#[sqlx::test]
async fn roster_as_admin_works_for_any_course(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Roster Admin Course", None).await;
    let session_id =
        seed_course_session(&app.db, course_id, Utc::now().date_naive(), t(9, 0), t(10, 0)).await;
    let member = app.register_member("att-roster-admin-member@example.com", "Password!234").await;
    seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .get(&format!("/api/v1/sessions/{session_id}/roster"))
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body.as_array().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// PUT /sessions/{id}/attendance
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn attendance_put_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .put(&format!("/api/v1/sessions/{}/attendance", Uuid::now_v7()))
        .json(&json!({"records": []}))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn attendance_put_as_member_returns_403(db: PgPool) {
    // staff gate (admin-or-coach) parity — mirrors `roster_as_member_returns_403`
    // above for the PUT sibling, which had no dedicated member-role test.
    let app = spawn_test_app(db).await;
    let user = app.register_member("att-put-member-403@example.com", "Password!234").await;
    let resp = app
        .put(&format!("/api/v1/sessions/{}/attendance", Uuid::now_v7()))
        .authorization_bearer(&user.access_token)
        .json(&json!({"records": []}))
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn attendance_put_as_non_course_coach_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (owner_user_id, _owner_token) = app
        .seed_user_with_roles("att-put-owner@example.com", &["coach"])
        .await;
    let owner_coach_id = seed_coach(&app.db, owner_user_id, "Put Owner Coach").await;
    let course_id = seed_course(&app.db, "Put Owned Course", Some(owner_coach_id)).await;
    let session_id =
        seed_course_session(&app.db, course_id, Utc::now().date_naive(), t(9, 0), t(10, 0)).await;
    let member = app.register_member("att-put-member@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;

    let (other_user_id, other_token) = app
        .seed_user_with_roles("att-put-other@example.com", &["coach"])
        .await;
    seed_coach(&app.db, other_user_id, "Put Other Coach").await;

    let resp = app
        .put(&format!("/api/v1/sessions/{session_id}/attendance"))
        .authorization_bearer(&other_token)
        .json(&json!({"records": [{"enrolment_id": enrolment_id, "status": "present"}]}))
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
    assert_eq!(attendance_records_count(&app.db, session_id).await, 0);
}

#[sqlx::test]
async fn attendance_put_cross_course_enrolment_rejects_whole_batch_with_no_writes(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Attendance Course A", None).await;
    let other_course_id = seed_course(&app.db, "Attendance Course B", None).await;
    let session_id = seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;

    let member_in = app.register_member("att-cross-in@example.com", "Password!234").await;
    let member_out = app.register_member("att-cross-out@example.com", "Password!234").await;
    let enrolment_in =
        seed_enrolment(&app.db, member_in.user_id, course_id, "active", Utc::now()).await;
    let enrolment_out =
        seed_enrolment(&app.db, member_out.user_id, other_course_id, "active", Utc::now()).await;

    let resp = app
        .put(&format!("/api/v1/sessions/{session_id}/attendance"))
        .authorization_bearer(&admin_token)
        .json(&json!({"records": [
            {"enrolment_id": enrolment_in, "status": "present"},
            {"enrolment_id": enrolment_out, "status": "present"},
        ]}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
    assert_eq!(
        attendance_records_count(&app.db, session_id).await,
        0,
        "the whole batch must be rejected with zero writes, including the valid record"
    );
}

#[sqlx::test]
async fn attendance_put_cancelled_enrolment_rejects_whole_batch(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Attendance Cancelled Course", None).await;
    let session_id = seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let member = app.register_member("att-cancelled-member@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "cancelled", Utc::now()).await;

    let resp = app
        .put(&format!("/api/v1/sessions/{session_id}/attendance"))
        .authorization_bearer(&admin_token)
        .json(&json!({"records": [{"enrolment_id": enrolment_id, "status": "present"}]}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
    assert_eq!(attendance_records_count(&app.db, session_id).await, 0);
}

#[sqlx::test]
async fn attendance_put_invalid_status_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Attendance Invalid Status Course", None).await;
    let session_id = seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let member = app.register_member("att-invalid-status@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .put(&format!("/api/v1/sessions/{session_id}/attendance"))
        .authorization_bearer(&admin_token)
        .json(&json!({"records": [{"enrolment_id": enrolment_id, "status": "late"}]}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
    assert_eq!(attendance_records_count(&app.db, session_id).await, 0);
}

#[sqlx::test]
async fn attendance_put_unknown_session_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .put(&format!("/api/v1/sessions/{}/attendance", Uuid::now_v7()))
        .authorization_bearer(&admin_token)
        .json(&json!({"records": []}))
        .await;
    assert_eq!(resp.status_code(), 404, "body={}", resp.text());
}

#[sqlx::test]
async fn attendance_put_is_idempotent_and_overwrites_on_second_call(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("att-idem-coach@example.com", &["coach"])
        .await;
    let coach_id = seed_coach(&app.db, coach_user_id, "Idem Coach").await;
    let course_id = seed_course(&app.db, "Idempotent Course", Some(coach_id)).await;
    let session_id = seed_course_session(&app.db, course_id, yesterday(), t(9, 0), t(10, 0)).await;

    let member_a = app.register_member("att-idem-a@example.com", "Password!234").await;
    let member_b = app.register_member("att-idem-b@example.com", "Password!234").await;
    let enrolment_a =
        seed_enrolment(&app.db, member_a.user_id, course_id, "active", Utc::now()).await;
    let enrolment_b =
        seed_enrolment(&app.db, member_b.user_id, course_id, "active", Utc::now()).await;

    // First call: mark A present, B absent.
    let resp1 = app
        .put(&format!("/api/v1/sessions/{session_id}/attendance"))
        .authorization_bearer(&coach_token)
        .json(&json!({"records": [
            {"enrolment_id": enrolment_a, "status": "present"},
            {"enrolment_id": enrolment_b, "status": "absent"},
        ]}))
        .await;
    assert_eq!(resp1.status_code(), 200, "body={}", resp1.text());
    let body1: serde_json::Value = resp1.json();
    let arr1 = body1.as_array().unwrap();
    assert_eq!(
        arr1.iter().find(|e| e["enrolment_id"] == enrolment_a.to_string()).unwrap()
            ["attendance_status"],
        "present"
    );
    assert_eq!(
        arr1.iter().find(|e| e["enrolment_id"] == enrolment_b.to_string()).unwrap()
            ["attendance_status"],
        "absent"
    );
    assert_eq!(attendance_records_count(&app.db, session_id).await, 2);

    // Second call with the exact same body: idempotent, still 2 rows.
    let resp2 = app
        .put(&format!("/api/v1/sessions/{session_id}/attendance"))
        .authorization_bearer(&coach_token)
        .json(&json!({"records": [
            {"enrolment_id": enrolment_a, "status": "present"},
            {"enrolment_id": enrolment_b, "status": "absent"},
        ]}))
        .await;
    assert_eq!(resp2.status_code(), 200, "body={}", resp2.text());
    assert_eq!(attendance_records_count(&app.db, session_id).await, 2);

    // Third call overwrites both statuses — still 2 rows, new values.
    let resp3 = app
        .put(&format!("/api/v1/sessions/{session_id}/attendance"))
        .authorization_bearer(&coach_token)
        .json(&json!({"records": [
            {"enrolment_id": enrolment_a, "status": "leave"},
            {"enrolment_id": enrolment_b, "status": "present"},
        ]}))
        .await;
    assert_eq!(resp3.status_code(), 200, "body={}", resp3.text());
    let body3: serde_json::Value = resp3.json();
    let arr3 = body3.as_array().unwrap();
    assert_eq!(
        arr3.iter().find(|e| e["enrolment_id"] == enrolment_a.to_string()).unwrap()
            ["attendance_status"],
        "leave"
    );
    assert_eq!(
        arr3.iter().find(|e| e["enrolment_id"] == enrolment_b.to_string()).unwrap()
            ["attendance_status"],
        "present"
    );
    assert_eq!(
        attendance_records_count(&app.db, session_id).await,
        2,
        "overwrite must not create duplicate rows"
    );
}

// ---------------------------------------------------------------------------
// GET /coaches/me/students
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn my_students_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/coaches/me/students").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn my_students_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("att-students-member@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/coaches/me/students")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn my_students_as_coach_with_no_coach_row_returns_empty(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_user_id, token) = app
        .seed_user_with_roles("att-students-nocoach@example.com", &["coach"])
        .await;

    let resp = app
        .get("/api/v1/coaches/me/students")
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    assert_eq!(resp.json::<serde_json::Value>().as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn my_students_as_coach_returns_distinct_students_with_their_courses(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("att-students-coach@example.com", &["coach"])
        .await;
    let coach_id = seed_coach(&app.db, coach_user_id, "Students Coach").await;
    let course_a = seed_course(&app.db, "Students Course A", Some(coach_id)).await;
    let course_b = seed_course(&app.db, "Students Course B", Some(coach_id)).await;

    let (other_coach_user_id, _) = app
        .seed_user_with_roles("att-students-othercoach@example.com", &["coach"])
        .await;
    let other_coach_id = seed_coach(&app.db, other_coach_user_id, "Other Students Coach").await;
    let other_course = seed_course(&app.db, "Students Other Course", Some(other_coach_id)).await;

    // student_x is enrolled in both of this coach's courses -> one distinct
    // entry with two courses.
    let student_x = app.register_member("att-students-x@example.com", "Password!234").await;
    let enrolment_x_a =
        seed_enrolment(&app.db, student_x.user_id, course_a, "active", Utc::now()).await;
    let enrolment_x_b =
        seed_enrolment(&app.db, student_x.user_id, course_b, "active", Utc::now()).await;

    // student_y is cancelled in course_a -> must not appear.
    let student_y = app.register_member("att-students-y@example.com", "Password!234").await;
    seed_enrolment(&app.db, student_y.user_id, course_a, "cancelled", Utc::now()).await;

    // student_z is enrolled only in the other coach's course -> must not appear.
    let student_z = app.register_member("att-students-z@example.com", "Password!234").await;
    seed_enrolment(&app.db, student_z.user_id, other_course, "active", Utc::now()).await;

    let resp = app
        .get("/api/v1/coaches/me/students")
        .authorization_bearer(&coach_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array, not an envelope");
    assert_eq!(arr.len(), 1, "only student_x belongs to this coach's active courses, got {arr:?}");

    let entry = &arr[0];
    assert_eq!(entry["user_id"], student_x.user_id.to_string());
    assert_eq!(entry["name"], "Test Member");
    let courses = entry["courses"].as_array().expect("courses array");
    assert_eq!(courses.len(), 2, "student_x must show both of this coach's courses");
    let course_ids: Vec<String> =
        courses.iter().map(|c| c["course_id"].as_str().unwrap().to_string()).collect();
    assert!(course_ids.contains(&course_a.to_string()));
    assert!(course_ids.contains(&course_b.to_string()));

    let course_entry_a = courses
        .iter()
        .find(|c| c["course_id"] == course_a.to_string())
        .expect("course_a entry present");
    assert_eq!(
        course_entry_a["enrolment_id"], enrolment_x_a.to_string(),
        "course_a entry must carry student_x's enrolment_id for that course"
    );
    let course_entry_b = courses
        .iter()
        .find(|c| c["course_id"] == course_b.to_string())
        .expect("course_b entry present");
    assert_eq!(
        course_entry_b["enrolment_id"], enrolment_x_b.to_string(),
        "course_b entry must carry student_x's enrolment_id for that course"
    );
}

#[sqlx::test]
async fn my_students_excludes_inactive_course(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("att-students-inactive@example.com", &["coach"])
        .await;
    let coach_id = seed_coach(&app.db, coach_user_id, "Inactive Course Coach").await;
    let course_id = seed_course(&app.db, "Soon Inactive Course", Some(coach_id)).await;
    let student = app.register_member("att-students-inactive-s@example.com", "Password!234").await;
    seed_enrolment(&app.db, student.user_id, course_id, "active", Utc::now()).await;

    sqlx::query("UPDATE courses SET is_active = false WHERE id = $1")
        .bind(course_id)
        .execute(&app.db)
        .await
        .expect("deactivate course");

    let resp = app
        .get("/api/v1/coaches/me/students")
        .authorization_bearer(&coach_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    assert_eq!(resp.json::<serde_json::Value>().as_array().unwrap().len(), 0);
}

/// Sanity check that `Duration` import above is actually used (kept for a
/// possible future date-offset test) — currently unused would warn; use it
/// once here so the import doesn't need `#[allow(unused_imports)]`.
#[sqlx::test]
async fn future_session_still_appears_in_roster(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Future Session Course", None).await;
    let session_id = seed_course_session(
        &app.db,
        course_id,
        (Utc::now() + Duration::days(3)).date_naive(),
        t(9, 0),
        t(10, 0),
    )
    .await;

    let resp = app
        .get(&format!("/api/v1/sessions/{session_id}/roster"))
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
}

// ---------------------------------------------------------------------------
// PUT /sessions/{id}/attendance — "session already started" gate
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn attendance_put_future_session_returns_422_and_writes_nothing(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Attendance Future Course", None).await;
    let session_id = seed_course_session(
        &app.db,
        course_id,
        (Utc::now() + Duration::days(1)).date_naive(),
        t(9, 0),
        t(10, 0),
    )
    .await;
    let member = app.register_member("att-future-member@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .put(&format!("/api/v1/sessions/{session_id}/attendance"))
        .authorization_bearer(&admin_token)
        .json(&json!({"records": [{"enrolment_id": enrolment_id, "status": "present"}]}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
    assert_eq!(attendance_records_count(&app.db, session_id).await, 0);
}

#[sqlx::test]
async fn attendance_put_at_exact_start_boundary_returns_200(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "Attendance Boundary Course", None).await;
    let today = Utc::now().date_naive();
    let session_id = seed_course_session(&app.db, course_id, today, t(9, 0), t(10, 0)).await;
    let member = app.register_member("att-boundary-member@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;

    // studio_timezone is pinned to UTC in the test harness (see
    // common::http::test_app_config), so "today 09:00 studio-local" is
    // exactly "today 09:00 UTC" — no zone conversion needed here. Pin the
    // clock to the session's exact start instant: `require_started`'s
    // boundary is inclusive (rejects only `> now`), mirroring
    // `has_started`'s `<=` semantics — the moment a session starts, marking
    // it must already be allowed. Real wall-clock time never enters into
    // it, so this is deterministic at any hour the suite runs.
    app.clock.set(today.and_time(t(9, 0)).and_utc());

    let resp = app
        .put(&format!("/api/v1/sessions/{session_id}/attendance"))
        .authorization_bearer(&admin_token)
        .json(&json!({"records": [{"enrolment_id": enrolment_id, "status": "present"}]}))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
}
