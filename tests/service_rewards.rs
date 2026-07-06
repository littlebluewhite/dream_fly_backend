//! Integration tests for `rewards::service`.
//!
//! Covers the atomic redeem transaction (`service::redeem`): success across
//! all four tables (ledger/balance/stock/redemption), insufficient balance
//! (409, zero side effects), zero stock (409), unlimited stock (no
//! decrement), and the concurrent double-redeem race for the last unit of
//! stock — mirrors `service_orders.rs`'s
//! `concurrent_checkout_last_unit_only_succeeds_once` /
//! `service_leave.rs`'s `concurrent_makeup_different_requests_last_seat_only_one_wins`
//! `tokio::spawn` + `tokio::join!` technique.

mod common;

use sqlx::PgPool;
use uuid::Uuid;

use common::fixtures::{seed_point_ledger_entry, seed_reward, set_points_balance};
use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::points::model::PointReason;
use dream_fly_backend::modules::points::repository as points_repo;
use dream_fly_backend::modules::rewards::service;

async fn attempt_redeem(db: PgPool, user_id: Uuid, reward_id: Uuid) -> bool {
    service::redeem(&db, user_id, reward_id).await.is_ok()
}

#[sqlx::test]
async fn redeem_success_updates_ledger_balance_stock_and_redemption(db: PgPool) {
    let user_id = common::seed_member(&db, "redeem-ok@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 100).await;
    let reward_id = seed_reward(&db, "Water Bottle", 30, Some(2), true, 0).await;

    let resp = service::redeem(&db, user_id, reward_id)
        .await
        .expect("redeem should succeed");

    assert_eq!(resp.points_spent, 30);
    assert_eq!(resp.balance_after, 70);

    // users.points_balance synced.
    let balance = points_repo::find_balance(&db, user_id)
        .await
        .expect("query balance")
        .expect("user exists");
    assert_eq!(balance, 70);

    // point_ledger row written via the shared points mechanism.
    let ledger = points_repo::find_ledger_by_user(&db, user_id, 10, 0)
        .await
        .expect("query ledger");
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].delta, -30);
    assert_eq!(ledger[0].balance_after, 70);
    assert_eq!(ledger[0].reason, PointReason::Redeem);
    assert_eq!(ledger[0].order_id, None);

    // rewards.stock decremented.
    let stock: Option<i32> = sqlx::query_scalar("SELECT stock FROM rewards WHERE id = $1")
        .bind(reward_id)
        .fetch_one(&db)
        .await
        .expect("fetch stock");
    assert_eq!(stock, Some(1));

    // reward_redemptions row inserted.
    let row: (Uuid, Uuid, i32) = sqlx::query_as(
        "SELECT reward_id, user_id, points_spent FROM reward_redemptions WHERE id = $1",
    )
    .bind(resp.redemption_id)
    .fetch_one(&db)
    .await
    .expect("fetch redemption row");
    assert_eq!(row.0, reward_id);
    assert_eq!(row.1, user_id);
    assert_eq!(row.2, 30);
}

