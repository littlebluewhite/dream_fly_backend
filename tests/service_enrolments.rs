//! Integration tests for `enrolments::service`.
//!
//! Covers:
//! - `enrol_batch_from_purchase_tx` (behind `lock_courses_tx`, via the
//!   one-course `enrol` helper below): happy path, capacity-full conflict,
//!   and the duplicate-active pre-check conflict.
//! - a course outside the `CourseLocks` witness is Internal.
//! - cancelling an enrolment frees the seat for a second enrol.
//! - concurrent enrol attempts for the same user+course: exactly one wins
//!   (the `FOR UPDATE` course lock serializes the two transactions).
//! - the partial unique index `uniq_enrolments_active` itself: a direct
//!   duplicate insert (bypassing the service's lock + pre-check) raises a
//!   unique violation, and the service maps a genuine DB-level violation
//!   to Conflict("already enrolled") even when its pre-check is blind.
//! - B5: `cancel_enrolment` racing an in-flight leave approval (假單列已被
//!   核准 tx 持有、其出勤 INSERT 還沒跑) must queue, not deadlock.

mod common;

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use common::fixtures::{
    CourseSeed, seed_course, seed_course_session, seed_enrolment, seed_leave_request,
};
use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::courses::seats;
use dream_fly_backend::modules::enrolments::model::{Enrolment, EnrolmentStatus};
use dream_fly_backend::modules::enrolments::repository as enrolments_repo;
use dream_fly_backend::modules::enrolments::service;
use dream_fly_backend::modules::leave::model::LeaveStatus;
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
        orders_repo::OrderAmounts {
            total_cents,
            discount_cents: 0,
            points_used: 0,
            points_earned: 0,
        },
        None,
        "credit_card",
        chrono::Utc::now(),
    )
    .await
    .expect("seed order")
    .id
}

/// One course through the purchase path: `lock_courses_tx` then
/// `enrol_batch_from_purchase_tx`, the same pair `orders::locks` +
/// `checkout` run.
async fn enrol(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    course_id: Uuid,
    order_id: Uuid,
) -> Result<Enrolment, AppError> {
    let locks = seats::lock_courses_tx(tx, &[course_id]).await?;
    let mut enrolments = service::enrol_batch_from_purchase_tx(
        tx,
        &locks,
        user_id,
        &[course_id],
        order_id,
        chrono::Utc::now(),
    )
    .await?;
    Ok(enrolments.remove(0))
}

