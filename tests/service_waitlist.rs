//! Integration tests for `waitlist::service`.
//!
//! Covers:
//! - `join_waitlist`: happy path (full course), course-not-full conflict,
//!   duplicate-waiting conflict, and the course existence/active guards
//!   (mirroring cart's "course not found" / "course is not available"
//!   choices, per the endpoint contract).
//! - cancelling a waitlist entry frees the user to rejoin (partial index
//!   semantics — only 'waiting' rows collide).
//! - the partial unique index `uniq_waitlist_waiting` itself: a direct
//!   duplicate insert (bypassing the service's pre-check) raises a unique
//!   violation with the expected constraint name — the exact condition
//!   `join_waitlist`'s fallback arm matches on.

mod common;

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use common::fixtures::{seed_course_with_capacity, seed_enrolment};
use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::waitlist::repository as waitlist_repo;
use dream_fly_backend::modules::waitlist::service;

#[sqlx::test]
async fn join_full_course_creates_waiting_entry(db: PgPool) {
    // max_students = 1, one active enrolment fills the only seat.
    let course_id = seed_course_with_capacity(&db, "Full Join Course", None, 1).await;
    let filler = common::seed_member(&db, "wl-filler-a@example.com", "Password!234").await;
    seed_enrolment(&db, filler, course_id, "active", Utc::now()).await;

    let joiner = common::seed_member(&db, "wl-joiner-a@example.com", "Password!234").await;

    let entry = service::join_waitlist(&db, joiner, course_id)
        .await
        .expect("join succeeds on a full course");

    assert_eq!(entry.course_id, course_id);
    assert_eq!(entry.course_name, "Full Join Course");
    assert_eq!(entry.status, "waiting");
}

#[sqlx::test]
async fn join_course_not_full_returns_conflict(db: PgPool) {
    let course_id = seed_course_with_capacity(&db, "Not Full Course", None, 2).await;
    let joiner = common::seed_member(&db, "wl-notfull@example.com", "Password!234").await;

    let err = service::join_waitlist(&db, joiner, course_id)
        .await
        .expect_err("must reject: course is not full");

    match err {
        AppError::Conflict(msg) => assert_eq!(msg, "course is not full"),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[sqlx::test]
async fn join_duplicate_waiting_returns_conflict(db: PgPool) {
    let course_id = seed_course_with_capacity(&db, "Dup Waitlist Course", None, 1).await;
    let filler = common::seed_member(&db, "wl-filler-b@example.com", "Password!234").await;
    seed_enrolment(&db, filler, course_id, "active", Utc::now()).await;
    let joiner = common::seed_member(&db, "wl-dup@example.com", "Password!234").await;

    service::join_waitlist(&db, joiner, course_id)
        .await
        .expect("first join succeeds");

    let err = service::join_waitlist(&db, joiner, course_id)
        .await
        .expect_err("second join for the same user+course must be rejected");

    match err {
        AppError::Conflict(msg) => assert_eq!(msg, "already on waitlist"),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[sqlx::test]
async fn join_nonexistent_course_returns_404(db: PgPool) {
    let joiner = common::seed_member(&db, "wl-404@example.com", "Password!234").await;

    let err = service::join_waitlist(&db, joiner, Uuid::now_v7())
        .await
        .expect_err("nonexistent course must 404");

    assert!(matches!(err, AppError::NotFound(_)), "got {err:?}");
}

#[sqlx::test]
async fn join_inactive_course_returns_400(db: PgPool) {
    let course_id = seed_course_with_capacity(&db, "Inactive Waitlist Course", None, 1).await;
    sqlx::query("UPDATE courses SET is_active = false WHERE id = $1")
        .bind(course_id)
        .execute(&db)
        .await
        .expect("deactivate course");
    let joiner = common::seed_member(&db, "wl-inactive@example.com", "Password!234").await;

    let err = service::join_waitlist(&db, joiner, course_id)
        .await
        .expect_err("inactive course must be rejected");

    assert!(
        matches!(err, AppError::BadRequest(ref m) if m.contains("not available")),
        "got {err:?}"
    );
}

#[sqlx::test]
async fn cancel_then_rejoin_succeeds(db: PgPool) {
    let course_id = seed_course_with_capacity(&db, "Rejoin Waitlist Course", None, 1).await;
    let filler = common::seed_member(&db, "wl-filler-c@example.com", "Password!234").await;
    seed_enrolment(&db, filler, course_id, "active", Utc::now()).await;
    let joiner = common::seed_member(&db, "wl-rejoin@example.com", "Password!234").await;

    let first = service::join_waitlist(&db, joiner, course_id)
        .await
        .expect("first join succeeds");

    // Cancel directly via the repository — the ownership/403 rules live in
    // the service's `cancel_waitlist_entry`, which is covered by the HTTP
    // tests.
    let mut tx = db.begin().await.expect("begin tx");
    waitlist_repo::cancel_if_waiting_tx(&mut tx, first.id)
        .await
        .expect("cancel query")
        .expect("cancel succeeds for a waiting entry");
    tx.commit().await.expect("commit");

    let second = service::join_waitlist(&db, joiner, course_id)
        .await
        .expect("re-join after cancel should succeed — partial index only guards 'waiting' rows");

    assert_ne!(first.id, second.id);
    assert_eq!(second.status, "waiting");
}

#[sqlx::test]
async fn duplicate_waiting_insert_trips_partial_unique_index(db: PgPool) {
    // Bypass the service's pre-check entirely: two direct repository
    // inserts for the same user+course. The second must be rejected by the
    // partial unique index `uniq_waitlist_waiting` itself, and the error
    // must be recognizable via `is_unique_violation()` with the expected
    // constraint name — the exact condition `join_waitlist`'s fallback arm
    // matches on. If the index were missing, misnamed, or no longer
    // partial-on-waiting, this test is the one that catches it.
    let course_id = seed_course_with_capacity(&db, "Constraint Waitlist Course", None, 1).await;
    let user_id = common::seed_member(&db, "wl-uniq@example.com", "Password!234").await;

    waitlist_repo::insert(&db, user_id, course_id)
        .await
        .expect("first insert");

    let err = waitlist_repo::insert(&db, user_id, course_id)
        .await
        .expect_err(
            "second waiting insert for the same user+course must violate uniq_waitlist_waiting",
        );

    match err {
        sqlx::Error::Database(ref e) => {
            assert!(
                e.is_unique_violation(),
                "expected a unique violation, got {e:?}"
            );
            assert_eq!(
                e.constraint(),
                Some("uniq_waitlist_waiting"),
                "expected the partial unique index's constraint name"
            );
        }
        other => panic!("expected a database error, got {other:?}"),
    }
}
