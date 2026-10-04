//! Studio-timezone (Asia/Taipei, UTC+8) behaviour of date-sensitive endpoints,
//! pinned at an instant where the Taipei calendar day is already one ahead of
//! the UTC day: 2026-07-14 16:30Z = 2026-07-15 00:30 Taipei. A service that
//! read the wrong clock or the UTC calendar would answer differently, so each
//! test would fail on it. Fixtures take their dates from `app.today()`.

mod common;

use chrono::{Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use common::fixtures::{
    TimeSlotSeed, seed_point_ledger_entry, seed_booking, seed_course, seed_course_session, seed_enrolment,
};
use common::http::{TestApp, spawn_test_app_with};
use dream_fly_backend::modules::bookings::model::BookingStatus;
use dream_fly_backend::modules::enrolments::model::EnrolmentStatus;
use dream_fly_backend::modules::points::model::PointReason;
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

/// `earned_this_month` follows the studio month, not the UTC month: at Taipei
/// 2026-07-15 the month began at 2026-06-30 16:00Z, so a row one second before
/// that is June, and one at 2026-07-31 16:00Z is already August.
#[sqlx::test]
async fn points_me_earned_this_month_uses_studio_month_boundaries(db: PgPool) {
    let app = taipei_app_at_half_past_midnight(db).await;
    let user = app.register_member("pts-taipei@example.com", "Password!234").await;
    let at = |y, mo, d, h, mi, s| Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap();
    let rows = [
        (at(2026, 6, 30, 15, 59, 59), PointReason::CheckoutEarn, 1000), // Taipei June 30: excluded
        (at(2026, 6, 30, 16, 0, 0), PointReason::CheckoutEarn, 1),      // Taipei July 1 00:00: in
        (at(2026, 7, 14, 16, 0, 0), PointReason::CheckoutEarn, 20),     // in
        (at(2026, 7, 14, 16, 0, 0), PointReason::RefundClawback, -5),   // other reason: excluded
        (at(2026, 7, 14, 16, 0, 0), PointReason::AdminAdjust, 300),     // other reason: excluded
        (at(2026, 7, 31, 16, 0, 0), PointReason::CheckoutEarn, 2000),   // Taipei Aug 1: excluded
    ];
    for (created_at, reason, delta) in rows {
        seed_point_ledger_entry(&app.db, user.user_id, delta, delta, reason, None, created_at)
            .await;
    }

    let resp = app
        .get("/api/v1/points/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200);
    assert_eq!(resp.json::<serde_json::Value>()["earned_this_month"], 21);
}

/// ADR-0017 W7-5:`point_ledger.created_at` 是業務時間(`earned_this_month` 依它
/// 分桶),真的走結帳時必須蓋上 handler 取樣的 now,而不是 DB 的 `NOW()`。
#[sqlx::test]
async fn checkout_points_earned_lands_in_studio_month(db: PgPool) {
    let app = taipei_app_at_half_past_midnight(db).await;
    let user = app
        .register_member("pts-checkout@example.com", "Password!234")
        .await;
    let (_admin, admin_token) = app.seed_admin().await;
    let product: serde_json::Value = app
        .post("/api/v1/products")
        .authorization_bearer(&admin_token)
        .json(&json!({
            "name": "Points Mug",
            "product_type": "merchandise",
            "price_cents": 100000,
            "stock": 10,
        }))
        .await
        .json();
    app.post("/api/v1/cart/items")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "item_type": "product", "item_id": product["id"], "quantity": 1 }))
        .await;

    let order = app
        .post("/api/v1/orders")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(order.status_code(), 200, "body={}", order.text());
    let points_earned = order.json::<serde_json::Value>()["points_earned"]
        .as_i64()
        .unwrap();
    assert!(points_earned > 0);

    let resp = app
        .get("/api/v1/points/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200);
    assert_eq!(
        resp.json::<serde_json::Value>()["earned_this_month"],
        points_earned
    );
}
