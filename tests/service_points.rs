//! Integration tests for `points::service`.
//!
//! Covers `apply_delta_tx`, which now takes a `points::model::LedgerDelta`
//! built via one of its six reason-named constructors instead of a bare
//! `(delta, reason, order_id)` triple: earn and redeem each carry a real
//! `order_id` here (a `CheckoutEarn`/`CheckoutRedeem` row with no
//! `order_id` is no longer constructible under `LedgerDelta` — exactly the
//! freedom this refactor removes), insufficient balance (the
//! `users_points_balance_check` CHECK constraint rejects the UPDATE and the
//! database error is mapped to `AppError::Conflict`, with nothing persisted
//! since the failure happened inside an uncommitted transaction), a zero
//! delta (rejected before touching the DB — via `LedgerDelta::admin_adjust`,
//! whose zero magnitude the reason-named constructors' `debug_assert`
//! deliberately lets through), and a nonexistent user (404). Also a
//! DB-layer test of the CHECK constraint itself via the repository function
//! directly — the exact condition `apply_delta_tx`'s `is_check_violation()`
//! arm matches on — a seam-local anchor pairing `LedgerDelta`'s
//! `checkout_earn`/`checkout_redeem` ledger writes with
//! `find_order_flow_sums_tx`'s read-back, the `/points/me` pagination clamp
//! (mirrors `service_coupons.rs` / `service_users.rs`), `try_spend_tx`
//! (now `(tx, user_id, cost)` — the spent reason is hard-coded to
//! `PointReason::Redeem` inside the function rather than caller-supplied),
//! and `adjust_points` (admin CAS adjustment, internally
//! `LedgerDelta::admin_adjust`).

mod common;

use sqlx::PgPool;
use uuid::Uuid;

use common::fixtures::set_points_balance;
use dream_fly_backend::error::AppError;
use dream_fly_backend::extractors::pagination::PaginationParams;
use dream_fly_backend::modules::points::dto::AdjustPointsRequest;
use dream_fly_backend::modules::points::model::{LedgerDelta, PointReason};
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
    let order_id = seed_order(&db, user_id).await;

    let mut tx = db.begin().await.expect("begin tx");
    let balance_after =
        service::apply_delta_tx(&mut tx, user_id, LedgerDelta::checkout_earn(50, order_id))
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
    assert_eq!(ledger[0].order_id, Some(order_id));
}