#[sqlx::test]
async fn redeem_unlimited_stock_leaves_stock_null(db: PgPool) {
    let user_id = common::seed_member(&db, "redeem-unlimited@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 50).await;
    let reward_id = seed_reward(&db, "Sticker", 10, None, true, 0).await;

    service::redeem(&db, user_id, reward_id)
        .await
        .expect("redeem should succeed");

    let stock: Option<i32> = sqlx::query_scalar("SELECT stock FROM rewards WHERE id = $1")
        .bind(reward_id)
        .fetch_one(&db)
        .await
        .expect("fetch stock");
    assert_eq!(stock, None, "unlimited stock must stay NULL");
}

#[sqlx::test]
async fn redeem_insufficient_balance_conflict_with_zero_side_effects(db: PgPool) {
    let user_id = common::seed_member(&db, "redeem-poor@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 5).await;
    let reward_id = seed_reward(&db, "Hoodie", 100, Some(3), true, 0).await;

    let err = service::redeem(&db, user_id, reward_id)
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
    assert_eq!(balance, 5, "balance must be unchanged");

    let ledger = points_repo::find_ledger_by_user(&db, user_id, 10, 0)
        .await
        .expect("query ledger");
    assert!(ledger.is_empty(), "no ledger row should have been written");

    let stock: Option<i32> = sqlx::query_scalar("SELECT stock FROM rewards WHERE id = $1")
        .bind(reward_id)
        .fetch_one(&db)
        .await
        .expect("fetch stock");
    assert_eq!(stock, Some(3), "stock must be unchanged");

    let redemption_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM reward_redemptions WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&db)
            .await
            .expect("count redemptions");
    assert_eq!(redemption_count, 0, "no redemption row should have been written");
}

#[sqlx::test]
async fn redeem_zero_stock_returns_conflict(db: PgPool) {
    let user_id = common::seed_member(&db, "redeem-oos@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 1000).await;
    let reward_id = seed_reward(&db, "Limited Tee", 20, Some(0), true, 0).await;

    let err = service::redeem(&db, user_id, reward_id)
        .await
        .expect_err("zero stock must be rejected");

    match err {
        AppError::Conflict(msg) => assert_eq!(msg, "已兌換完畢"),
        other => panic!("expected Conflict(\"已兌換完畢\"), got {other:?}"),
    }

    let balance = points_repo::find_balance(&db, user_id)
        .await
        .expect("query balance")
        .expect("user exists");
    assert_eq!(balance, 1000, "balance must be unchanged when stock blocks the redeem");
}

#[sqlx::test]
async fn redeem_inactive_reward_returns_not_found(db: PgPool) {
    let user_id = common::seed_member(&db, "redeem-inactive@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 1000).await;
    let reward_id = seed_reward(&db, "Disabled Reward", 20, None, false, 0).await;

    let err = service::redeem(&db, user_id, reward_id)
        .await
        .expect_err("inactive reward must 404");

    assert!(matches!(err, AppError::NotFound(_)), "got {err:?}");
}

#[sqlx::test]
async fn redeem_unknown_reward_returns_not_found(db: PgPool) {
    let user_id = common::seed_member(&db, "redeem-unknown@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 1000).await;

    let err = service::redeem(&db, user_id, Uuid::now_v7())
        .await
        .expect_err("unknown reward must 404");

    assert!(matches!(err, AppError::NotFound(_)), "got {err:?}");
}

#[sqlx::test]
async fn concurrent_redeem_last_stock_unit_only_one_succeeds(db: PgPool) {
    // Two concurrent redeems of the same reward with exactly one unit of
    // stock left, by two different users each with enough balance. The `FOR
    // UPDATE` lock in `repository::lock_by_id_tx` must serialize the two
    // stock checks so only the first commits its decrement — the loser
    // re-reads stock=0 (post-commit) and gets the 409.
    let reward_id = seed_reward(&db, "Last Unit Cap", 10, Some(1), true, 0).await;
    let user_a = common::seed_member(&db, "redeem-race-a@example.com", "Password!234").await;
    let user_b = common::seed_member(&db, "redeem-race-b@example.com", "Password!234").await;
    set_points_balance(&db, user_a, 100).await;
    set_points_balance(&db, user_b, 100).await;

    let (res_a, res_b) = tokio::join!(
        tokio::spawn(attempt_redeem(db.clone(), user_a, reward_id)),
        tokio::spawn(attempt_redeem(db.clone(), user_b, reward_id)),
    );
    let ok_count = [res_a.expect("task a panicked"), res_b.expect("task b panicked")]
        .iter()
        .filter(|ok| **ok)
        .count();
    assert_eq!(ok_count, 1, "exactly one concurrent redeem should succeed");

    let stock: Option<i32> = sqlx::query_scalar("SELECT stock FROM rewards WHERE id = $1")
        .bind(reward_id)
        .fetch_one(&db)
        .await
        .expect("fetch stock");
    assert_eq!(stock, Some(0), "stock must not go negative");

    let redemption_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM reward_redemptions WHERE reward_id = $1")
            .bind(reward_id)
            .fetch_one(&db)
            .await
            .expect("count redemptions");
    assert_eq!(redemption_count, 1, "exactly one redemption row must exist");
}

#[sqlx::test]
async fn my_redemptions_joins_reward_name_and_paginates(db: PgPool) {
    let user_id = common::seed_member(&db, "redeem-history@example.com", "Password!234").await;
    let reward_id = seed_reward(&db, "History Reward", 15, None, true, 0).await;

    // Seed ledger noise so the redemptions list isn't accidentally reading
    // from point_ledger instead of reward_redemptions.
    set_points_balance(&db, user_id, 100).await;
    seed_point_ledger_entry(&db, user_id, -15, 85, "redeem", None, chrono::Utc::now()).await;

    for _ in 0..3 {
        service::redeem(&db, user_id, reward_id).await.expect("redeem");
    }

    let page = service::my_redemptions(
        &db,
        user_id,
        &dream_fly_backend::extractors::pagination::PaginationParams { page: 1, per_page: 2 },
    )
    .await
    .expect("my_redemptions");

    assert_eq!(page.redemptions.len(), 2);
    assert_eq!(page.total, 3);
    assert_eq!(page.page, 1);
    assert_eq!(page.per_page, 2);
    assert_eq!(page.redemptions[0].reward_name, "History Reward");
    assert_eq!(page.redemptions[0].points_spent, 15);
}
