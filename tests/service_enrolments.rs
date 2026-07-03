//! Integration tests for `enrolments::service`.
//!
//! Covers:
//! - `enrol_from_purchase_tx`: happy path, capacity-full conflict, and the
//!   duplicate-active pre-check conflict.
//! - cancelling an enrolment frees the seat for a second enrol.
//! - concurrent enrol attempts for the same user+course: exactly one wins
//!   (the `FOR UPDATE` course lock serializes the two transactions).
//! - the partial unique index `uniq_enrolments_active` itself: a direct
//!   duplicate insert (bypassing the service's lock + pre-check) raises a
//!   unique violation, and the service maps a genuine DB-level violation
//!   to Conflict("already enrolled") even when its pre-check is blind.

mod common;

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use common::fixtures::{seed_course, seed_course_with_capacity, seed_enrolment};
use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::enrolments::repository as enrolments_repo;
use dream_fly_backend::modules::enrolments::service;
use dream_fly_backend::modules::orders::repository as orders_repo;

/// `enrolments.order_id` is a real FK into `orders`, so tests need an actual
/// order row committed in the same transaction rather than a bare random
/// UUID (mirrors `tests/service_subscriptions.rs`).
async fn seed_order(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    total_cents: i64,
) -> Uuid {
    orders_repo::create_order(
        tx,
        user_id,
        &format!("TEST-{}", Uuid::now_v7()),
        total_cents,
        0,
        None,
        0,
        0,
    )
    .await
    .expect("seed order")
    .id
}

#[sqlx::test]
async fn enrol_from_purchase_creates_active_enrolment(db: PgPool) {
    let user_id = common::seed_member(&db, "enrol-a@example.com", "Password!234").await;
    let course_id = seed_course(&db, "Enrol Course A", None).await;

    let mut tx = db.begin().await.expect("begin tx");
    let order_id = seed_order(&mut tx, user_id, 50_000).await;
    let enrolment = service::enrol_from_purchase_tx(&mut tx, user_id, course_id, order_id)
        .await
        .expect("enrol");
    tx.commit().await.expect("commit");

    assert_eq!(enrolment.user_id, user_id);
    assert_eq!(enrolment.course_id, course_id);
    assert_eq!(enrolment.order_id, Some(order_id));
    assert_eq!(enrolment.status.as_str(), "active");
}

