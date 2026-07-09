//! Integration tests for `sessions::service` / `sessions::repository`.
//!
//! Covered paths:
//! - `materialize_range` is idempotent (repeat calls don't duplicate rows)
//! - `list_course_sessions` materializes and returns a course's sessions;
//!   404 on an unknown course; 422 on `to < from` or a >60-day span
//! - `my_weekly_schedule` includes only courses the caller holds an *active*
//!   enrolment in
//! - `today_sessions`: a coach sees only their own courses (empty if they
//!   have no `coaches` row), with a correct active-enrolment count; an
//!   admin sees every course

mod common;

use chrono::{Datelike, NaiveTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::extractors::auth::AuthUser;
use dream_fly_backend::modules::sessions::dto::SessionsRangeQuery;
use dream_fly_backend::modules::sessions::{repository as sessions_repository, service};

use common::fixtures::{
    seed_coach, seed_course, seed_course_schedule_slot, seed_course_schedule_slot_with_venue,
    seed_course_session, seed_enrolment,
};

fn auth_for(user_id: Uuid, roles: &[&str]) -> AuthUser {
    AuthUser {
        user_id,
        email: format!("{user_id}@example.com"),
        roles: roles.iter().map(|r| (*r).to_string()).collect(),
    }
}

/// PostgreSQL `EXTRACT(DOW)` / this module's `day_of_week` convention:
/// 0=Sunday .. 6=Saturday.
fn dow_of(date: chrono::NaiveDate) -> i16 {
    date.weekday().num_days_from_sunday() as i16
}

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

#[sqlx::test]
async fn materialize_range_is_idempotent(db: PgPool) {
    let course_id = seed_course(&db, "Materialize Course", None).await;
    let today = Utc::now().date_naive();
    seed_course_schedule_slot(&db, course_id, dow_of(today), t(9, 0), t(10, 0)).await;

    sessions_repository::materialize_range(&db, &[course_id], today, today)
        .await
        .expect("first materialize");
    let count_1: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM course_sessions WHERE course_id = $1")
            .bind(course_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(count_1, 1);

    sessions_repository::materialize_range(&db, &[course_id], today, today)
        .await
        .expect("second materialize");
    let count_2: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM course_sessions WHERE course_id = $1")
            .bind(course_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        count_2, 1,
        "repeat materialize_range calls must not duplicate sessions"
    );
}

#[sqlx::test]
async fn list_course_sessions_materializes_todays_slot(db: PgPool) {
    // Server config pinned to UTC (common::test_server_config), so the
    // service's studio-local "today" equals the UTC date used for seeding.
    let course_id = seed_course(&db, "Weekly Course", None).await;
    let today = Utc::now().date_naive();
    seed_course_schedule_slot(&db, course_id, dow_of(today), t(9, 0), t(10, 0)).await;

    let sessions = service::list_course_sessions(
        &db,
        &common::test_server_config(),
        course_id,
        SessionsRangeQuery { from: None, to: None },
    )
    .await
    .expect("list");

    assert!(
        sessions.iter().any(|s| s.session_date == today
            && s.start_time == t(9, 0)
            && s.end_time == t(10, 0)),
        "today's weekly slot should have materialized into a session, got {sessions:?}"
    );
}

#[sqlx::test]
async fn list_course_sessions_nonexistent_course_returns_not_found(db: PgPool) {
    let err = service::list_course_sessions(
        &db,
        &common::test_server_config(),
        Uuid::now_v7(),
        SessionsRangeQuery { from: None, to: None },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn list_course_sessions_rejects_to_before_from(db: PgPool) {
    let course_id = seed_course(&db, "Range Course A", None).await;
    let err = service::list_course_sessions(
        &db,
        &common::test_server_config(),
        course_id,
        SessionsRangeQuery {
            from: Some("2026-08-01".into()),
            to: Some("2026-06-01".into()),
        },
    )
    .await
    .unwrap_err();
    match err {
        AppError::Validation(_) => {}
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[sqlx::test]
async fn list_course_sessions_rejects_range_over_60_days(db: PgPool) {
    let course_id = seed_course(&db, "Range Course B", None).await;
    let err = service::list_course_sessions(
        &db,
        &common::test_server_config(),
        course_id,
        SessionsRangeQuery {
            from: Some("2026-01-01".into()),
            to: Some("2026-12-31".into()),
        },
    )
    .await
    .unwrap_err();
    match err {
        AppError::Validation(_) => {}
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[sqlx::test]
async fn list_course_sessions_allows_exactly_60_days(db: PgPool) {
    // Boundary check: a 60-day span itself must NOT be rejected (only
    // spans strictly greater than 60 days should 422).
    let course_id = seed_course(&db, "Range Course C", None).await;
    service::list_course_sessions(
        &db,
        &common::test_server_config(),
        course_id,
        SessionsRangeQuery {
            from: Some("2026-01-01".into()),
            to: Some("2026-03-02".into()), // exactly 60 days after Jan 1
        },
    )
    .await
    .expect("60-day span must be accepted");
}

#[sqlx::test]
async fn my_weekly_schedule_only_includes_active_enrolments(db: PgPool) {
    let user_id = common::seed_member(&db, "sched-me@example.com", "hunter22-secret").await;
    let active_course = seed_course(&db, "Active Enrolled Course", None).await;
    let cancelled_course = seed_course(&db, "Cancelled Enrolled Course", None).await;
    let not_enrolled_course = seed_course(&db, "Not Enrolled Course", None).await;

    seed_course_schedule_slot(&db, active_course, 1, t(9, 0), t(10, 0)).await;
    seed_course_schedule_slot(&db, cancelled_course, 2, t(9, 0), t(10, 0)).await;
    seed_course_schedule_slot(&db, not_enrolled_course, 3, t(9, 0), t(10, 0)).await;

    seed_enrolment(&db, user_id, active_course, "active", Utc::now()).await;
    seed_enrolment(&db, user_id, cancelled_course, "cancelled", Utc::now()).await;

    let schedule = service::my_weekly_schedule(&db, user_id)
        .await
        .expect("my schedule");
    let course_ids: Vec<Uuid> = schedule.iter().map(|e| e.course_id).collect();
    assert_eq!(
        course_ids,
        vec![active_course],
        "only the active enrolment's course should appear"
    );
    assert_eq!(schedule[0].day_of_week, 1);
}

#[sqlx::test]
async fn today_sessions_coach_sees_only_own_courses_with_enrolled_count(db: PgPool) {
    let coach_user = common::seed_member(&db, "coach-today@example.com", "hunter22-secret").await;
    let coach_id = seed_coach(&db, coach_user, "Coach Today").await;
    let own_course = seed_course(&db, "Own Course Today", Some(coach_id)).await;
    let other_course = seed_course(&db, "Other Course Today", None).await;

    let today = Utc::now().date_naive();
    let dow = dow_of(today);
    seed_course_schedule_slot(&db, own_course, dow, t(9, 0), t(10, 0)).await;
    seed_course_schedule_slot(&db, other_course, dow, t(9, 0), t(10, 0)).await;

    // Two active enrolments + one cancelled on own_course -> enrolled_count
    // must be 2, not 3.
    let m1 = common::seed_member(&db, "m1-today@example.com", "hunter22-secret").await;
    let m2 = common::seed_member(&db, "m2-today@example.com", "hunter22-secret").await;
    let m3 = common::seed_member(&db, "m3-today@example.com", "hunter22-secret").await;
    seed_enrolment(&db, m1, own_course, "active", Utc::now()).await;
    seed_enrolment(&db, m2, own_course, "active", Utc::now()).await;
    seed_enrolment(&db, m3, own_course, "cancelled", Utc::now()).await;

    let auth = auth_for(coach_user, &["coach"]);
    let sessions = service::today_sessions(&db, &common::test_server_config(), &auth)
        .await
        .expect("today sessions");

    assert_eq!(
        sessions.len(),
        1,
        "coach must only see their own course's session today, got {sessions:?}"
    );
    assert_eq!(sessions[0].course_id, own_course);
    assert_eq!(sessions[0].enrolled_count, 2);
}

#[sqlx::test]
async fn today_sessions_coach_role_without_coach_row_returns_empty(db: PgPool) {
    // A user with the "coach" role but no matching `coaches` row (data
    // anomaly) must get an empty list, not an error.
    let user_id = common::seed_member(&db, "phantom-coach@example.com", "hunter22-secret").await;
    let auth = auth_for(user_id, &["coach"]);
    let sessions = service::today_sessions(&db, &common::test_server_config(), &auth)
        .await
        .expect("today sessions");
    assert!(sessions.is_empty());
}

#[sqlx::test]
async fn today_sessions_coach_name_present_with_coach_and_null_without(db: PgPool) {
    let coach_user = common::seed_member(&db, "coach-name-today@example.com", "hunter22-secret").await;
    sqlx::query("UPDATE users SET name = $2 WHERE id = $1")
        .bind(coach_user)
        .bind("Today Coach Display Name")
        .execute(&db)
        .await
        .expect("rename coach user");
    let coach_id = seed_coach(&db, coach_user, "Today Coach Title").await;
    let with_coach = seed_course(&db, "Course With Coach Today", Some(coach_id)).await;
    let without_coach = seed_course(&db, "Course Without Coach Today", None).await;

    let today = Utc::now().date_naive();
    let dow = dow_of(today);
    seed_course_schedule_slot(&db, with_coach, dow, t(9, 0), t(10, 0)).await;
    seed_course_schedule_slot(&db, without_coach, dow, t(11, 0), t(12, 0)).await;

    let admin_id = common::seed_member(&db, "coach-name-admin@example.com", "hunter22-secret").await;
    let auth = auth_for(admin_id, &["admin"]);
    let sessions = service::today_sessions(&db, &common::test_server_config(), &auth)
        .await
        .expect("today sessions");

    let with_row = sessions.iter().find(|s| s.course_id == with_coach).expect("with_coach session");
    assert_eq!(with_row.coach_name.as_deref(), Some("Today Coach Display Name"));

    let without_row =
        sessions.iter().find(|s| s.course_id == without_coach).expect("without_coach session");
    assert_eq!(without_row.coach_name, None, "course has no coach_id -> null");
}

#[sqlx::test]
async fn today_sessions_venue_resolves_when_slot_matches(db: PgPool) {
    let course_id = seed_course(&db, "Venue Match Course Today", None).await;
    let today = Utc::now().date_naive();
    let dow = dow_of(today);
    seed_course_schedule_slot_with_venue(&db, course_id, dow, t(9, 0), t(10, 0), "Main Hall").await;

    let admin_id = common::seed_member(&db, "venue-match-admin@example.com", "hunter22-secret").await;
    let auth = auth_for(admin_id, &["admin"]);
    let sessions = service::today_sessions(&db, &common::test_server_config(), &auth)
        .await
        .expect("today sessions");

    let row = sessions.iter().find(|s| s.course_id == course_id).expect("session present");
    assert_eq!(row.venue.as_deref(), Some("Main Hall"));
}

#[sqlx::test]
async fn today_sessions_venue_is_null_when_no_matching_slot(db: PgPool) {
    // A materialized session with no corresponding `course_schedule_slots`
    // row at all — covers both stated causes in the brief ("slot 改過/無
    // slot"): whether the slot was edited away or never existed, the LEFT
    // JOIN finds nothing either way, so this single setup exercises the
    // exact same code path as a since-changed slot.
    let course_id = seed_course(&db, "Venue No Match Course Today", None).await;
    let today = Utc::now().date_naive();
    seed_course_session(&db, course_id, today, t(9, 0), t(10, 0)).await;

    let admin_id = common::seed_member(&db, "venue-no-match-admin@example.com", "hunter22-secret").await;
    let auth = auth_for(admin_id, &["admin"]);
    let sessions = service::today_sessions(&db, &common::test_server_config(), &auth)
        .await
        .expect("today sessions");

    let row = sessions.iter().find(|s| s.course_id == course_id).expect("session present");
    assert_eq!(row.venue, None);
}

#[sqlx::test]
async fn today_sessions_admin_sees_all_courses(db: PgPool) {
    let coach_user =
        common::seed_member(&db, "coach-admin-today@example.com", "hunter22-secret").await;
    let coach_id = seed_coach(&db, coach_user, "Coach").await;
    let course_a = seed_course(&db, "Course A Admin Today", Some(coach_id)).await;
    let course_b = seed_course(&db, "Course B Admin Today", None).await;

    let today = Utc::now().date_naive();
    let dow = dow_of(today);
    seed_course_schedule_slot(&db, course_a, dow, t(9, 0), t(10, 0)).await;
    seed_course_schedule_slot(&db, course_b, dow, t(9, 0), t(10, 0)).await;

    let admin_id = common::seed_member(&db, "admin-today@example.com", "hunter22-secret").await;
    let auth = auth_for(admin_id, &["admin"]);
    let sessions = service::today_sessions(&db, &common::test_server_config(), &auth)
        .await
        .expect("admin today sessions");

    let ids: Vec<Uuid> = sessions.iter().map(|s| s.course_id).collect();
    assert!(ids.contains(&course_a), "admin must see course_a, got {ids:?}");
    assert!(ids.contains(&course_b), "admin must see course_b, got {ids:?}");
}
