//! Integration tests for `points::service`.
//!
//! Covers `apply_delta_tx`: earn (positive delta, no order), redeem
//! (negative delta, with an order id), insufficient balance (the
//! `users_points_balance_check` CHECK constraint rejects the UPDATE and the
//! database error is mapped to `AppError::Conflict`, with nothing persisted
//! since the failure happened inside an uncommitted transaction), a zero
//! delta (rejected before touching the DB), and a nonexistent user (404).
//! Also a DB-layer test of the CHECK constraint itself via the repository
//! function directly — the exact condition `apply_delta_tx`'s
//! `is_check_violation()` arm matches on — and the `/points/me` pagination
//! clamp (mirrors `service_coupons.rs` / `service_users.rs`).

mod common;

use sqlx::PgPool;
use uuid::Uuid;

use common::fixtures::set_points_balance;
use dream_fly_backend::error::AppError;
use dream_fly_backend::extractors::pagination::PaginationParams;
use dream_fly_backend::modules::points::model::PointReason;
use dream_fly_backend::modules::points::repository as points_repo;
use dream_fly_backend::modules::points::service;

/// Insert a minimal order row directly, purely so `point_ledger.order_id`
/// has a valid FK target. Local to this test file (not a shared fixture,
/// mirrors `http_orders.rs`'s file-local `seed_product_via_admin`) since
/// points tests only need the barest possible order shell.
async fn seed_order(db: &PgPool, user_id: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO orders (id, user_id, order_number, status, total_cents, discount_cents, created_at, updated_at)
        VALUES ($1, $2, $3, 'pending'::order_status, 1000, 0, NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(format!("TEST-PTS-{id}"))
    .execute(db)
    .await
    .expect("insert order");
    id
}

#[sqlx::test]
async fn apply_delta_earn_increases_balance_and_writes_ledger_row(db: PgPool) {
    let user_id = common::seed_member(&db, "pts-earn@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 10).await;

    let mut tx = db.begin().await.expect("begin tx");
    let balance_after =
        service::apply_delta_tx(&mut tx, user_id, 50, PointReason::CheckoutEarn, None)
            .await
            .expect("earn should succeed");
    tx.commit().await.expect("commit");

    assert_eq!(balance_after, 60);

    let balance = points_repo::find_balance(&db, user_id)
        .await
        .expect("query balance")
        .expect("user exists");
    assert_eq!(balance, 60, "users.points_balance must match balance_after");

    let ledger = points_repo::find_ledger_by_user(&db, user_id, 10, 0)
        .await
        .expect("query ledger");
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].delta, 50);
    assert_eq!(ledger[0].balance_after, 60);
    assert_eq!(ledger[0].reason, PointReason::CheckoutEarn);
    assert_eq!(ledger[0].order_id, None);
}

#[sqlx::test]
async fn apply_delta_redeem_decreases_balance_and_writes_ledger_row_with_order_id(db: PgPool) {
    let user_id = common::seed_member(&db, "pts-redeem@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 100).await;
    let order_id = seed_order(&db, user_id).await;

    let mut tx = db.begin().await.expect("begin tx");
    let balance_after = service::apply_delta_tx(
        &mut tx,
        user_id,
        -30,
        PointReason::CheckoutRedeem,
        Some(order_id),
    )
    .await
    .expect("redeem should succeed");
    tx.commit().await.expect("commit");

    assert_eq!(balance_after, 70);

    let ledger = points_repo::find_ledger_by_user(&db, user_id, 10, 0)
        .await
        .expect("query ledger");
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].delta, -30);
    assert_eq!(ledger[0].balance_after, 70);
    assert_eq!(ledger[0].reason, PointReason::CheckoutRedeem);
    assert_eq!(ledger[0].order_id, Some(order_id));
}

