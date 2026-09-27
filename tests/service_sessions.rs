//! Integration tests for `sessions::service` / `sessions::repository` /
//! `sessions::calendar`.
//!
//! Covered paths:
//! - `materialize_range` is idempotent (repeat calls don't duplicate rows)
//!   and snapshots the slot's `venue` onto the session at creation time
//! - materialize and reconcile agree on the slot↔session correspondence:
//!   reconciling right after materializing touches no row
//! - `list_course_sessions` materializes and returns a course's sessions;
//!   404 on an unknown course; 422 on `to < from` or a >60-day span
//! - `my_weekly_schedule` includes only courses the caller holds an *active*
//!   enrolment in
//! - `today_sessions`: a coach sees only their own courses (empty if they
//!   have no `coaches` row), with a correct active-enrolment count; an
//!   admin sees every course; a session's `venue` is its own snapshot, kept
//!   even after its slot's start_time is edited away
//! - `today_sessions` materializes today's slot itself when no session row
//!   was pre-seeded (the slot-only guard on the materialize-before-read wire)

mod common;

use chrono::{Datelike, NaiveTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::sessions::dto::SessionsRangeQuery;
use dream_fly_backend::modules::sessions::{calendar, service};

use common::fixtures::{
    seed_coach, seed_course, seed_course_schedule_slot, seed_course_schedule_slot_with_venue,
    seed_course_session, seed_enrolment, seed_session_scene,
};

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

    calendar::materialize_range(&db, &[course_id], today, today)
        .await
        .expect("first materialize");
    let count_1: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM course_sessions WHERE course_id = $1")
            .bind(course_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(count_1, 1);

    calendar::materialize_range(&db, &[course_id], today, today)
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
async fn materialize_snapshots_slot_venue(db: PgPool) {
    let course_id = seed_course(&db, "Venue Snapshot Course", None).await;
    let today = Utc::now().date_naive();
    let slot_id = seed_course_schedule_slot_with_venue(
        &db,
        course_id,
        dow_of(today),
        t(9, 0),
        t(10, 0),
        "Main Hall",
    )
    .await;

    calendar::materialize_range(&db, &[course_id], today, today)
        .await
        .expect("first materialize");
    assert_eq!(
        session_venues(&db, course_id).await,
        vec![Some("Main Hall".to_string())]
    );

    // A later slot venue edit must not rewrite an already-materialized
    // session: re-materializing hits `ON CONFLICT DO NOTHING`, so the row
    // keeps the venue it was snapshotted with.
    sqlx::query("UPDATE course_schedule_slots SET venue = 'Side Room' WHERE id = $1")
        .bind(slot_id)
        .execute(&db)
        .await
        .unwrap();
    calendar::materialize_range(&db, &[course_id], today, today)
        .await
        .expect("second materialize");
    assert_eq!(
        session_venues(&db, course_id).await,
        vec![Some("Main Hall".to_string())]
    );
}

#[sqlx::test]
async fn materialize_then_reconcile_is_noop(db: PgPool) {
    // Cross-anchor for `SLOT_OF_SESSION`: every row `materialize_range`
    // creates must be one the reconcile step already considers aligned —
    // re-applying the same weekly schedule right after materializing must
    // UPDATE 0 rows (no `xmin` changes) and DELETE 0 rows.
    let course_id = seed_course(&db, "Calendar Anchor Course", None).await;
    let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 16).unwrap(); // Wednesday
    let slots: Vec<calendar::SlotRow> = vec![
        (1, t(9, 0), t(10, 0), Some("A".to_string())),
        (1, t(14, 0), t(15, 30), Some("C".to_string())),
        (3, t(18, 0), t(19, 30), None),
        (5, t(7, 0), t(8, 0), Some("B".to_string())),
    ];
    let mut tx = db.begin().await.unwrap();
    calendar::set_initial_schedule_tx(&mut tx, course_id, &slots)
        .await
        .expect("set slots");
    tx.commit().await.unwrap();

    let from = today + chrono::Duration::days(1);
    let to = today + chrono::Duration::days(14);
    calendar::materialize_range(&db, &[course_id], from, to)
        .await
        .expect("materialize");
    let before = session_snapshot(&db, course_id).await;
    // Mon 9/21, 9/28 ×2 slots; Wed 9/23, 9/30; Fri 9/18, 9/25.
    assert_eq!(
        before.len(),
        8,
        "materialize should create every future slot date: {before:?}"
    );

    let mut tx = db.begin().await.unwrap();
    calendar::replace_weekly_schedule_tx(&mut tx, course_id, &slots, today)
        .await
        .expect("replace + reconcile");
    tx.commit().await.unwrap();

    assert_eq!(
        session_snapshot(&db, course_id).await,
        before,
        "reconcile right after materialize must neither update nor delete any session"
    );
}

type SessionSnapshotRow = (
    String,
    Uuid,
    chrono::NaiveDate,
    NaiveTime,
    NaiveTime,
    Option<String>,
);

/// Every session of `course_id` incl. its row version (`xmin`), so an
/// UPDATE that rewrites identical values still shows up as a change.
async fn session_snapshot(db: &PgPool, course_id: Uuid) -> Vec<SessionSnapshotRow> {
    sqlx::query_as(
        "SELECT xmin::text, id, session_date, start_time, end_time, venue \
         FROM course_sessions WHERE course_id = $1 ORDER BY session_date, start_time",
    )
    .bind(course_id)
    .fetch_all(db)
    .await
    .expect("fetch session snapshot")
}

async fn session_venues(db: &PgPool, course_id: Uuid) -> Vec<Option<String>> {
    sqlx::query_scalar(
        "SELECT venue FROM course_sessions WHERE course_id = $1 ORDER BY session_date",
    )
    .bind(course_id)
    .fetch_all(db)
    .await
    .expect("fetch session venues")
}

#[sqlx::test]
async fn list_course_sessions_materializes_todays_slot(db: PgPool) {
    // StudioNow pinned to UTC (common::studio_now_utc), so the service's
    // studio-local "today" equals the UTC date used for seeding.
    let course_id = seed_course(&db, "Weekly Course", None).await;
    let today = Utc::now().date_naive();
    seed_course_schedule_slot(&db, course_id, dow_of(today), t(9, 0), t(10, 0)).await;

    let sessions = service::list_course_sessions(
        &db,
        common::studio_now_utc(Utc::now()),
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
        common::studio_now_utc(Utc::now()),
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
        common::studio_now_utc(Utc::now()),
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
        common::studio_now_utc(Utc::now()),
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
        common::studio_now_utc(Utc::now()),
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
    let today = Utc::now().date_naive();
    let dow = dow_of(today);
    let own_scene =
        seed_session_scene(&db, "Own Course Today", Some(coach_id), today, t(9, 0), None).await;
    let own_course = own_scene.course;
    // other_course intentionally has no session pre-seeded — the coach-scoped
    // today_sessions() materialize/read must never surface a course this
    // coach doesn't own, so its session is left to that (never-triggered)
    // scope exclusion rather than composited into existence.
    let other_course = seed_course(&db, "Other Course Today", None).await;
    seed_course_schedule_slot(&db, other_course, dow, t(9, 0), t(10, 0)).await;

    // Two active enrolments + one cancelled on own_course -> enrolled_count
    // must be 2, not 3.
    let m1 = common::seed_member(&db, "m1-today@example.com", "hunter22-secret").await;
    let m2 = common::seed_member(&db, "m2-today@example.com", "hunter22-secret").await;
    let m3 = common::seed_member(&db, "m3-today@example.com", "hunter22-secret").await;
    seed_enrolment(&db, m1, own_course, "active", Utc::now()).await;
    seed_enrolment(&db, m2, own_course, "active", Utc::now()).await;
    seed_enrolment(&db, m3, own_course, "cancelled", Utc::now()).await;

    let auth = common::coach_auth(coach_user);
    let sessions = service::today_sessions(&db, common::studio_now_utc(Utc::now()), &auth)
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
    let auth = common::coach_auth(user_id);
    let sessions = service::today_sessions(&db, common::studio_now_utc(Utc::now()), &auth)
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

    let today = Utc::now().date_naive();
    let with_scene =
        seed_session_scene(&db, "Course With Coach Today", Some(coach_id), today, t(9, 0), None)
            .await;
    let with_coach = with_scene.course;
    let without_scene =
        seed_session_scene(&db, "Course Without Coach Today", None, today, t(11, 0), None).await;
    let without_coach = without_scene.course;

    let admin_id = common::seed_member(&db, "coach-name-admin@example.com", "hunter22-secret").await;
    let auth = common::admin_auth(admin_id);
    let sessions = service::today_sessions(&db, common::studio_now_utc(Utc::now()), &auth)
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
    let today = Utc::now().date_naive();
    let scene = seed_session_scene(
        &db,
        "Venue Match Course Today",
        None,
        today,
        t(9, 0),
        Some("Main Hall"),
    )
    .await;
    let course_id = scene.course;

    let admin_id = common::seed_member(&db, "venue-match-admin@example.com", "hunter22-secret").await;
    let auth = common::admin_auth(admin_id);
    let sessions = service::today_sessions(&db, common::studio_now_utc(Utc::now()), &auth)
        .await
        .expect("today sessions");

    let row = sessions.iter().find(|s| s.course_id == course_id).expect("session present");
    assert_eq!(row.venue.as_deref(), Some("Main Hall"));
}

#[sqlx::test]
async fn today_session_keeps_venue_after_slot_start_time_edit(db: PgPool) {
    // The session was materialized (venue snapshotted) before its slot's
    // start_time moved — it no longer matches any slot, but it still took
    // place in the venue it was snapshotted with.
    let at = common::studio_now_utc(Utc::now());
    let scene = seed_session_scene(
        &db,
        "Venue Kept Course Today",
        None,
        at.today(),
        t(9, 0),
        Some("Main Hall"),
    )
    .await;
    sqlx::query(
        "UPDATE course_schedule_slots SET start_time = '10:00', end_time = '11:00' WHERE id = $1",
    )
    .bind(scene.slot)
    .execute(&db)
    .await
    .unwrap();

    let admin_id =
        common::seed_member(&db, "venue-kept-admin@example.com", "hunter22-secret").await;
    let auth = common::admin_auth(admin_id);
    let sessions = service::today_sessions(&db, at, &auth)
        .await
        .expect("today sessions");

    let row = sessions
        .iter()
        .find(|s| s.id == scene.session)
        .expect("session present");
    assert_eq!(row.venue.as_deref(), Some("Main Hall"));
}

#[sqlx::test]
async fn today_sessions_venue_is_null_when_no_matching_slot(db: PgPool) {
    // A session with no corresponding `course_schedule_slots` row and a NULL
    // venue snapshot (e.g. a pre-snapshot row the backfill couldn't match to
    // any slot) reads as null. A slot edited away *after* materialization is
    // a different case — the snapshot survives, see
    // `today_session_keeps_venue_after_slot_start_time_edit`.
    let course_id = seed_course(&db, "Venue No Match Course Today", None).await;
    let today = Utc::now().date_naive();
    seed_course_session(&db, course_id, today, t(9, 0), t(10, 0)).await;

    let admin_id = common::seed_member(&db, "venue-no-match-admin@example.com", "hunter22-secret").await;
    let auth = common::admin_auth(admin_id);
    let sessions = service::today_sessions(&db, common::studio_now_utc(Utc::now()), &auth)
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

    let today = Utc::now().date_naive();
    let scene_a =
        seed_session_scene(&db, "Course A Admin Today", Some(coach_id), today, t(9, 0), None).await;
    let course_a = scene_a.course;
    let scene_b = seed_session_scene(&db, "Course B Admin Today", None, today, t(9, 0), None).await;
    let course_b = scene_b.course;

    let admin_id = common::seed_member(&db, "admin-today@example.com", "hunter22-secret").await;
    let auth = common::admin_auth(admin_id);
    let sessions = service::today_sessions(&db, common::studio_now_utc(Utc::now()), &auth)
        .await
        .expect("admin today sessions");

    let ids: Vec<Uuid> = sessions.iter().map(|s| s.course_id).collect();
    assert!(ids.contains(&course_a), "admin must see course_a, got {ids:?}");
    assert!(ids.contains(&course_b), "admin must see course_b, got {ids:?}");
}

#[sqlx::test]
async fn today_sessions_materializes_todays_slot_without_preexisting_session(db: PgPool) {
    // Every other today_sessions test pre-seeds its session row (via
    // seed_session_scene), so they stay green even if the materialize step
    // inside `materialize_day` silently stopped running. This slot-only
    // arrange is the one guard on that wire: the session row must come into
    // existence through today_sessions() itself.
    let course_id = seed_course(&db, "Slot Only Today", None).await;
    let today = Utc::now().date_naive();
    seed_course_schedule_slot(&db, course_id, dow_of(today), t(9, 0), t(10, 0)).await;

    let admin_id =
        common::seed_member(&db, "materialize-today-admin@example.com", "hunter22-secret").await;
    let auth = common::admin_auth(admin_id);
    let sessions = service::today_sessions(&db, common::studio_now_utc(Utc::now()), &auth)
        .await
        .expect("today sessions");

    let ids: Vec<Uuid> = sessions.iter().map(|s| s.course_id).collect();
    assert!(
        ids.contains(&course_id),
        "today's weekly slot should have been materialized by today_sessions() itself, got {ids:?}"
    );
}
