//! Integration tests for `orders::service::checkout`.
//!
//! The checkout flow is the most concurrency-sensitive code path in the
//! application: it reads the cart under `FOR UPDATE`, decrements product
//! stock atomically, resolves a coupon and points redemption, creates an
//! order + order_items (product and course lines), grants the resulting
//! enrolments/subscriptions, adjusts the points ledger, and clears the cart —
//! all inside a single transaction. These tests exercise the happy path, the
//! coupon/points math, the mixed product+course artifact creation, the
//! critical race-condition boundary (two users, one last unit), and the
//! full-rollback guarantee when an artifact step (course capacity) fails.

mod common;

use sqlx::PgPool;
use std::sync::Arc;

use common::add_course_to_cart;
use common::fixtures::{
    seed_course_with_capacity, seed_coupon, seed_enrolment, seed_entitlement_product,
    set_points_balance,
};
use dream_fly_backend::error::AppError;
use dream_fly_backend::extractors::pagination::PaginationParams;
use dream_fly_backend::modules::coupons::dto::UpdateCouponRequest;
use dream_fly_backend::modules::coupons::service as coupons_service;
use dream_fly_backend::modules::orders::dto::CheckoutRequest;
use dream_fly_backend::modules::orders::service;

#[sqlx::test]
async fn checkout_creates_order_and_clears_cart(db: PgPool) {
    let user = common::seed_member(&db, "buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "prod-1", 1500, Some(5)).await;
    common::add_to_cart(&db, user, product, 2).await;

    let resp = service::checkout(&db, user, None, CheckoutRequest::default(), None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect("checkout");

    assert_eq!(resp.total_cents, 3000);
    assert_eq!(resp.items.len(), 1);
    assert_eq!(resp.items[0].quantity, 2);
    assert_eq!(resp.items[0].unit_price_cents, 1500);
    assert_eq!(resp.items[0].item_type, "product");

    // No coupon/points regression: a plain product checkout still behaves
    // exactly as before, and now also earns points — 5% of NT$30 (3000
    // cents), rounded to the nearest point: (30*5+50)/100 = 2.
    assert_eq!(resp.coupon_code, None);
    assert_eq!(resp.points_used, 0);
    assert_eq!(resp.points_earned, 2);

    // Cart is now empty
    let cart_count = common::cart_count(&db, user).await;
    assert_eq!(cart_count, 0, "cart must be cleared after checkout");

    // Exactly one order row exists
    let order_count = common::order_count(&db, user).await;
    assert_eq!(order_count, 1);

    // An "Order Placed" notification is written post-commit.
    let (title, _) = common::latest_notification(&db, user, "order_placed")
        .await
        .expect("order placed notification row");
    assert_eq!(title, "Order Placed");
}

#[sqlx::test]
async fn checkout_decrements_stock(db: PgPool) {
    let user = common::seed_member(&db, "buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "prod-1", 1000, Some(3)).await;
    common::add_to_cart(&db, user, product, 2).await;

    service::checkout(&db, user, None, CheckoutRequest::default(), None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect("checkout");

    assert_eq!(common::product_stock(&db, product).await, Some(1));
}

#[sqlx::test]
async fn checkout_unlimited_stock_unchanged(db: PgPool) {
    // Products with NULL stock (tickets / memberships) are unlimited —
    // checkout must not touch the column.
    let user = common::seed_member(&db, "buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "ticket-1", 500, None).await;
    common::add_to_cart(&db, user, product, 10).await;

    service::checkout(&db, user, None, CheckoutRequest::default(), None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect("checkout");

    assert_eq!(common::product_stock(&db, product).await, None);
}

#[sqlx::test]
async fn checkout_fails_on_insufficient_stock(db: PgPool) {
    let user = common::seed_member(&db, "buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "prod-1", 1000, Some(1)).await;
    common::add_to_cart(&db, user, product, 2).await;

    let err = service::checkout(&db, user, None, CheckoutRequest::default(), None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect_err("insufficient stock should fail");
    assert!(matches!(err, AppError::Conflict(_)), "got: {err:?}");

    // Transaction rolled back: cart intact, no order created, stock unchanged.
    let cart_count = common::cart_count(&db, user).await;
    assert_eq!(
        cart_count, 1,
        "cart should still exist after failed checkout"
    );

    let order_count = common::order_count(&db, user).await;
    assert_eq!(order_count, 0);

    assert_eq!(common::product_stock(&db, product).await, Some(1));
}

#[sqlx::test]
async fn checkout_empty_cart_fails(db: PgPool) {
    let user = common::seed_member(&db, "buyer@example.com", "passw0rd!").await;

    let err = service::checkout(&db, user, None, CheckoutRequest::default(), None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect_err("empty cart should fail");
    assert!(matches!(err, AppError::BadRequest(_)), "got: {err:?}");
}

#[sqlx::test]
async fn checkout_course_and_product_mix_creates_both_artifacts(db: PgPool) {
    let user = common::seed_member(&db, "mixed-buyer@example.com", "passw0rd!").await;
    let course = seed_course_with_capacity(&db, "Mixed Cart Course", None, 12).await;
    let product = seed_entitlement_product(&db, "membership-mix", "membership", 8000, None, None).await;

    add_course_to_cart(&db, user, course).await;
    common::add_to_cart(&db, user, product, 1).await;

    let resp = service::checkout(&db, user, None, CheckoutRequest::default(), None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect("checkout");

    // order_items: two lines, correctly discriminated.
    assert_eq!(resp.items.len(), 2);
    let course_item = resp
        .items
        .iter()
        .find(|i| i.item_type == "course")
        .expect("a course order_item");
    assert_eq!(course_item.course_id, Some(course));
    assert_eq!(course_item.product_id, None);
    let product_item = resp
        .items
        .iter()
        .find(|i| i.item_type == "product")
        .expect("a product order_item");
    assert_eq!(product_item.product_id, Some(product));
    assert_eq!(product_item.course_id, None);

    // Exactly one enrolment + one subscription were produced, and both are
    // reflected directly in the response.
    assert_eq!(resp.enrolments.len(), 1);
    assert_eq!(resp.enrolments[0].course_id, course);
    assert_eq!(resp.subscriptions.len(), 1);
    assert_eq!(resp.subscriptions[0].product_id, product);

    let enrolment_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM enrolments WHERE user_id = $1 AND course_id = $2 AND status = 'active'",
    )
    .bind(user)
    .bind(course)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(enrolment_count, 1);

    let subscription_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM subscriptions WHERE user_id = $1 AND product_id = $2",
    )
    .bind(user)
    .bind(product)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(subscription_count, 1);
}

#[sqlx::test]
async fn checkout_with_valid_coupon_applies_discount(db: PgPool) {
    let user = common::seed_member(&db, "coupon-buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "coupon-prod", 30_000, Some(5)).await;
    common::add_to_cart(&db, user, product, 1).await;
    common::fixtures::seed_coupon(&db, "DREAMFLY100", 10_000, true, None).await;

    let req = CheckoutRequest {
        coupon_code: Some("DREAMFLY100".to_string()),
        use_points: None,
        payment_method: None,
    };
    let resp = service::checkout(&db, user, None, req, None, &common::test_server_config(), chrono::Utc::now()).await.expect("checkout");

    assert_eq!(resp.discount_cents, 10_000);
    assert_eq!(resp.coupon_code, Some("DREAMFLY100".to_string()));
    assert_eq!(resp.total_cents, 20_000);
}

#[sqlx::test]
async fn checkout_coupon_over_half_subtotal_succeeds(db: PgPool) {
    // Regression (task-9 review): the original `orders_discount_bound`
    // CHECK compared `discount_cents` against the *post-discount* total, so
    // any coupon worth more than half the subtotal tripped it at INSERT
    // time and surfaced as a 500. Migration 20260704000002 relaxes the
    // constraint to non-negativity only; the app-level
    // `min(discount, subtotal)` clamp is the real upper bound.
    let user = common::seed_member(&db, "bigcoupon-buyer@example.com", "passw0rd!").await;
    // NT$150 product + NT$100 coupon: discount (10000) > 50% of subtotal (15000).
    let product = common::seed_product(&db, "bigcoupon-prod", 15_000, Some(5)).await;
    common::add_to_cart(&db, user, product, 1).await;
    common::fixtures::seed_coupon(&db, "BIG100", 10_000, true, None).await;

    let req = CheckoutRequest {
        coupon_code: Some("BIG100".to_string()),
        use_points: None,
        payment_method: None,
    };
    let resp = service::checkout(&db, user, None, req, None, &common::test_server_config(), chrono::Utc::now()).await.expect("checkout");

    assert_eq!(resp.discount_cents, 10_000);
    assert_eq!(resp.total_cents, 5_000);
}

#[sqlx::test]
async fn checkout_coupon_at_or_above_subtotal_clamps_to_free_order(db: PgPool) {
    let user = common::seed_member(&db, "freecoupon-buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "freecoupon-prod", 5_000, Some(5)).await;
    common::add_to_cart(&db, user, product, 1).await;
    // Coupon worth twice the subtotal — clamped down to the subtotal.
    common::fixtures::seed_coupon(&db, "MEGA", 10_000, true, None).await;

    let req = CheckoutRequest {
        coupon_code: Some("MEGA".to_string()),
        use_points: None,
        payment_method: None,
    };
    let resp = service::checkout(&db, user, None, req, None, &common::test_server_config(), chrono::Utc::now()).await.expect("checkout");

    assert_eq!(resp.discount_cents, 5_000, "discount clamps to the subtotal");
    assert_eq!(resp.total_cents, 0);
    assert_eq!(resp.points_earned, 0, "a free order earns no points");

    // points_earned == 0 must also mean no earn ledger row was written.
    let earn_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM point_ledger WHERE user_id = $1 AND reason = 'checkout_earn'::point_reason",
    )
    .bind(user)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(earn_count, 0);
}

#[sqlx::test]
async fn checkout_coupon_plus_points_can_reach_zero_total(db: PgPool) {
    let user = common::seed_member(&db, "combo-buyer@example.com", "passw0rd!").await;
    set_points_balance(&db, user, 100).await;
    // Subtotal NT$200; the NT$100 coupon leaves NT$100 payable; 100 points
    // cover exactly the rest — total lands on 0 with discount 10000 stored.
    let product = common::seed_product(&db, "combo-prod", 20_000, Some(5)).await;
    common::add_to_cart(&db, user, product, 1).await;
    common::fixtures::seed_coupon(&db, "COMBO100", 10_000, true, None).await;

    let req = CheckoutRequest {
        coupon_code: Some("COMBO100".to_string()),
        use_points: Some(true),
        payment_method: None,
    };
    let resp = service::checkout(&db, user, None, req, None, &common::test_server_config(), chrono::Utc::now()).await.expect("checkout");

    assert_eq!(resp.discount_cents, 10_000);
    assert_eq!(resp.points_used, 100);
    assert_eq!(resp.total_cents, 0);
    assert_eq!(resp.points_earned, 0, "a fully-covered order earns nothing");

    let balance = common::points_balance_of(&db, user).await;
    assert_eq!(balance, 0, "all 100 points redeemed, none earned back");
}

#[sqlx::test]
async fn checkout_with_invalid_coupon_returns_validation_error(db: PgPool) {
    let user = common::seed_member(&db, "badcoupon-buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "prod-1", 1500, Some(5)).await;
    common::add_to_cart(&db, user, product, 1).await;

    let req = CheckoutRequest {
        coupon_code: Some("NOSUCHCODE".to_string()),
        use_points: None,
        payment_method: None,
    };
    let err = service::checkout(&db, user, None, req, None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect_err("invalid coupon should fail");
    assert!(matches!(err, AppError::Validation(_)), "got: {err:?}");

    let order_count = common::order_count(&db, user).await;
    assert_eq!(order_count, 0, "no order should be created on invalid coupon");
}

/// Round 4 Task B3: `PATCH /coupons/{id}` setting `is_active: false` is the
/// primary "retire this code" path (vs. hard `DELETE`), and it must actually
/// be honored by checkout, not just by `GET /coupons/{code}/validate`.
#[sqlx::test]
async fn checkout_with_deactivated_coupon_returns_validation_error(db: PgPool) {
    let user = common::seed_member(&db, "deactivated-coupon-buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "deactivated-coupon-prod", 1500, Some(5)).await;
    common::add_to_cart(&db, user, product, 1).await;

    let coupon_id = seed_coupon(&db, "WASACTIVE", 500, true, None).await;
    coupons_service::update_coupon(
        &db,
        coupon_id,
        UpdateCouponRequest {
            discount_cents: None,
            is_active: Some(false),
            expires_at: None,
        },
    )
    .await
    .expect("deactivate coupon");

    let req = CheckoutRequest {
        coupon_code: Some("WASACTIVE".to_string()),
        use_points: None,
        payment_method: None,
    };
    let err = service::checkout(&db, user, None, req, None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect_err("deactivated coupon should fail checkout");
    assert!(matches!(err, AppError::Validation(_)), "got: {err:?}");

    let order_count = common::order_count(&db, user).await;
    assert_eq!(order_count, 0, "no order should be created on deactivated coupon");
}

#[sqlx::test]
async fn checkout_use_points_caps_at_balance(db: PgPool) {
    let user = common::seed_member(&db, "points-buyer@example.com", "passw0rd!").await;
    set_points_balance(&db, user, 500).await;
    // Payable NT$3000 = 300,000 cents.
    let product = common::seed_product(&db, "points-prod", 300_000, Some(5)).await;
    common::add_to_cart(&db, user, product, 1).await;

    let req = CheckoutRequest {
        coupon_code: None,
        use_points: Some(true),
        payment_method: None,
    };
    let resp = service::checkout(&db, user, None, req, None, &common::test_server_config(), chrono::Utc::now()).await.expect("checkout");

    assert_eq!(resp.points_used, 500);
    assert_eq!(resp.total_cents, 300_000 - 50_000);

    let balance = common::points_balance_of(&db, user).await;
    // Started at 500, redeemed all 500 (-> 0), then earned this checkout's
    // own `points_earned` on top.
    assert_eq!(balance, resp.points_earned);
}

#[sqlx::test]
async fn checkout_use_points_zero_balance_uses_none(db: PgPool) {
    let user = common::seed_member(&db, "nopoints-buyer@example.com", "passw0rd!").await;
    // No `set_points_balance` call — a fresh user's balance defaults to 0.
    let product = common::seed_product(&db, "nopoints-prod", 1000, Some(5)).await;
    common::add_to_cart(&db, user, product, 1).await;

    let req = CheckoutRequest {
        coupon_code: None,
        use_points: Some(true),
        payment_method: None,
    };
    let resp = service::checkout(&db, user, None, req, None, &common::test_server_config(), chrono::Utc::now()).await.expect("checkout");

    assert_eq!(resp.points_used, 0);
    assert_eq!(resp.total_cents, 1000);

    // No redeem ledger row should exist since points_used == 0 is skipped.
    let redeem_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM point_ledger WHERE user_id = $1 AND reason = 'checkout_redeem'::point_reason",
    )
    .bind(user)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(redeem_count, 0);
}

#[sqlx::test]
async fn checkout_full_course_rolls_back_everything(db: PgPool) {
    let other_user = common::seed_member(&db, "already-in@example.com", "passw0rd!").await;
    let user = common::seed_member(&db, "latecomer@example.com", "passw0rd!").await;
    let course = seed_course_with_capacity(&db, "Full Class", None, 1).await;
    seed_enrolment(&db, other_user, course, "active", chrono::Utc::now()).await;

    let product = common::seed_product(&db, "prod-1", 1000, Some(3)).await;
    add_course_to_cart(&db, user, course).await;
    common::add_to_cart(&db, user, product, 1).await;
    // Also request points redemption so, if the failure did NOT roll back
    // everything, a stray point_ledger row or balance mutation would show
    // up below. The enrolment check (course capacity) runs before the
    // points-ledger step, so this must never be reached either.
    set_points_balance(&db, user, 100).await;
    let req = CheckoutRequest {
        coupon_code: None,
        use_points: Some(true),
        payment_method: None,
    };

    let err = service::checkout(&db, user, None, req, None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect_err("full course must reject the whole checkout");
    assert!(matches!(err, AppError::Conflict(_)), "got: {err:?}");

    // Nothing was written: no order, enrolment count for the course
    // unchanged (still just the pre-existing one), stock untouched, no
    // ledger row, and the points balance untouched.
    let order_count = common::order_count(&db, user).await;
    assert_eq!(order_count, 0);

    let enrolment_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM enrolments WHERE course_id = $1 AND status = 'active'",
    )
    .bind(course)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(enrolment_count, 1, "enrolment count must be unchanged");

    assert_eq!(common::product_stock(&db, product).await, Some(3));

    let ledger_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM point_ledger WHERE user_id = $1")
        .bind(user)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(ledger_count, 0, "no ledger row should survive the rollback");

    let balance = common::points_balance_of(&db, user).await;
    assert_eq!(balance, 100, "points balance must be unchanged");

    // The cart itself is untouched too (checkout never got to clear it).
    let cart_count = common::cart_count(&db, user).await;
    assert_eq!(cart_count, 2);
}

#[sqlx::test]
async fn checkout_idempotent_replay_returns_same_order_with_artifacts(db: PgPool) {
    let user = common::seed_member(&db, "replay-buyer@example.com", "passw0rd!").await;
    let course = seed_course_with_capacity(&db, "Replay Course", None, 12).await;
    add_course_to_cart(&db, user, course).await;

    let key = Some("idempotency-key-1".to_string());
    let first = service::checkout(
        &db,
        user,
        key.clone(),
        CheckoutRequest::default(),
        None,
        &common::test_server_config(),
        chrono::Utc::now(),
    )
    .await
    .expect("first checkout");
    assert_eq!(first.enrolments.len(), 1);

    let second = service::checkout(
        &db,
        user,
        key,
        CheckoutRequest::default(),
        None,
        &common::test_server_config(),
        chrono::Utc::now(),
    )
    .await
    .expect("replayed checkout");

    assert_eq!(first.order_number, second.order_number);
    assert_eq!(second.enrolments.len(), 1, "replay must still surface artifacts");
    assert_eq!(second.enrolments[0].course_id, course);

    let enrolment_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM enrolments WHERE user_id = $1 AND course_id = $2",
    )
    .bind(user)
    .bind(course)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(enrolment_count, 1, "replay must not create a duplicate enrolment");

    let order_count = common::order_count(&db, user).await;
    assert_eq!(order_count, 1, "replay must not create a duplicate order");
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
        // Build the server config inside each task — a borrow can't cross the
        // `tokio::spawn` boundary into a `'static` future.
        let server = common::test_server_config();
        service::checkout(
            db_a.as_ref(),
            user_a,
            None,
            CheckoutRequest::default(),
            None,
            &server,
            chrono::Utc::now(),
        )
        .await
    });
    let task_b = tokio::spawn(async move {
        let server = common::test_server_config();
        service::checkout(
            db_b.as_ref(),
            user_b,
            None,
            CheckoutRequest::default(),
            None,
            &server,
            chrono::Utc::now(),
        )
        .await
    });

    let (res_a, res_b) = tokio::join!(task_a, task_b);
    let res_a = res_a.expect("task a panicked");
    let res_b = res_b.expect("task b panicked");

    // Exactly one succeeded.
    let (ok_count, err_count) = [&res_a, &res_b].iter().fold((0, 0), |(o, e), r| match r {
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

#[sqlx::test]
async fn my_orders_lists_items_per_order_without_cross_contamination(db: PgPool) {
    // Two separate orders, each with a different single product line. The
    // items aggregate (json_agg correlated subquery keyed on order_id) must
    // not leak order 1's item into order 2's summary or vice versa.
    let user = common::seed_member(&db, "items-buyer@example.com", "passw0rd!").await;
    let product_a = common::seed_product(&db, "item-a", 1000, Some(10)).await;
    let product_b = common::seed_product(&db, "item-b", 2000, Some(10)).await;

    common::add_to_cart(&db, user, product_a, 3).await;
    service::checkout(&db, user, None, CheckoutRequest::default(), None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect("checkout 1");

    common::add_to_cart(&db, user, product_b, 1).await;
    service::checkout(&db, user, None, CheckoutRequest::default(), None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect("checkout 2");

    let list = service::my_orders(&db, user, 1, 10).await.expect("my_orders");
    assert_eq!(list.orders.len(), 2);

    // Newest first (ORDER BY created_at DESC): order 2 (item-b) then order 1 (item-a).
    let newest = &list.orders[0];
    assert_eq!(newest.items.len(), 1, "newest order must only carry its own item");
    assert_eq!(newest.items[0].name, "Test Product item-b");
    assert_eq!(newest.items[0].quantity, 1);

    let oldest = &list.orders[1];
    assert_eq!(oldest.items.len(), 1, "oldest order must only carry its own item");
    assert_eq!(oldest.items[0].name, "Test Product item-a");
    assert_eq!(oldest.items[0].quantity, 3);
}

#[sqlx::test]
async fn my_orders_aggregates_multiple_items_in_one_order(db: PgPool) {
    // A single order with two distinct product lines — both must appear in
    // that one order's `items`, with the right quantities.
    let user = common::seed_member(&db, "multi-item-buyer@example.com", "passw0rd!").await;
    let product_a = common::seed_product(&db, "multi-a", 1000, Some(10)).await;
    let product_b = common::seed_product(&db, "multi-b", 500, Some(10)).await;

    common::add_to_cart(&db, user, product_a, 2).await;
    common::add_to_cart(&db, user, product_b, 5).await;
    service::checkout(&db, user, None, CheckoutRequest::default(), None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect("checkout");

    let list = service::my_orders(&db, user, 1, 10).await.expect("my_orders");
    assert_eq!(list.orders.len(), 1);
    let items = &list.orders[0].items;
    assert_eq!(items.len(), 2, "both cart lines must appear in the summary");

    let a = items
        .iter()
        .find(|i| i.name == "Test Product multi-a")
        .expect("item a present");
    assert_eq!(a.quantity, 2);
    let b = items
        .iter()
        .find(|i| i.name == "Test Product multi-b")
        .expect("item b present");
    assert_eq!(b.quantity, 5);
}

#[sqlx::test]
async fn admin_list_orders_includes_items(db: PgPool) {
    let user = common::seed_member(&db, "admin-items-buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "admin-item", 1000, Some(10)).await;
    common::add_to_cart(&db, user, product, 4).await;
    service::checkout(&db, user, None, CheckoutRequest::default(), None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect("checkout");

    let pagination = PaginationParams {
        page: 1,
        per_page: 10,
    };
    let list = service::list_all_orders(&db, &pagination)
        .await
        .expect("list_all_orders");
    assert_eq!(list.orders.len(), 1);
    assert_eq!(list.orders[0].items.len(), 1);
    assert_eq!(list.orders[0].items[0].name, "Test Product admin-item");
    assert_eq!(list.orders[0].items[0].quantity, 4);
}

#[sqlx::test]
async fn update_order_status_transitions_and_notifies(db: PgPool) {
    // Checkout now creates the order already `paid` (checkout succeeding IS
    // the payment in this application — there's no separate capture step).
    // Transition it to `processing` (a valid edge from `paid`) and assert
    // both the persisted status and the "Order Update" notification the
    // seam writes post-commit.
    let user = common::seed_member(&db, "buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "prod-1", 1500, Some(5)).await;
    common::add_to_cart(&db, user, product, 1).await;

    let order = service::checkout(&db, user, None, CheckoutRequest::default(), None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect("checkout");
    assert_eq!(order.status, "paid");

    let updated = service::update_order_status(&db, order.id, "processing", None)
        .await
        .expect("update status");
    assert_eq!(updated.status, "processing");

    // Status persisted in the DB.
    let db_status: String = sqlx::query_scalar("SELECT status::text FROM orders WHERE id = $1")
        .bind(order.id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(db_status, "processing");

    // An "Order Update" notification (type order_status) is written.
    let (title, message) = common::latest_notification(&db, user, "order_status")
        .await
        .expect("order status notification row");
    assert_eq!(title, "Order Update");
    assert!(message.contains("processing"));
}

// ---------------------------------------------------------------------
// Task E2: `correlation_id` (x-request-id) threaded into the outbox payload
// ---------------------------------------------------------------------

#[sqlx::test]
async fn checkout_with_correlation_id_appears_in_outbox_payload(db: PgPool) {
    let user = common::seed_member(&db, "corr-buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "corr-prod", 1000, Some(5)).await;
    common::add_to_cart(&db, user, product, 1).await;

    service::checkout(
        &db,
        user,
        None,
        CheckoutRequest::default(),
        Some("rid-test-1".to_string()),
        &common::test_server_config(),
        chrono::Utc::now(),
    )
    .await
    .expect("checkout");

    let correlation_id: String =
        sqlx::query_scalar("SELECT payload->>'correlation_id' FROM events_outbox")
            .fetch_one(&db)
            .await
            .expect("order_created outbox row");
    assert_eq!(correlation_id, "rid-test-1");
}

#[sqlx::test]
async fn checkout_without_correlation_id_omits_payload_key(db: PgPool) {
    let user = common::seed_member(&db, "nocorr-buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "nocorr-prod", 1000, Some(5)).await;
    common::add_to_cart(&db, user, product, 1).await;

    service::checkout(&db, user, None, CheckoutRequest::default(), None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect("checkout");

    // `correlation_id` is skipped entirely at serialization time when `None`
    // (see `KafkaEvent`'s `skip_serializing_if`), so the key itself must be
    // absent from the JSONB payload rather than present with a JSON null.
    let has_key: bool = sqlx::query_scalar("SELECT payload ? 'correlation_id' FROM events_outbox")
        .fetch_one(&db)
        .await
        .expect("order_created outbox row");
    assert!(!has_key, "correlation_id key must be absent when None");
}
