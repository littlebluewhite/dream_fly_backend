//! Studio-timezone (Asia/Taipei, UTC+8) behaviour of date-sensitive endpoints,
//! pinned at an instant where the Taipei calendar day is already one ahead of
//! the UTC day: 2026-07-14 16:30Z = 2026-07-15 00:30 Taipei. A service that
//! read the wrong clock or the UTC calendar would answer differently, so each
//! test would fail on it. Fixtures take their dates from `app.today()`.

mod common;

use chrono::{Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use common::fixtures::{
    TimeSlotSeed, seed_booking, seed_course, seed_course_session, seed_enrolment,
};
use common::http::{TestApp, spawn_test_app_with};
use dream_fly_backend::modules::bookings::model::BookingStatus;
use dream_fly_backend::modules::enrolments::model::EnrolmentStatus;
use serde_json::json;
use sqlx::PgPool;

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

/// A Taipei app whose clock is pinned to Taipei 2026-07-15 00:30 (UTC 07-14 16:30).
async fn taipei_app_at_half_past_midnight(db: PgPool) -> TestApp {
    let app = spawn_test_app_with(db, |cfg| {
        cfg.server.studio_timezone = "Asia/Taipei".parse().unwrap();
    })
    .await;
    app.clock.set(Utc.with_ymd_and_hms(2026, 7, 14, 16, 30, 0).unwrap());
    assert_eq!(app.today(), NaiveDate::from_ymd_opt(2026, 7, 15).unwrap());
    app
}

async fn cancel_status(app: &TestApp, token: &str, booking_id: uuid::Uuid) -> u16 {
    app.patch(&format!("/api/v1/bookings/{booking_id}/cancel"))
        .authorization_bearer(token)
        .await
        .status_code()
        .as_u16()
}

/// 24h cancel window is measured in studio wall-clock: a slot at Taipei
/// 07-15 20:00 is 19.5h away (rejected; read as UTC it would be 27.5h and
/// slip through), while 07-16 01:00 Taipei is 24.5h away (allowed).
#[sqlx::test]
async fn venue_cancel_window_uses_taipei_wall_clock(db: PgPool) {
    let app = taipei_app_at_half_past_midnight(db).await;
    let user = app.register_member("taipei-cancel@example.com", "Password!234").await;

    let inside =
        TimeSlotSeed::new(5, app.today()).on(app.today()).start(t(20, 0)).insert(&app.db).await;
    let outside = TimeSlotSeed::new(5, app.today())
        .on(app.today() + Duration::days(1))
        .start(t(1, 0))
        .insert(&app.db)
        .await;
    let b_inside = seed_booking(&app.db, user.user_id, inside, BookingStatus::Confirmed, 1000).await;
    let b_outside =
        seed_booking(&app.db, user.user_id, outside, BookingStatus::Confirmed, 1000).await;

    assert_eq!(cancel_status(&app, &user.access_token, b_inside).await, 400);
    assert_eq!(cancel_status(&app, &user.access_token, b_outside).await, 200);
}

/// "Session already started" is judged in studio wall-clock: Taipei 07-15
/// 00:10 began 20 minutes ago (422; read as UTC it would still be 16h away),
/// while Taipei 07-15 09:00 has not started (200).
#[sqlx::test]
async fn leave_request_session_started_uses_taipei_wall_clock(db: PgPool) {
    let app = taipei_app_at_half_past_midnight(db).await;
    let user = app.register_member("taipei-leave@example.com", "Password!234").await;
    let course = seed_course(&app.db, "Taipei Leave", None).await;
    seed_enrolment(&app.db, user.user_id, course, EnrolmentStatus::Active, app.studio_now().now).await;
    let started = seed_course_session(&app.db, course, app.today(), t(0, 10), t(1, 10)).await;
    let upcoming = seed_course_session(&app.db, course, app.today(), t(9, 0), t(10, 0)).await;

    let resp = app
        .post("/api/v1/leave-requests")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "session_id": started }))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());

    let resp = app
        .post("/api/v1/leave-requests")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "session_id": upcoming }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
}

/// Just after Taipei midnight, "today's sessions" is the Taipei date (07-15),
/// not the UTC date (07-14).
#[sqlx::test]
async fn sessions_today_after_taipei_midnight_lists_taipei_date(db: PgPool) {
    let app = taipei_app_at_half_past_midnight(db).await;
    let coach = app.seed_coach_user().await;
    let course = seed_course(&app.db, "Taipei Today", Some(coach.coach_id)).await;
    let today_session = seed_course_session(&app.db, course, app.today(), t(9, 0), t(10, 0)).await;
    let utc_day = app.today() - Duration::days(1);
    seed_course_session(&app.db, course, utc_day, t(9, 0), t(10, 0)).await;

    let resp = app.get("/api/v1/sessions/today").authorization_bearer(&coach.token).await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let ids: Vec<&str> = body.as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec![today_session.to_string()]);
}

/// The coach report's "今天" is likewise the Taipei date.
#[sqlx::test]
async fn coach_report_today_sessions_uses_taipei_date(db: PgPool) {
    let app = taipei_app_at_half_past_midnight(db).await;
    let coach = app.seed_coach_user().await;
    let course = seed_course(&app.db, "Taipei Report", Some(coach.coach_id)).await;
    seed_course_session(&app.db, course, app.today(), t(9, 0), t(10, 0)).await;
    seed_course_session(&app.db, course, app.today(), t(11, 0), t(12, 0)).await;
    seed_course_session(&app.db, course, app.today() - Duration::days(1), t(9, 0), t(10, 0)).await;

    let resp = app.get("/api/v1/reports/coach").authorization_bearer(&coach.token).await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["today_sessions"], 2);
}
