//! Integration tests for `orders::service::checkout`.
//!
//! The checkout flow is the most concurrency-sensitive code path in the
//! application: it reads the cart under `FOR UPDATE`, decrements product
//! stock atomically, creates an order + order_items, and clears the cart —
//! all inside a single transaction. These tests exercise the happy path and
//! the critical race-condition boundary (two users, one last unit).

mod common;

use sqlx::PgPool;
use std::sync::Arc;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::orders::service;

#[sqlx::test]
async fn checkout_creates_order_and_clears_cart(db: PgPool) {
    let user = common::seed_member(&db, "buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "prod-1", 1500, Some(5)).await;
    common::add_to_cart(&db, user, product, 2).await;

    let resp = service::checkout(&db, user, None).await.expect("checkout");

    assert_eq!(resp.total_cents, 3000);
    assert_eq!(resp.items.len(), 1);
    assert_eq!(resp.items[0].quantity, 2);
    assert_eq!(resp.items[0].unit_price_cents, 1500);

    // Cart is now empty
    let cart_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cart_items WHERE user_id = $1")
        .bind(user)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(cart_count, 0, "cart must be cleared after checkout");

    // Exactly one order row exists
    let order_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orders WHERE user_id = $1")
        .bind(user)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(order_count, 1);
}

#[sqlx::test]
async fn checkout_decrements_stock(db: PgPool) {
    let user = common::seed_member(&db, "buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "prod-1", 1000, Some(3)).await;
    common::add_to_cart(&db, user, product, 2).await;

    service::checkout(&db, user, None).await.expect("checkout");

    assert_eq!(common::product_stock(&db, product).await, Some(1));
}

#[sqlx::test]
async fn checkout_unlimited_stock_unchanged(db: PgPool) {
    // Products with NULL stock (tickets / memberships) are unlimited —
    // checkout must not touch the column.
    let user = common::seed_member(&db, "buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "ticket-1", 500, None).await;
    common::add_to_cart(&db, user, product, 10).await;

    service::checkout(&db, user, None).await.expect("checkout");

    assert_eq!(common::product_stock(&db, product).await, None);
}

#[sqlx::test]
async fn checkout_fails_on_insufficient_stock(db: PgPool) {
    let user = common::seed_member(&db, "buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "prod-1", 1000, Some(1)).await;
    common::add_to_cart(&db, user, product, 2).await;

    let err = service::checkout(&db, user, None)
        .await
        .expect_err("insufficient stock should fail");
    assert!(matches!(err, AppError::Conflict(_)), "got: {err:?}");

    // Transaction rolled back: cart intact, no order created, stock unchanged.
    let cart_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cart_items WHERE user_id = $1")
        .bind(user)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(cart_count, 1, "cart should still exist after failed checkout");

    let order_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orders WHERE user_id = $1")
        .bind(user)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(order_count, 0);

    assert_eq!(common::product_stock(&db, product).await, Some(1));
}

#[sqlx::test]
async fn checkout_empty_cart_fails(db: PgPool) {
    let user = common::seed_member(&db, "buyer@example.com", "passw0rd!").await;

    let err = service::checkout(&db, user, None)
        .await
        .expect_err("empty cart should fail");
    assert!(matches!(err, AppError::BadRequest(_)), "got: {err:?}");
}

#[sqlx::test]
async fn concurrent_checkout_last_unit_only_succeeds_once(db: PgPool) {
    // The crown-jewel race test: two users, one unit of stock, both hit
    // checkout simultaneously. Exactly one should succeed, the other should
    // fail with Conflict, and the product should end up with 0 stock and
    // exactly 1 order.
    let user_a = common::seed_member(&db, "a@example.com", "passw0rd!").await;
    let user_b = common::seed_member(&db, "b@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "prod-1", 1000, Some(1)).await;

    common::add_to_cart(&db, user_a, product, 1).await;
    common::add_to_cart(&db, user_b, product, 1).await;

    let db_a = Arc::new(db.clone());
    let db_b = Arc::new(db.clone());

    let task_a = tokio::spawn(async move {
        service::checkout(db_a.as_ref(), user_a, None).await
    });
    let task_b = tokio::spawn(async move {
        service::checkout(db_b.as_ref(), user_b, None).await
    });

    let (res_a, res_b) = tokio::join!(task_a, task_b);
    let res_a = res_a.expect("task a panicked");
    let res_b = res_b.expect("task b panicked");

    // Exactly one succeeded.
    let (ok_count, err_count) = [&res_a, &res_b]
        .iter()
        .fold((0, 0), |(o, e), r| match r {
            Ok(_) => (o + 1, e),
            Err(_) => (o, e + 1),
        });
    assert_eq!(ok_count, 1, "exactly one checkout should succeed");
    assert_eq!(err_count, 1, "the other should fail");

    // The failure is a Conflict.
    let failed = match (res_a, res_b) {
        (Err(e), _) | (_, Err(e)) => e,
        _ => unreachable!(),
    };
    assert!(matches!(failed, AppError::Conflict(_)), "got: {failed:?}");

    // Final state: stock = 0, exactly one order exists.
    assert_eq!(common::product_stock(&db, product).await, Some(0));

    let total_orders: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orders")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(total_orders, 1);
}
