//! Harness smoke test. Verifies that `spawn_test_app` can build the full
//! router and that a call passes rate-limit middleware using the synthetic
//! X-Forwarded-For header, and pins the scene instant (`common::scene_now`)
//! and the fixtures that write business time at the caller's instant.
//! The `/health` body lives in `tests/http_health.rs`.

mod common;

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, SubsecRound, TimeZone, Utc};
use common::fixtures::{
    OrderSeed, seed_course, seed_full_course, seed_leave_scene, seed_marked_attendance,
};
use common::http::spawn_test_app;
use dream_fly_backend::modules::attendance::model::AttendanceStatus;
use dream_fly_backend::modules::leave::model::LeaveStatus;
use dream_fly_backend::modules::orders::model::OrderStatus;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn harness_missing_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    // `/users/me` requires AuthUser. Without a token we expect 401.
    let resp = app.get("/api/v1/users/me").await;
    assert_eq!(resp.status_code(), 401);
}

/// The scene instant is in UTC and already at Postgres precision, so an
/// instant written to a `timestamptz` column reads back equal.
#[sqlx::test]
async fn scene_now_is_utc_and_survives_a_timestamptz_round_trip(db: PgPool) {
    let scene = common::scene_now();
    assert_eq!(scene.tz, chrono_tz::UTC);

    let stored: DateTime<Utc> = sqlx::query_scalar("SELECT $1::timestamptz")
        .bind(scene.now)
        .fetch_one(&db)
        .await
        .expect("round-trip scene instant");
    assert_eq!(stored, scene.now);
}

/// `TestApp` opens with its clock pinned at the scene instant: it does not
/// move on its own, and `advance` moves it without a prior `set`.
#[sqlx::test]
async fn test_app_opens_with_the_clock_pinned(db: PgPool) {
    let app = spawn_test_app(db).await;
    let opened = app.studio_now().now;
    assert_eq!(opened, opened.trunc_subsecs(6), "pinned at DB precision");

    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    assert_eq!(app.studio_now().now, opened, "the clock moved on its own");

    app.clock.advance(Duration::minutes(5));
    assert_eq!(app.studio_now().now, opened + Duration::minutes(5));
}

/// The users `TestApp` seeds join at the app's "now", not the wall clock.
#[sqlx::test]
async fn test_app_seeded_users_join_at_the_apps_now(db: PgPool) {
    let app = spawn_test_app(db).await;
    let t = Utc.with_ymd_and_hms(2020, 1, 15, 3, 0, 0).unwrap();
    app.clock.set(t);

    let (admin_id, _) = app.seed_admin().await;
    let coach = app.seed_coach_user().await;

    for user_id in [admin_id, coach.user_id] {
        let created_at: DateTime<Utc> =
            sqlx::query_scalar("SELECT created_at FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_one(&app.db)
                .await
                .expect("load users.created_at");
        assert_eq!(created_at, t);
    }
}

/// `users.created_at` of `user_id`.
async fn joined_at(db: &PgPool, user_id: Uuid) -> DateTime<Utc> {
    sqlx::query_scalar("SELECT created_at FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
        .expect("load users.created_at")
}

/// Every `enrolments.enrolled_at` in `course_id`.
async fn enrolled_at_in(db: &PgPool, course_id: Uuid) -> Vec<DateTime<Utc>> {
    sqlx::query_scalar("SELECT enrolled_at FROM enrolments WHERE course_id = $1")
        .bind(course_id)
        .fetch_all(db)
        .await
        .expect("load enrolments.enrolled_at")
}

/// The leave scene enrols at the caller's instant and puts the session on
/// the next studio day.
#[sqlx::test]
async fn leave_scene_is_set_at_the_callers_instant(db: PgPool) {
    let at = common::studio_now_utc(Utc.with_ymd_and_hms(2020, 1, 15, 3, 0, 0).unwrap());
    let member = common::seed_member(&db, "leave-scene@example.com", "Password!234").await;
    let course = seed_course(&db, "Leave Scene", None).await;

    let scene = seed_leave_scene(&db, member, course, LeaveStatus::Pending, None, at).await;

    assert_eq!(enrolled_at_in(&db, course).await, vec![at.now]);
    let session_date: NaiveDate =
        sqlx::query_scalar("SELECT session_date FROM course_sessions WHERE id = $1")
            .bind(scene.session)
            .fetch_one(&db)
            .await
            .expect("load course_sessions.session_date");
    assert_eq!(session_date, NaiveDate::from_ymd_opt(2020, 1, 16).unwrap());
}

/// The marked-attendance scene's member joins and enrols at the caller's
/// instant.
#[sqlx::test]
async fn marked_attendance_is_set_at_the_callers_instant(db: PgPool) {
    let now = Utc.with_ymd_and_hms(2020, 1, 15, 3, 0, 0).unwrap();
    let course = seed_course(&db, "Marked Attendance", None).await;
    let session_date = NaiveDate::from_ymd_opt(2020, 1, 10).unwrap();
    let start = NaiveTime::from_hms_opt(9, 0, 0).unwrap();

    let scene = seed_marked_attendance(
        &db,
        course,
        session_date,
        start,
        AttendanceStatus::Present,
        now,
    )
    .await;

    assert_eq!(enrolled_at_in(&db, course).await, vec![now]);
    assert_eq!(joined_at(&db, scene.member).await, now);
}

/// A full course's occupants join and enrol at the caller's instant.
#[sqlx::test]
async fn full_course_is_set_at_the_callers_instant(db: PgPool) {
    let now = Utc.with_ymd_and_hms(2020, 1, 15, 3, 0, 0).unwrap();

    let full = seed_full_course(&db, "Full Course", 2, now).await;

    assert_eq!(enrolled_at_in(&db, full.course).await, vec![now, now]);
    for member in full.occupants {
        assert_eq!(joined_at(&db, member).await, now);
    }
}

/// An order with no `paid_at` is created at the instant the caller gives.
#[sqlx::test]
async fn unpaid_order_seed_is_created_at_the_given_instant(db: PgPool) {
    let t = Utc.with_ymd_and_hms(2020, 1, 15, 3, 0, 0).unwrap();
    let buyer = common::seed_member(&db, "unpaid-order@example.com", "Password!234").await;

    let order_id = OrderSeed::new(buyer, OrderStatus::Pending)
        .created_at(t)
        .insert(&db)
        .await;

    let created_at: DateTime<Utc> =
        sqlx::query_scalar("SELECT created_at FROM orders WHERE id = $1")
            .bind(order_id)
            .fetch_one(&db)
            .await
            .expect("load orders.created_at");
    assert_eq!(created_at, t);
}

/// An order with neither `paid_at` nor `created_at` has no instant to be
/// created at, and the panic says how to fix the call. The check runs before
/// any write, so the pool never connects (a panicking `#[sqlx::test]` would
/// leave its database behind).
#[tokio::test]
#[should_panic(expected = "call .created_at(t)")]
async fn unpaid_order_seed_without_an_instant_names_the_fix() {
    let db = PgPool::connect_lazy("postgres://unused").expect("lazy pool");
    OrderSeed::new(Uuid::now_v7(), OrderStatus::Pending)
        .insert(&db)
        .await;
}