#[sqlx::test]
async fn enrol_from_purchase_creates_active_enrolment(db: PgPool) {
    let user_id = common::seed_member(&db, "enrol-a@example.com", "Password!234").await;
    let course_id = seed_course(&db, "Enrol Course A", None).await;

    let mut tx = db.begin().await.expect("begin tx");
    let order_id = seed_order(&mut tx, user_id, 50_000).await;
    let enrolment = enrol(&mut tx, user_id, course_id, order_id)
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
    let course_id = CourseSeed::new("Full Course").max_students(1).insert(&db).await;
    let user_a = common::seed_member(&db, "enrol-full-a@example.com", "Password!234").await;
    let user_b = common::seed_member(&db, "enrol-full-b@example.com", "Password!234").await;

    let mut tx = db.begin().await.expect("begin tx");
    let order_a = seed_order(&mut tx, user_a, 50_000).await;
    enrol(&mut tx, user_a, course_id, order_a)
        .await
        .expect("first enrol fills the only seat");
    tx.commit().await.expect("commit");

    let mut tx2 = db.begin().await.expect("begin tx2");
    let order_b = seed_order(&mut tx2, user_b, 50_000).await;
    let err = enrol(&mut tx2, user_b, course_id, order_b)
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
    enrol(&mut tx, user_id, course_id, order_id)
        .await
        .expect("first enrol");
    tx.commit().await.expect("commit");

    let mut tx2 = db.begin().await.expect("begin tx2");
    let order_id_2 = seed_order(&mut tx2, user_id, 50_000).await;
    let err = enrol(&mut tx2, user_id, course_id, order_id_2)
        .await
        .expect_err("second enrol for the same user+course must be rejected");
    tx2.rollback().await.expect("rollback");

    match err {
        AppError::Conflict(msg) => assert_eq!(msg, "already enrolled"),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[sqlx::test]
async fn enrol_course_outside_the_witness_is_internal(db: PgPool) {
    let locked = seed_course(&db, "Locked Course", None).await;
    let unlocked = seed_course(&db, "Unlocked Course", None).await;
    let user_id = common::seed_member(&db, "enrol-witness@example.com", "Password!234").await;

    let mut tx = db.begin().await.expect("begin tx");
    let order_id = seed_order(&mut tx, user_id, 50_000).await;
    let locks = seats::lock_courses_tx(&mut tx, &[locked])
        .await
        .expect("lock course");
    let err = service::enrol_batch_from_purchase_tx(
        &mut tx,
        &locks,
        user_id,
        &[locked, unlocked],
        order_id,
        chrono::Utc::now(),
    )
    .await
    .expect_err("a course the witness doesn't cover must be rejected");
    tx.rollback().await.expect("rollback");

    assert!(matches!(err, AppError::Internal(_)), "got {err:?}");
}

#[sqlx::test]
async fn cancel_then_reenrol_succeeds(db: PgPool) {
    let course_id = seed_course(&db, "Reenrol Course", None).await;
    let user_id = common::seed_member(&db, "enrol-re@example.com", "Password!234").await;

    let mut tx = db.begin().await.expect("begin tx");
    let order_id = seed_order(&mut tx, user_id, 50_000).await;
    let first = enrol(&mut tx, user_id, course_id, order_id)
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
    let second = enrol(&mut tx2, user_id, course_id, order_id_2)
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
    // exact condition the service's (`enrol_one_tx`) fallback arm matches on.
    // If the index were missing, misnamed, or no longer partial-on-active,
    // this test is the one that catches it.
    let course_id = seed_course(&db, "Constraint Course", None).await;
    let user_id = common::seed_member(&db, "enrol-uniq@example.com", "Password!234").await;

    let mut tx = db.begin().await.expect("begin tx");
    let order_a = seed_order(&mut tx, user_id, 50_000).await;
    enrolments_repo::insert_tx(&mut tx, user_id, course_id, order_a, chrono::Utc::now())
        .await
        .expect("first insert");
    tx.commit().await.expect("commit");

    let mut tx2 = db.begin().await.expect("begin tx2");
    let order_b = seed_order(&mut tx2, user_id, 50_000).await;
    let err = enrolments_repo::insert_tx(&mut tx2, user_id, course_id, order_b, chrono::Utc::now())
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
    seed_enrolment(&db, user_id, course_id, EnrolmentStatus::Active, Utc::now()).await;

    let err = enrol(&mut tx, user_id, course_id, order_id)
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
    // Two concurrent enrol attempts (lock + batch) for the same user+course.
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
            orders_repo::OrderAmounts {
                total_cents: 50_000,
                discount_cents: 0,
                points_used: 0,
                points_earned: 0,
            },
            None,
            "credit_card",
            chrono::Utc::now(),
        )
        .await
        .expect("seed order")
        .id;
        match enrol(&mut tx, user_id, course_id, order_id).await {
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

/// B5 防死鎖回歸:核准 tx(T1)已持有假單列、出勤 INSERT 還沒跑時,
/// `cancel_enrolment` 鎖住報名列後卡在「取消待審假單」上;T1 接著 INSERT
/// 出勤,其 FK 對報名列取 `FOR KEY SHARE`。報名列若是 `FOR UPDATE` 就成環
/// ("deadlock detected");`FOR NO KEY UPDATE` 與 `KEY SHARE` 相容,T1 先
/// commit,取消再看到假單已是 approved → 不動(ADR-0008 gap 1)。
///
/// `spawn_blocking` + `Handle::block_on` 讓取消跑在真的 OS thread 上(同
/// `service_attendance.rs` 的併發測試)。
#[sqlx::test]
async fn cancel_enrolment_vs_in_flight_approval_does_not_deadlock(db: PgPool) {
    let course_id = seed_course(&db, "Cancel vs Approval Course", None).await;
    let tomorrow = (Utc::now() + chrono::Duration::days(1)).date_naive();
    let nine = chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap();
    let ten = chrono::NaiveTime::from_hms_opt(10, 0, 0).unwrap();
    let session_id = seed_course_session(&db, course_id, tomorrow, nine, ten).await;
    let admin = common::seed_member(&db, "cancel-race-admin@example.com", "Password!234").await;
    let member = common::seed_member(&db, "cancel-race-member@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&db, member, course_id, EnrolmentStatus::Active, Utc::now()).await;
    let leave_id = seed_leave_request(&db, enrolment_id, session_id, LeaveStatus::Pending).await;

    let mut t1 = db.begin().await.expect("begin t1");
    sqlx::query(
        "UPDATE leave_requests \
         SET status = 'approved'::leave_status, decided_by = $2, decided_at = NOW(), \
             updated_at = NOW() \
         WHERE id = $1",
    )
    .bind(leave_id)
    .bind(admin)
    .execute(&mut *t1)
    .await
    .expect("t1 approve");
    let t1_pid = common::backend_pid(&mut t1).await;

    let db_cancel = db.clone();
    let member_auth = common::auth_for(&db, member).await;
    let handle = tokio::runtime::Handle::current();
    let cancel = tokio::task::spawn_blocking(move || {
        handle.block_on(service::cancel_enrolment(
            &db_cancel,
            &member_auth,
            enrolment_id,
        ))
    });

    common::wait_for_lock_waiter(&db, t1_pid).await;
    assert!(
        !cancel.is_finished(),
        "cancel must be blocked on t1's leave row"
    );

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        sqlx::query(
            "INSERT INTO attendance_records \
             (id, session_id, enrolment_id, status, marked_by, marked_at, created_at) \
             VALUES ($1, $2, $3, 'leave'::attendance_status, $4, NOW(), NOW())",
        )
        .bind(Uuid::now_v7())
        .bind(session_id)
        .bind(enrolment_id)
        .bind(admin)
        .execute(&mut *t1),
    )
    .await
    .expect("t1 attendance insert must not hang")
    .expect("t1 attendance insert");
    t1.commit().await.expect("commit t1");

    cancel
        .await
        .expect("join cancel")
        .expect("cancel must succeed after t1 commits");

    let enrolment_status: String =
        sqlx::query_scalar("SELECT status::text FROM enrolments WHERE id = $1")
            .bind(enrolment_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(enrolment_status, "cancelled");
    let leave_status: String =
        sqlx::query_scalar("SELECT status::text FROM leave_requests WHERE id = $1")
            .bind(leave_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(leave_status, "approved", "an approved leave is left as-is");
}
