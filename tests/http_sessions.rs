//! HTTP integration tests for the sessions module's endpoints:
//! `GET /courses/{id}/sessions`, `GET /sessions/today`, `GET /schedule/me`.

mod common;

use chrono::{Datelike, Duration, NaiveTime, Utc};
use common::fixtures::{seed_coach, seed_course, seed_course_schedule_slot, seed_enrolment};
use common::http::spawn_test_app;
use sqlx::PgPool;
use uuid::Uuid;

fn dow_of(date: chrono::NaiveDate) -> i16 {
    date.weekday().num_days_from_sunday() as i16
}

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

// ---------------------------------------------------------------------------
// GET /courses/{id}/sessions
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn course_sessions_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .get(&format!("/api/v1/courses/{}/sessions", Uuid::now_v7()))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn course_sessions_unknown_course_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("sess-404@example.com", "Password!234").await;

    let resp = app
        .get(&format!("/api/v1/courses/{}/sessions", Uuid::now_v7()))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 404, "body={}", resp.text());
}

#[sqlx::test]
async fn course_sessions_default_range_materializes_todays_slot(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("sess-default@example.com", "Password!234").await;
    let course_id = seed_course(&app.db, "Sessions Default Range Course", None).await;

    let today = Utc::now().date_naive();
    seed_course_schedule_slot(&app.db, course_id, dow_of(today), t(9, 0), t(10, 0)).await;

    let resp = app
        .get(&format!("/api/v1/courses/{course_id}/sessions"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array, not an envelope");
    assert!(
        arr.iter().any(|s| s["session_date"] == today.to_string()
            && s["start_time"] == "09:00:00"
            && s["end_time"] == "10:00:00"
            && s["course_id"] == course_id.to_string()
            && s["id"].as_str().is_some()),
        "expected today's materialized session in {arr:?}"
    );

    // Calling again must not duplicate rows (materialize idempotency at the
    // HTTP layer; the repository-level row-count assertion lives in
    // service_sessions.rs::materialize_range_is_idempotent).
    let resp2 = app
        .get(&format!("/api/v1/courses/{course_id}/sessions"))
        .authorization_bearer(&user.access_token)
        .await;
    let body2: serde_json::Value = resp2.json();
    assert_eq!(body2.as_array().unwrap().len(), arr.len());
}

#[sqlx::test]
async fn course_sessions_to_before_from_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("sess-422a@example.com", "Password!234").await;
    let course_id = seed_course(&app.db, "Sessions 422 Course A", None).await;

    let resp = app
        .get(&format!(
            "/api/v1/courses/{course_id}/sessions?from=2026-08-01&to=2026-06-01"
        ))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

#[sqlx::test]
async fn course_sessions_range_over_60_days_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("sess-422b@example.com", "Password!234").await;
    let course_id = seed_course(&app.db, "Sessions 422 Course B", None).await;

    let resp = app
        .get(&format!(
            "/api/v1/courses/{course_id}/sessions?from=2026-01-01&to=2026-12-31"
        ))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

// ---------------------------------------------------------------------------
// GET /sessions/today
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn today_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/sessions/today").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn today_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("sess-today-member@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/sessions/today")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn today_as_coach_returns_own_course_with_enrolled_count(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("sess-today-coach@example.com", &["coach"])
        .await;
    let coach_id = seed_coach(&app.db, coach_user_id, "Today Coach").await;
    let own_course = seed_course(&app.db, "HTTP Today Own Course", Some(coach_id)).await;
    let other_course = seed_course(&app.db, "HTTP Today Other Course", None).await;

    let today = Utc::now().date_naive();
    let dow = dow_of(today);
    seed_course_schedule_slot(&app.db, own_course, dow, t(9, 0), t(10, 0)).await;
    seed_course_schedule_slot(&app.db, other_course, dow, t(9, 0), t(10, 0)).await;

    let member = app.register_member("sess-today-member2@example.com", "Password!234").await;
    seed_enrolment(&app.db, member.user_id, own_course, "active", Utc::now()).await;

    let resp = app
        .get("/api/v1/sessions/today")
        .authorization_bearer(&coach_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array");
    assert_eq!(
        arr.len(),
        1,
        "coach must only see their own course's session, got {arr:?}"
    );
    assert_eq!(arr[0]["course_id"], own_course.to_string());
    assert_eq!(arr[0]["course_name"], "HTTP Today Own Course");
    assert_eq!(arr[0]["enrolled_count"], 1);
    assert!(arr[0]["id"].as_str().is_some());
}

#[sqlx::test]
async fn today_as_admin_returns_all_courses(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let course_a = seed_course(&app.db, "HTTP Today Admin Course A", None).await;
    let course_b = seed_course(&app.db, "HTTP Today Admin Course B", None).await;
    let today = Utc::now().date_naive();
    let dow = dow_of(today);
    seed_course_schedule_slot(&app.db, course_a, dow, t(9, 0), t(10, 0)).await;
    seed_course_schedule_slot(&app.db, course_b, dow, t(14, 0), t(15, 0)).await;

    let resp = app
        .get("/api/v1/sessions/today")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array");
    let ids: Vec<String> = arr
        .iter()
        .map(|s| s["course_id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&course_a.to_string()));
    assert!(ids.contains(&course_b.to_string()));
}

// ---------------------------------------------------------------------------
// GET /schedule/me
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn schedule_me_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/schedule/me").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn schedule_me_returns_only_active_enrolment_courses(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("sched-me-http@example.com", "Password!234").await;

    let coach_user = common::seed_member(&app.db, "sched-me-coach@example.com", "hunter22-secret")
        .await;
    let coach_id = seed_coach(&app.db, coach_user, "Schedule Coach").await;
    let active_course =
        seed_course(&app.db, "HTTP Schedule Active Course", Some(coach_id)).await;
    let cancelled_course =
        seed_course(&app.db, "HTTP Schedule Cancelled Course", None).await;

    seed_course_schedule_slot(&app.db, active_course, 1, t(19, 0), t(20, 0)).await;
    seed_course_schedule_slot(&app.db, cancelled_course, 2, t(19, 0), t(20, 0)).await;

    seed_enrolment(&app.db, user.user_id, active_course, "active", Utc::now()).await;
    seed_enrolment(
        &app.db,
        user.user_id,
        cancelled_course,
        "cancelled",
        Utc::now() - Duration::days(1),
    )
    .await;

    let resp = app
        .get("/api/v1/schedule/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array");
    assert_eq!(
        arr.len(),
        1,
        "only the active enrolment's course slot should appear, got {arr:?}"
    );
    assert_eq!(arr[0]["course_id"], active_course.to_string());
    assert_eq!(arr[0]["course_name"], "HTTP Schedule Active Course");
    assert_eq!(arr[0]["coach_name"], "Test Member");
    assert_eq!(arr[0]["day_of_week"], 1);
    assert_eq!(arr[0]["start_time"], "19:00:00");
    assert_eq!(arr[0]["end_time"], "20:00:00");
    assert!(arr[0]["venue"].is_null());
}