#[sqlx::test]
async fn apply_delta_insufficient_balance_returns_conflict_and_does_not_persist(db: PgPool) {
    let user_id = common::seed_member(&db, "pts-insufficient@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 100).await;

    let mut tx = db.begin().await.expect("begin tx");
    let err = service::apply_delta_tx(&mut tx, user_id, -200, PointReason::CheckoutRedeem, None)
        .await
        .expect_err("insufficient balance must be rejected");
    // Roll back explicitly (rather than relying on Drop) so the connection
    // is cleanly back in the pool before the independent verification
    // queries below run over the same pool.
    tx.rollback().await.expect("rollback");

    match err {
        AppError::Conflict(msg) => assert_eq!(msg, "insufficient points"),
        other => panic!("expected Conflict, got {other:?}"),
    }

    // Independent queries via the pool (outside the failed tx) prove
    // nothing landed: neither the balance change nor a ledger row.
    let balance = points_repo::find_balance(&db, user_id)
        .await
        .expect("query balance")
        .expect("user exists");
    assert_eq!(balance, 100, "balance must be unchanged after rollback");

    let ledger = points_repo::find_ledger_by_user(&db, user_id, 10, 0)
        .await
        .expect("query ledger");
    assert!(ledger.is_empty(), "no ledger row should have been written");
}

#[sqlx::test]
async fn apply_delta_zero_returns_validation_error(db: PgPool) {
    let user_id = common::seed_member(&db, "pts-zero@example.com", "Password!234").await;

    let mut tx = db.begin().await.expect("begin tx");
    let err = service::apply_delta_tx(&mut tx, user_id, 0, PointReason::AdminAdjust, None)
        .await
        .expect_err("zero delta must be rejected");
    tx.rollback().await.expect("rollback");

    match err {
        AppError::Validation(msg) => assert_eq!(msg, "delta must be non-zero"),
        other => panic!("expected Validation, got {other:?}"),
    }

    let ledger = points_repo::find_ledger_by_user(&db, user_id, 10, 0)
        .await
        .expect("query ledger");
    assert!(ledger.is_empty(), "a zero-delta call must not write a ledger row");
}

#[sqlx::test]
async fn apply_delta_nonexistent_user_returns_not_found(db: PgPool) {
    let mut tx = db.begin().await.expect("begin tx");
    let err = service::apply_delta_tx(&mut tx, Uuid::now_v7(), 10, PointReason::AdminAdjust, None)
        .await
        .expect_err("nonexistent user must 404");
    tx.rollback().await.expect("rollback");

    assert!(
        matches!(err, AppError::NotFound(ref m) if m == "user not found"),
        "got {err:?}"
    );
}

#[sqlx::test]
async fn adjust_balance_tx_check_violation_reports_expected_constraint_and_sqlstate(db: PgPool) {
    // Calls the repository function directly, bypassing `apply_delta_tx`
    // (and any other application-level logic) entirely, to prove the real
    // `users_points_balance_check` CHECK constraint fires with the exact
    // shape `apply_delta_tx`'s `is_check_violation()` mapping arm relies on.
    let user_id = common::seed_member(&db, "pts-checkviol@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 50).await;

    let mut tx = db.begin().await.expect("begin tx");
    let err = points_repo::adjust_balance_tx(&mut tx, user_id, -100)
        .await
        .expect_err("negative balance must trip the CHECK constraint");
    tx.rollback().await.expect("rollback");

    match err {
        sqlx::Error::Database(db_err) => {
            assert!(
                db_err.is_check_violation(),
                "expected a check violation, got {db_err:?}"
            );
            assert_eq!(db_err.code().as_deref(), Some("23514"));
            assert_eq!(db_err.constraint(), Some("users_points_balance_check"));
        }
        other => panic!("expected Database error, got {other:?}"),
    }
}

#[sqlx::test]
async fn apply_delta_unrelated_check_violation_is_not_mapped_to_insufficient_points(db: PgPool) {
    // The Conflict("insufficient points") mapping must be scoped to the
    // `users_points_balance_check` constraint specifically — `users`
    // carries other CHECK constraints (`users_has_auth_method` exists
    // today, and future ones like a balance cap could be added), and a
    // blanket is_check_violation() → Conflict mapping would misreport
    // those as "insufficient points". Simulate that future: add an
    // artificial cap constraint in this test's throwaway database and
    // violate it — the error must surface as a generic Database error,
    // not Conflict.
    let user_id = common::seed_member(&db, "pts-cap@example.com", "Password!234").await;
    sqlx::query(
        "ALTER TABLE users ADD CONSTRAINT test_points_balance_cap CHECK (points_balance <= 1000)",
    )
    .execute(&db)
    .await
    .expect("add artificial cap constraint");

    let mut tx = db.begin().await.expect("begin tx");
    let err = service::apply_delta_tx(&mut tx, user_id, 5000, PointReason::AdminAdjust, None)
        .await
        .expect_err("cap violation must be rejected");
    tx.rollback().await.expect("rollback");

    match err {
        AppError::Database(sqlx::Error::Database(db_err)) => {
            assert!(db_err.is_check_violation(), "got {db_err:?}");
            assert_eq!(db_err.constraint(), Some("test_points_balance_cap"));
        }
        other => panic!(
            "an unrelated check violation must pass through as Database, got {other:?}"
        ),
    }
}

/// Attempt a `try_spend_tx` spend inside its own transaction, committing on
/// success / rolling back on failure — used by the concurrent race test
/// below. Mirrors `service_rewards.rs`'s `attempt_redeem` helper.
async fn attempt_spend(db: PgPool, user_id: Uuid, cost: i64) -> Result<i64, AppError> {
    let mut tx = db.begin().await.expect("begin tx");
    let result = service::try_spend_tx(&mut tx, user_id, cost, PointReason::Redeem, None).await;
    match &result {
        Ok(_) => tx.commit().await.expect("commit"),
        Err(_) => tx.rollback().await.expect("rollback"),
    }
    result
}

#[sqlx::test]
async fn try_spend_success_returns_balance_after_and_writes_ledger_row(db: PgPool) {
    let user_id = common::seed_member(&db, "pts-spend-ok@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 100).await;

    let mut tx = db.begin().await.expect("begin tx");
    let balance_after = service::try_spend_tx(&mut tx, user_id, 30, PointReason::Redeem, None)
        .await
        .expect("spend should succeed");
    tx.commit().await.expect("commit");

    assert_eq!(balance_after, 70);

    let balance = points_repo::find_balance(&db, user_id)
        .await
        .expect("query balance")
        .expect("user exists");
    assert_eq!(balance, 70, "users.points_balance must match balance_after");

    let ledger = points_repo::find_ledger_by_user(&db, user_id, 10, 0)
        .await
        .expect("query ledger");
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].delta, -30);
    assert_eq!(ledger[0].balance_after, 70);
    assert_eq!(ledger[0].reason, PointReason::Redeem);
    assert_eq!(ledger[0].order_id, None);
}

#[sqlx::test]
async fn try_spend_insufficient_balance_returns_conflict_and_does_not_persist(db: PgPool) {
    let user_id = common::seed_member(&db, "pts-spend-poor@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 10).await;

    let mut tx = db.begin().await.expect("begin tx");
    let err = service::try_spend_tx(&mut tx, user_id, 50, PointReason::Redeem, None)
        .await
        .expect_err("insufficient balance must be rejected");
    tx.rollback().await.expect("rollback");

    match err {
        AppError::Conflict(msg) => assert_eq!(msg, "點數不足"),
        other => panic!("expected Conflict(\"點數不足\"), got {other:?}"),
    }

    let balance = points_repo::find_balance(&db, user_id)
        .await
        .expect("query balance")
        .expect("user exists");
    assert_eq!(balance, 10, "balance must be unchanged");

    let ledger = points_repo::find_ledger_by_user(&db, user_id, 10, 0)
        .await
        .expect("query ledger");
    assert!(ledger.is_empty(), "no ledger row should have been written");
}

#[sqlx::test]
async fn concurrent_try_spend_same_user_exactly_one_succeeds(db: PgPool) {
    // Two concurrent try_spend_tx calls against the same user, with a
    // balance that covers exactly one of the two spends. The `FOR UPDATE`
    // lock in `lock_balance_tx` must serialize the two attempts so only the
    // first commits its spend — the loser re-reads the now-updated (lower)
    // balance and gets Conflict("點數不足"). Mirrors
    // `service_orders.rs::concurrent_checkout_last_unit_only_succeeds_once`
    // / `service_rewards.rs::concurrent_redeem_last_stock_unit_only_one_succeeds`.
    let user_id = common::seed_member(&db, "pts-spend-race@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 50).await;

    let (res_a, res_b) = tokio::join!(
        tokio::spawn(attempt_spend(db.clone(), user_id, 50)),
        tokio::spawn(attempt_spend(db.clone(), user_id, 50)),
    );
    let res_a = res_a.expect("task a panicked");
    let res_b = res_b.expect("task b panicked");

    let ok_count = [&res_a, &res_b].iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok_count, 1, "exactly one concurrent spend should succeed");

    let conflict_count = [&res_a, &res_b]
        .iter()
        .filter(|r| {
            matches!(r, Err(AppError::Conflict(msg)) if msg == "點數不足")
        })
        .count();
    assert_eq!(conflict_count, 1, "the other must fail with Conflict(\"點數不足\")");

    let balance = points_repo::find_balance(&db, user_id)
        .await
        .expect("query balance")
        .expect("user exists");
    assert_eq!(balance, 0, "exactly one spend of 50 should have landed");

    let ledger = points_repo::find_ledger_by_user(&db, user_id, 10, 0)
        .await
        .expect("query ledger");
    assert_eq!(
        ledger.len(),
        1,
        "exactly one ledger row should have been written"
    );
}

#[sqlx::test]
async fn try_spend_nonpositive_cost_returns_validation_error(db: PgPool) {
    let user_id = common::seed_member(&db, "pts-spend-badcost@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 100).await;

    let mut tx = db.begin().await.expect("begin tx");
    let err = service::try_spend_tx(&mut tx, user_id, 0, PointReason::Redeem, None)
        .await
        .expect_err("zero cost must be rejected");
    tx.rollback().await.expect("rollback");
    assert!(matches!(err, AppError::Validation(_)), "got {err:?}");

    let mut tx = db.begin().await.expect("begin tx");
    let err = service::try_spend_tx(&mut tx, user_id, -10, PointReason::Redeem, None)
        .await
        .expect_err("negative cost must be rejected");
    tx.rollback().await.expect("rollback");
    assert!(matches!(err, AppError::Validation(_)), "got {err:?}");
}

#[sqlx::test]
async fn get_my_points_clamps_per_page_to_100(db: PgPool) {
    let user_id = common::seed_member(&db, "pts-clamp@example.com", "Password!234").await;

    let resp = service::get_my_points(
        &db,
        user_id,
        &PaginationParams {
            page: 1,
            per_page: 500,
        },
    )
    .await
    .expect("get_my_points");

    assert_eq!(resp.meta.per_page, 100, "per_page should clamp to 100");
}