#[sqlx::test]
async fn apply_delta_redeem_decreases_balance_and_writes_ledger_row_with_order_id(db: PgPool) {
    let user_id = common::seed_member(&db, "pts-redeem@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 100).await;
    let order_id = seed_order(&db, user_id).await;

    let mut tx = db.begin().await.expect("begin tx");
    let balance_after =
        service::apply_delta_tx(&mut tx, user_id, LedgerDelta::checkout_redeem(30, order_id))
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
    let order_id = seed_order(&db, user_id).await;

    let mut tx = db.begin().await.expect("begin tx");
    let err = service::apply_delta_tx(
        &mut tx,
        user_id,
        LedgerDelta::checkout_redeem(200, order_id),
    )
    .await
    .expect_err("insufficient balance must be rejected");
    // Roll back explicitly (rather than relying on Drop) so the connection
    // is cleanly back in the pool before the independent verification
    // queries below run over the same pool.
    tx.rollback().await.expect("rollback");

    match err {
        AppError::Conflict(msg) => assert_eq!(msg, "點數不足"),
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
    let err = service::apply_delta_tx(&mut tx, user_id, LedgerDelta::admin_adjust(0))
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
    let err = service::apply_delta_tx(&mut tx, Uuid::now_v7(), LedgerDelta::admin_adjust(10))
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
    let err = service::apply_delta_tx(&mut tx, user_id, LedgerDelta::admin_adjust(5000))
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

// ---------------------------------------------------------------------
// LedgerDelta: seam-local anchor for the checkout_earn/checkout_redeem
// write <-> find_order_flow_sums_tx read-back sign twin
// ---------------------------------------------------------------------

/// `LedgerDelta::checkout_redeem` writes a *negative* `delta`;
/// `find_order_flow_sums_tx`'s SQL negates the summed `checkout_redeem`
/// delta back on the way out (`points::repository`'s doc comment on that
/// function has the exact `COALESCE(-(SUM(...)))` shape). Nothing forces
/// those two signs to stay in step with each other — only e2e
/// checkout/refund tests would notice if they ever drifted apart. This
/// test pulls that twin from e2e distance to the seam itself: write both
/// directions for the same order via `apply_delta_tx`, then read them back
/// through the exact repository function `orders::refund::plan_refund`
/// consumes, and assert both magnitudes come back positive.
#[sqlx::test]
async fn find_order_flow_sums_tx_returns_positive_magnitudes_for_earn_and_redeem_writes(
    db: PgPool,
) {
    let user_id = common::seed_member(&db, "pts-flow-sums@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 100).await; // covers the 30-point redeem below
    let order_id = seed_order(&db, user_id).await;

    let mut tx = db.begin().await.expect("begin tx");
    service::apply_delta_tx(&mut tx, user_id, LedgerDelta::checkout_earn(10, order_id))
        .await
        .expect("earn should succeed");
    service::apply_delta_tx(&mut tx, user_id, LedgerDelta::checkout_redeem(30, order_id))
        .await
        .expect("redeem should succeed");

    let (earned, redeemed) = points_repo::find_order_flow_sums_tx(&mut tx, order_id)
        .await
        .expect("read back flow sums");
    tx.commit().await.expect("commit");

    assert_eq!(earned, 10, "checkout_earn magnitude reads back positive");
    assert_eq!(
        redeemed, 30,
        "checkout_redeem magnitude reads back positive despite the negative delta written"
    );
}

// ---------------------------------------------------------------------
// C3: `lock_balance_tx` returns a `BalanceLock` witness
// ---------------------------------------------------------------------

#[sqlx::test]
async fn lock_balance_tx_returns_witness_carrying_user_and_locked_balance(db: PgPool) {
    let user_id = common::seed_member(&db, "pts-lock-witness@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 42).await;

    let mut tx = db.begin().await.expect("begin tx");
    let lock = service::lock_balance_tx(&mut tx, user_id)
        .await
        .expect("lock should succeed for an existing user");
    tx.rollback().await.expect("rollback");

    assert_eq!(lock.user_id(), user_id);
    assert_eq!(lock.balance(), 42);

    // NotFound mapping unchanged through the new (witness-returning) signature.
    let mut tx = db.begin().await.expect("begin tx");
    let err = service::lock_balance_tx(&mut tx, Uuid::now_v7())
        .await
        .expect_err("nonexistent user must 404");
    tx.rollback().await.expect("rollback");

    assert!(
        matches!(err, AppError::NotFound(ref m) if m == "user not found"),
        "got {err:?}"
    );
}

/// Attempt a `try_spend_tx` spend inside its own transaction, committing on
/// success / rolling back on failure — used by the concurrent race test
/// below. Mirrors `service_rewards.rs`'s `attempt_redeem` helper.
async fn attempt_spend(db: PgPool, user_id: Uuid, cost: i64) -> Result<i64, AppError> {
    let mut tx = db.begin().await.expect("begin tx");
    let result = service::try_spend_tx(&mut tx, user_id, cost).await;
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
    let balance_after = service::try_spend_tx(&mut tx, user_id, 30)
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
    let err = service::try_spend_tx(&mut tx, user_id, 50)
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
    let err = service::try_spend_tx(&mut tx, user_id, 0)
        .await
        .expect_err("zero cost must be rejected");
    tx.rollback().await.expect("rollback");
    assert!(matches!(err, AppError::Validation(_)), "got {err:?}");

    let mut tx = db.begin().await.expect("begin tx");
    let err = service::try_spend_tx(&mut tx, user_id, -10)
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

// ---------------------------------------------------------------------
// `POST /points/adjustments` (admin) — service::adjust_points
// ---------------------------------------------------------------------

#[sqlx::test]
async fn adjust_points_positive_delta_increases_balance_and_writes_admin_adjust_ledger_row(
    db: PgPool,
) {
    let user_id = common::seed_member(&db, "pts-adj-pos@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 100).await;

    let result = service::adjust_points(
        &db,
        &AdjustPointsRequest {
            user_id,
            delta: 50,
            expected_balance: 100,
        },
    )
    .await
    .expect("adjustment should succeed");

    assert_eq!(result.user_id, user_id);
    assert_eq!(result.balance, 150);

    let balance = points_repo::find_balance(&db, user_id)
        .await
        .expect("query balance")
        .expect("user exists");
    assert_eq!(balance, 150, "users.points_balance must match the response");

    let ledger = points_repo::find_ledger_by_user(&db, user_id, 10, 0)
        .await
        .expect("query ledger");
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].delta, 50);
    assert_eq!(ledger[0].balance_after, 150);
    assert_eq!(ledger[0].reason, PointReason::AdminAdjust);
    assert_eq!(ledger[0].order_id, None);
}

#[sqlx::test]
async fn adjust_points_negative_delta_decreases_balance_and_writes_admin_adjust_ledger_row(
    db: PgPool,
) {
    let user_id = common::seed_member(&db, "pts-adj-neg@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 100).await;

    let result = service::adjust_points(
        &db,
        &AdjustPointsRequest {
            user_id,
            delta: -30,
            expected_balance: 100,
        },
    )
    .await
    .expect("adjustment should succeed");

    assert_eq!(result.user_id, user_id);
    assert_eq!(result.balance, 70);

    let ledger = points_repo::find_ledger_by_user(&db, user_id, 10, 0)
        .await
        .expect("query ledger");
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].delta, -30);
    assert_eq!(ledger[0].balance_after, 70);
    assert_eq!(ledger[0].reason, PointReason::AdminAdjust);
    assert_eq!(ledger[0].order_id, None);
}

/// CAS mismatch (`adjust_points`'s core CAS behaviour): `expected_balance` doesn't
/// match the actual locked balance → `Conflict`, nothing persisted. The
/// message names both sides so an admin can immediately re-judge from the
/// response without a follow-up query.
#[sqlx::test]
async fn adjust_points_balance_mismatch_returns_conflict_and_does_not_persist(db: PgPool) {
    let user_id = common::seed_member(&db, "pts-adj-cas@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 100).await;

    let err = service::adjust_points(
        &db,
        &AdjustPointsRequest {
            user_id,
            delta: 50,
            expected_balance: 999, // stale/incorrect caller expectation
        },
    )
    .await
    .expect_err("balance mismatch must be rejected");

    match err {
        AppError::Conflict(msg) => {
            assert!(
                msg.contains("999") && msg.contains("100"),
                "message should surface both expected and actual balances, got {msg:?}"
            );
        }
        other => panic!("expected Conflict, got {other:?}"),
    }

    let balance = points_repo::find_balance(&db, user_id)
        .await
        .expect("query balance")
        .expect("user exists");
    assert_eq!(
        balance, 100,
        "balance must be unchanged after a CAS mismatch"
    );

    let ledger = points_repo::find_ledger_by_user(&db, user_id, 10, 0)
        .await
        .expect("query ledger");
    assert!(ledger.is_empty(), "no ledger row should have been written");
}

/// CAS passes (the caller's `expected_balance` is accurate), but the
/// deduction itself would drive the balance negative — distinct failure
/// mode from the CAS mismatch above, surfaced via `apply_delta_tx`'s
/// existing `users_points_balance_check` mapping.
#[sqlx::test]
async fn adjust_points_negative_delta_exceeding_balance_returns_conflict_and_does_not_persist(
    db: PgPool,
) {
    let user_id = common::seed_member(&db, "pts-adj-poor@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 20).await;

    let err = service::adjust_points(
        &db,
        &AdjustPointsRequest {
            user_id,
            delta: -50,
            expected_balance: 20, // CAS matches; the deduction itself is what fails
        },
    )
    .await
    .expect_err("insufficient balance must be rejected");

    match err {
        AppError::Conflict(msg) => assert_eq!(msg, "點數不足"),
        other => panic!("expected Conflict(\"點數不足\"), got {other:?}"),
    }

    let balance = points_repo::find_balance(&db, user_id)
        .await
        .expect("query balance")
        .expect("user exists");
    assert_eq!(balance, 20, "balance must be unchanged");

    let ledger = points_repo::find_ledger_by_user(&db, user_id, 10, 0)
        .await
        .expect("query ledger");
    assert!(ledger.is_empty(), "no ledger row should have been written");
}

#[sqlx::test]
async fn adjust_points_nonexistent_user_returns_not_found(db: PgPool) {
    let err = service::adjust_points(
        &db,
        &AdjustPointsRequest {
            user_id: Uuid::now_v7(),
            delta: 10,
            expected_balance: 0,
        },
    )
    .await
    .expect_err("nonexistent user must 404");

    assert!(
        matches!(err, AppError::NotFound(ref m) if m == "user not found"),
        "got {err:?}"
    );
}