#[sqlx::test]
async fn enrol_full_course_returns_course_is_full_conflict(db: PgPool) {
    let course_id = seed_course_with_capacity(&db, "Full Course", None, 1).await;
    let user_a = common::seed_member(&db, "enrol-full-a@example.com", "Password!234").await;
    let user_b = common::seed_member(&db, "enrol-full-b@example.com", "Password!234").await;

    let mut tx = db.begin().await.expect("begin tx");
    let order_a = seed_order(&mut tx, user_a, 50_000).await;
    service::enrol_from_purchase_tx(&mut tx, user_a, course_id, order_a)
        .await
        .expect("first enrol fills the only seat");
    tx.commit().await.expect("commit");

    let mut tx2 = db.begin().await.expect("begin tx2");
    let order_b = seed_order(&mut tx2, user_b, 50_000).await;
    let err = service::enrol_from_purchase_tx(&mut tx2, user_b, course_id, order_b)
        .await
        .expect_err("second enrol must be rejected: course is full");
    tx2.rollback().await.expect("rollback");

    match err {
        AppError::Conflict(msg) => assert_eq!(msg, "course is full"),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[sqlx::test]
async fn enrol_duplicate_active_returns_already_enrolled_conflict(db: PgPool) {
    let course_id = seed_course(&db, "Dup Course", None).await;
    let user_id = common::seed_member(&db, "enrol-dup@example.com", "Password!234").await;

    let mut tx = db.begin().await.expect("begin tx");
    let order_id = seed_order(&mut tx, user_id, 50_000).await;
    service::enrol_from_purchase_tx(&mut tx, user_id, course_id, order_id)
        .await
        .expect("first enrol");
    tx.commit().await.expect("commit");

    let mut tx2 = db.begin().await.expect("begin tx2");
    let order_id_2 = seed_order(&mut tx2, user_id, 50_000).await;
    let err = service::enrol_from_purchase_tx(&mut tx2, user_id, course_id, order_id_2)
        .await
        .expect_err("second enrol for the same user+course must be rejected");
    tx2.rollback().await.expect("rollback");

    match err {
        AppError::Conflict(msg) => assert_eq!(msg, "already enrolled"),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[sqlx::test]
async fn cancel_then_reenrol_succeeds(db: PgPool) {
    let course_id = seed_course(&db, "Reenrol Course", None).await;
    let user_id = common::seed_member(&db, "enrol-re@example.com", "Password!234").await;

    let mut tx = db.begin().await.expect("begin tx");
    let order_id = seed_order(&mut tx, user_id, 50_000).await;
    let first = service::enrol_from_purchase_tx(&mut tx, user_id, course_id, order_id)
        .await
        .expect("first enrol");
    tx.commit().await.expect("commit");

    // Cancel directly via the repository — the ownership/403 rules live in
    // the service's `cancel_enrolment`, which is covered by the HTTP tests.
    let mut cancel_tx = db.begin().await.expect("begin cancel tx");
    enrolments_repo::cancel_if_active_tx(&mut cancel_tx, first.id)
        .await
        .expect("cancel query")
        .expect("cancel succeeds for an active enrolment");
    cancel_tx.commit().await.expect("commit cancel");

    let mut tx2 = db.begin().await.expect("begin tx2");
    let order_id_2 = seed_order(&mut tx2, user_id, 50_000).await;
    let second = service::enrol_from_purchase_tx(&mut tx2, user_id, course_id, order_id_2)
        .await
        .expect("re-enrol after cancel should succeed");
    tx2.commit().await.expect("commit tx2");

    assert_ne!(first.id, second.id);
    assert_eq!(second.status.as_str(), "active");
}

#[sqlx::test]
async fn duplicate_active_insert_trips_partial_unique_index(db: PgPool) {
    // Bypass the service's course lock and pre-check entirely: two direct
    // repository inserts for the same user+course. The second must be
    // rejected by the partial unique index `uniq_enrolments_active` itself,
    // and the error must be recognizable via `is_unique_violation()` — the
    // exact condition `enrol_from_purchase_tx`'s fallback arm matches on.
    // If the index were missing, misnamed, or no longer partial-on-active,
    // this test is the one that catches it.
    let course_id = seed_course(&db, "Constraint Course", None).await;
    let user_id = common::seed_member(&db, "enrol-uniq@example.com", "Password!234").await;

    let mut tx = db.begin().await.expect("begin tx");
    let order_a = seed_order(&mut tx, user_id, 50_000).await;
    enrolments_repo::insert_tx(&mut tx, user_id, course_id, order_a)
        .await
        .expect("first insert");
    tx.commit().await.expect("commit");

    let mut tx2 = db.begin().await.expect("begin tx2");
    let order_b = seed_order(&mut tx2, user_id, 50_000).await;
    let err = enrolments_repo::insert_tx(&mut tx2, user_id, course_id, order_b)
        .await
        .expect_err("second active insert for the same user+course must violate uniq_enrolments_active");
    tx2.rollback().await.expect("rollback");

    assert!(
        matches!(err, sqlx::Error::Database(ref e) if e.is_unique_violation()),
        "expected a unique violation from the partial index, got {err:?}"
    );
}

#[sqlx::test]
async fn enrol_maps_db_unique_violation_to_already_enrolled(db: PgPool) {
    // Forces the INSERT itself to trip `uniq_enrolments_active` *through
    // the service*, proving the `is_unique_violation()` fallback arm maps
    // the DB error to Conflict("already enrolled"). Under READ COMMITTED
    // the pre-check would see any committed duplicate first, so we blind
    // it: pin a REPEATABLE READ snapshot before the conflicting row is
    // committed. The in-tx capacity count and exists pre-check then read
    // the old (empty) snapshot, while the INSERT still collides with the
    // committed index entry — deterministically exercising the second
    // line of defense.
    let course_id = seed_course(&db, "Unique Map Course", None).await;
    let user_id = common::seed_member(&db, "enrol-map@example.com", "Password!234").await;

    let mut tx = db.begin().await.expect("begin tx");
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await
        .expect("set isolation level");
    // First query in the tx pins the snapshot (before the duplicate exists).
    let order_id = seed_order(&mut tx, user_id, 50_000).await;

    // Commit the conflicting active enrolment from outside the transaction.
    seed_enrolment(&db, user_id, course_id, "active", Utc::now()).await;

    let err = service::enrol_from_purchase_tx(&mut tx, user_id, course_id, order_id)
        .await
        .expect_err("the insert must trip the partial unique index");
    tx.rollback().await.expect("rollback");

    match err {
        AppError::Conflict(msg) => assert_eq!(msg, "already enrolled"),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[sqlx::test]
async fn concurrent_enrol_same_user_course_only_one_succeeds(db: PgPool) {
    // Two concurrent enrol_from_purchase_tx calls for the same user+course.
    // Exactly one must succeed and exactly one active row must exist after.
    // Note: this validates the `FOR UPDATE` course-lock serialization — the
    // loser blocks on the lock until the winner commits, so it is the
    // pre-check that rejects it here. The unique-index second line of
    // defense is exercised directly by
    // `duplicate_active_insert_trips_partial_unique_index` and
    // `enrol_maps_db_unique_violation_to_already_enrolled` above.
    let course_id = seed_course(&db, "Race Course", None).await;
    let user_id = common::seed_member(&db, "enrol-race@example.com", "Password!234").await;

    async fn attempt(db: PgPool, user_id: Uuid, course_id: Uuid) -> bool {
        let mut tx = db.begin().await.expect("begin tx");
        let order_id = orders_repo::create_order(
            &mut tx,
            user_id,
            &format!("TEST-{}", Uuid::now_v7()),
            50_000,
            0,
            None,
            0,
            0,
        )
        .await
        .expect("seed order")
        .id;
        match service::enrol_from_purchase_tx(&mut tx, user_id, course_id, order_id).await {
            Ok(_) => {
                tx.commit().await.expect("commit");
                true
            }
            Err(_) => {
                tx.rollback().await.expect("rollback");
                false
            }
        }
    }

    let (res_a, res_b) = tokio::join!(
        tokio::spawn(attempt(db.clone(), user_id, course_id)),
        tokio::spawn(attempt(db.clone(), user_id, course_id)),
    );
    let ok_count = [res_a.expect("task a panicked"), res_b.expect("task b panicked")]
        .iter()
        .filter(|ok| **ok)
        .count();
    assert_eq!(ok_count, 1, "exactly one concurrent enrol should succeed");

    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM enrolments WHERE user_id = $1 AND course_id = $2 AND status = 'active'",
    )
    .bind(user_id)
    .bind(course_id)
    .fetch_one(&db)
    .await
    .expect("count active enrolments");
    assert_eq!(active_count, 1);
}
