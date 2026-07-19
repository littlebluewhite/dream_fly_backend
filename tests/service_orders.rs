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
use uuid::Uuid;

use common::add_course_to_cart;
use common::fixtures::{
    SeedCartLine, seed_carted_member, seed_course_with_capacity, seed_coupon,
    seed_entitlement_product, seed_full_course, seed_order_with_item, set_points_balance,
};
use dream_fly_backend::error::AppError;
use dream_fly_backend::extractors::pagination::PaginationParams;
use dream_fly_backend::modules::cart::repository as cart_repository;
use dream_fly_backend::modules::coupons::dto::UpdateCouponRequest;
use dream_fly_backend::modules::coupons::service as coupons_service;
use dream_fly_backend::modules::enrolments::service as enrolments_service;
use dream_fly_backend::modules::orders::dto::{CheckoutRequest, OrderResponse};
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
async fn checkout_records_stock_decremented_snapshot(db: PgPool) {
    // Step 10c: `order_items.stock_decremented` must reflect each line's
    // own checkout-time fate, not "did this order touch stock at all" — a
    // mixed cart (finite-stock product, NULL-stock/unlimited product,
    // course) exercises all three outcomes in one checkout. The response
    // DTO deliberately doesn't expose this column (wire safety), so this
    // reads `order_items` directly.
    let course = seed_course_with_capacity(&db, "Snapshot Course", None, 12).await;
    let limited = common::seed_product(&db, "snapshot-limited", 1000, Some(5)).await;
    let unlimited = common::seed_product(&db, "snapshot-unlimited", 500, None).await;
    let user = seed_carted_member(
        &db,
        "snapshot-buyer@example.com",
        &[
            SeedCartLine::Product { product_id: limited, quantity: 1 },
            SeedCartLine::Product { product_id: unlimited, quantity: 1 },
            SeedCartLine::Course { course_id: course },
        ],
        0,
    )
    .await;

    service::checkout(&db, user, None, CheckoutRequest::default(), None, &common::test_server_config(), chrono::Utc::now())
        .await
        .expect("checkout");

    let rows: Vec<(Option<uuid::Uuid>, Option<uuid::Uuid>, bool)> = sqlx::query_as(
        "SELECT oi.product_id, oi.course_id, oi.stock_decremented \
         FROM order_items oi JOIN orders o ON o.id = oi.order_id \
         WHERE o.user_id = $1",
    )
    .bind(user)
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);

    let limited_row = rows
        .iter()
        .find(|(pid, _, _)| *pid == Some(limited))
        .expect("limited product line");
    assert!(
        limited_row.2,
        "finite-stock product line must record stock_decremented = true"
    );

    let unlimited_row = rows
        .iter()
        .find(|(pid, _, _)| *pid == Some(unlimited))
        .expect("unlimited product line");
    assert!(
        !unlimited_row.2,
        "NULL-stock product line must record stock_decremented = false"
    );

    let course_row = rows
        .iter()
        .find(|(_, cid, _)| *cid == Some(course))
        .expect("course line");
    assert!(
        !course_row.2,
        "course line must record stock_decremented = false"
    );
}

#[sqlx::test]
async fn checkout_course_and_product_mix_creates_both_artifacts(db: PgPool) {
    let course = seed_course_with_capacity(&db, "Mixed Cart Course", None, 12).await;
    let product = seed_entitlement_product(&db, "membership-mix", "membership", 8000, None, None).await;
    let user = seed_carted_member(
        &db,
        "mixed-buyer@example.com",
        &[
            SeedCartLine::Course { course_id: course },
            SeedCartLine::Product { product_id: product, quantity: 1 },
        ],
        0,
    )
    .await;

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
    let full_course = seed_full_course(&db, "Full Class", 1).await;
    let product = common::seed_product(&db, "prod-1", 1000, Some(3)).await;
    // Also request points redemption so, if the failure did NOT roll back
    // everything, a stray point_ledger row or balance mutation would show
    // up below. The enrolment check (course capacity) runs before the
    // points-ledger step, so this must never be reached either.
    let user = seed_carted_member(
        &db,
        "latecomer@example.com",
        &[
            SeedCartLine::Course { course_id: full_course.course },
            SeedCartLine::Product { product_id: product, quantity: 1 },
        ],
        100,
    )
    .await;
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
    .bind(full_course.course)
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

    // ...and no order_placed notification either: the notify:: call sits
    // after commit, so a rolled-back checkout must never reach it.
    assert!(
        common::latest_notification(&db, user, "order_placed")
            .await
            .is_none(),
        "rollback must not leave a ghost order_placed notification"
    );
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
    let product = common::seed_product(&db, "prod-1", 1000, Some(1)).await;
    let user_a = seed_carted_member(
        &db,
        "a@example.com",
        &[SeedCartLine::Product { product_id: product, quantity: 1 }],
        0,
    )
    .await;
    let user_b = seed_carted_member(
        &db,
        "b@example.com",
        &[SeedCartLine::Product { product_id: product, quantity: 1 }],
        0,
    )
    .await;

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
async fn concurrent_checkout_same_idempotency_key_converges_to_one_order(db: PgPool) {
    // The empty-cart idempotency race (codex r2 gap): two requests carrying
    // the SAME idempotency key check out the SAME user's cart concurrently
    // (double-click / client retry). Idempotency is scoped per user_id, so
    // both lock attempts land on the same `users` row first — the
    // unconditional `lock_balance_tx` call, taken before the cart read since
    // Step 10b — so one blocks until the other commits (order created + cart
    // cleared + idempotency row inserted, all in that one transaction). Once
    // unblocked, the loser's re-read always finds an empty cart — before the
    // fix, `orders/service.rs:96-98` returned `400 cart is empty` right there,
    // never consulting idempotency. Stock is plentiful (this is not the
    // last-unit race); the only contested resource is the users row lock, so
    // both requests must resolve to the SAME order.
    let product = common::seed_product(&db, "prod-1", 1000, Some(5)).await;
    let user = seed_carted_member(
        &db,
        "concurrent-replay@example.com",
        &[SeedCartLine::Product { product_id: product, quantity: 1 }],
        0,
    )
    .await;

    let key = Some("concurrent-idempotency-key".to_string());
    let key_a = key.clone();
    let key_b = key;

    let db_a = Arc::new(db.clone());
    let db_b = Arc::new(db.clone());

    // `#[sqlx::test]` drives this test on a single-threaded Tokio runtime
    // (sqlx hard-codes `Builder::new_current_thread()` for its test harness),
    // and a fresh local test DB answers each query fast enough that a plain
    // `tokio::spawn` task's checkout resolves every internal `.await`
    // synchronously and runs to `commit` in one uninterrupted poll — the
    // second task never gets a turn until the first is entirely done, so
    // neither row lock ever actually contends (verified: 16/16 tokio::spawn
    // trials during development never hit the race). `spawn_blocking` +
    // `Handle::block_on` hands each checkout call to a genuine OS thread
    // against the same runtime's reactor, so both calls make real
    // independent progress and the users row lock becomes the actual
    // arbiter.
    let handle_a = tokio::runtime::Handle::current();
    let handle_b = handle_a.clone();

    let task_a = tokio::task::spawn_blocking(move || {
        let server = common::test_server_config();
        handle_a.block_on(service::checkout(
            db_a.as_ref(),
            user,
            key_a,
            CheckoutRequest::default(),
            None,
            &server,
            chrono::Utc::now(),
        ))
    });
    let task_b = tokio::task::spawn_blocking(move || {
        let server = common::test_server_config();
        handle_b.block_on(service::checkout(
            db_b.as_ref(),
            user,
            key_b,
            CheckoutRequest::default(),
            None,
            &server,
            chrono::Utc::now(),
        ))
    });

    let (res_a, res_b) = tokio::join!(task_a, task_b);
    let res_a = res_a.expect("task a panicked");
    let res_b = res_b.expect("task b panicked");

    let order_a = res_a.expect("task a should succeed (same key must replay, not 400)");
    let order_b = res_b.expect("task b should succeed (same key must replay, not 400)");

    assert_eq!(
        order_a.order_number, order_b.order_number,
        "both concurrent requests must converge on the same order"
    );

    let order_count = common::order_count(&db, user).await;
    assert_eq!(order_count, 1, "exactly one order must exist despite two concurrent checkouts");
}

#[sqlx::test]
async fn checkout_cart_read_locks_products_ascending_no_cross_buyer_deadlock(db: PgPool) {
    // Cross-buyer lock-order regression (codex branch-review P1): the cart
    // checkout read takes product `FOR SHARE` locks, while checkout's
    // `reserve_stock_tx` and refund's `restore_stock_tx` take per-row
    // `FOR UPDATE` locks in `product_id` ASCENDING order. The users-first
    // lock (Step 10b) only serializes SAME-buyer paths — for different
    // buyers, a cart read acquiring SHARE locks in cart-creation order can
    // interleave with another order's refund in the opposite order and
    // deadlock. `find_cart_items_for_checkout_tx` therefore pre-locks the
    // cart's products ascending, joining the same global product order.
    //
    // Adversarial construction (UUIDv7 is time-ordered, so seeding order ≈
    // ascending ids — same trap as the plan_refund input-order test): sort
    // the two ids explicitly and build the cart in DESCENDING id order, the
    // exact shape that deadlocked before the pre-lock.
    let a = common::seed_product(&db, "lock-order-a", 1000, Some(5)).await;
    let b = common::seed_product(&db, "lock-order-b", 1000, Some(5)).await;
    let (p_low, p_high) = if a < b { (a, b) } else { (b, a) };

    let user = seed_carted_member(
        &db,
        "cross-buyer-lock-order@example.com",
        &[
            SeedCartLine::Product { product_id: p_high, quantity: 1 },
            SeedCartLine::Product { product_id: p_low, quantity: 1 },
        ],
        0,
    )
    .await;

    // Refund-shaped locker: another buyer's refund holding its FIRST
    // ascending product lock (UPDATE on p_low), exactly like
    // `products::service::restore_stock_tx` mid-flight.
    let mut refund_tx = db.begin().await.unwrap();
    sqlx::query("SELECT id FROM products WHERE id = $1 FOR UPDATE")
        .bind(p_low)
        .execute(&mut *refund_tx)
        .await
        .unwrap();

    // Checkout-shaped reader on a real OS thread (same single-threaded
    // runtime rationale as the test above).
    let db_reader = Arc::new(db.clone());
    let handle = tokio::runtime::Handle::current();
    let reader = tokio::task::spawn_blocking(move || {
        handle.block_on(async move {
            let mut tx = db_reader.begin().await?;
            let lines = cart_repository::find_cart_items_for_checkout_tx(&mut tx, user).await?;
            tx.commit().await?;
            Ok::<usize, sqlx::Error>(lines.len())
        })
    });

    // Let the reader reach its first product lock. With the ascending
    // pre-lock it blocks on p_low while holding NO product lock; before the
    // fix it grabbed SHARE(p_high) first (cart-creation order) and then
    // blocked on p_low — holding exactly what the refund side needs next.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!reader.is_finished(), "reader must be blocked behind the held p_low lock");

    // The refund side takes its SECOND ascending lock. Post-fix the blocked
    // reader holds no product lock, so this returns immediately — no cycle.
    // Pre-fix the reader held SHARE(p_high), closing the cycle, and
    // PostgreSQL's deadlock detector aborted one side (SQLSTATE 40P01) —
    // this expect or the reader join below failed.
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        sqlx::query("SELECT id FROM products WHERE id = $1 FOR UPDATE")
            .bind(p_high)
            .execute(&mut *refund_tx),
    )
    .await
    .expect("refund-side second ascending lock must not block: the waiting cart read may hold no product lock")
    .expect("refund-side second ascending lock must not deadlock");

    refund_tx.commit().await.unwrap();

    let lines = reader
        .await
        .expect("reader task panicked")
        .expect("cart read must succeed once the refund-shaped locker commits");
    assert_eq!(lines, 2, "both cart lines survive the ordered locking");
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

// ---------------------------------------------------------------------
// Step 10e: refund/cancel compensation orchestration
// (`update_order_status` → `compensate_order_artifacts_tx`).
// ---------------------------------------------------------------------

/// Shared setup for the two full-compensation tests (rows 1-2): a mixed cart
/// — finite-stock product + course + membership — checked out with a points
/// balance, so a later refund/cancel has stock to restore, an enrolment AND a
/// subscription to cancel, and BOTH a `checkout_redeem` and a `checkout_earn`
/// ledger row to reverse. Artifacts must be built by a REAL checkout (the
/// `seed_*` fixtures never write `order_id`, so a directly-built order carries
/// no traces to compensate). Returns `(user, order, finite_product_id)`.
async fn checkout_mixed_with_points(db: &PgPool, email: &str) -> (Uuid, OrderResponse, Uuid) {
    let course = seed_course_with_capacity(db, "Compensation Course", None, 12).await;
    let limited = common::seed_product(db, "comp-limited", 10_000, Some(5)).await;
    let membership =
        seed_entitlement_product(db, "comp-membership", "membership", 8_000, None, None).await;
    let user = seed_carted_member(
        db,
        email,
        &[
            SeedCartLine::Product { product_id: limited, quantity: 1 },
            SeedCartLine::Course { course_id: course },
            SeedCartLine::Product { product_id: membership, quantity: 1 },
        ],
        500,
    )
    .await;

    let req = CheckoutRequest {
        coupon_code: None,
        use_points: Some(true),
        payment_method: None,
    };
    let order = service::checkout(
        db,
        user,
        None,
        req,
        None,
        &common::test_server_config(),
        chrono::Utc::now(),
    )
    .await
    .expect("checkout");

    // The scenario must actually move points BOTH ways, else the
    // restore/clawback assertions below would be vacuous.
    assert!(order.points_used > 0, "scenario must redeem points");
    assert!(order.points_earned > 0, "scenario must earn points");

    (user, order, limited)
}

/// The full-compensation assertion set shared by refund (row 1) and cancel
/// (row 2) — the brief pins them to be identical. `outcome` is the response
/// the terminal transition returned (its artifacts are re-read fresh, so they
/// reflect the just-applied cancellation).
async fn assert_fully_compensated(
    db: &PgPool,
    user: Uuid,
    order: &OrderResponse,
    outcome: &OrderResponse,
    finite_product: Uuid,
) {
    // Stock restored to its pre-checkout value (5).
    assert_eq!(
        common::product_stock(db, finite_product).await,
        Some(5),
        "finite stock restored"
    );

    // Enrolment + subscription cancelled (the response re-reads the latest
    // artifact rows).
    assert_eq!(outcome.enrolments.len(), 1);
    assert_eq!(outcome.enrolments[0].status, "cancelled", "enrolment cancelled");
    assert_eq!(outcome.subscriptions.len(), 1);
    assert_eq!(
        outcome.subscriptions[0].status, "cancelled",
        "subscription cancelled"
    );

    // paid_at is retained (§1.8) — a refund/cancel doesn't erase the payment
    // timestamp.
    assert!(outcome.paid_at.is_some(), "paid_at retained");

    // Balance fully restored to the original 500 (redeemed points returned,
    // earned points clawed back).
    assert_eq!(
        common::points_balance_of(db, user).await,
        500,
        "balance fully restored"
    );

    // Exactly one refund_restore (+redeemed) and one refund_clawback
    // (-earned), both carrying the correct order_id.
    let restore: (i64, Option<Uuid>) = sqlx::query_as(
        "SELECT delta, order_id FROM point_ledger \
         WHERE order_id = $1 AND reason = 'refund_restore'::point_reason",
    )
    .bind(order.id)
    .fetch_one(db)
    .await
    .expect("one refund_restore row");
    assert_eq!(restore.0, order.points_used, "restore reverses the redeemed magnitude");
    assert_eq!(restore.1, Some(order.id));

    let clawback: (i64, Option<Uuid>) = sqlx::query_as(
        "SELECT delta, order_id FROM point_ledger \
         WHERE order_id = $1 AND reason = 'refund_clawback'::point_reason",
    )
    .bind(order.id)
    .fetch_one(db)
    .await
    .expect("one refund_clawback row");
    assert_eq!(clawback.0, -order.points_earned, "clawback reverses the earned magnitude");
    assert_eq!(clawback.1, Some(order.id));

    let refund_ledger_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM point_ledger \
         WHERE order_id = $1 AND reason IN ('refund_restore'::point_reason, 'refund_clawback'::point_reason)",
    )
    .bind(order.id)
    .fetch_one(db)
    .await
    .unwrap();
    assert_eq!(refund_ledger_count, 2, "exactly one restore + one clawback");
}

/// Row 1: paid→refunded reverses stock, enrolment, subscription, and points;
/// exactly one restore + one clawback ledger row; paid_at retained; a
/// refunded notification is written.
#[sqlx::test]
async fn refund_reverses_stock_enrolment_subscription_and_points(db: PgPool) {
    let (user, order, limited) = checkout_mixed_with_points(&db, "refund-buyer@example.com").await;

    let refunded = service::update_order_status(&db, order.id, "refunded", None)
        .await
        .expect("refund");
    assert_eq!(refunded.status, "refunded");

    assert_fully_compensated(&db, user, &order, &refunded, limited).await;

    let (_title, message) = common::latest_notification(&db, user, "order_status")
        .await
        .expect("refund notification");
    assert!(message.contains("refunded"), "message={message}");
}

/// Row 2: target=cancelled compensates identically to refunded (裁決: same
/// semantics).
#[sqlx::test]
async fn cancel_compensates_identically_to_refund(db: PgPool) {
    let (user, order, limited) = checkout_mixed_with_points(&db, "cancel-buyer@example.com").await;

    let cancelled = service::update_order_status(&db, order.id, "cancelled", None)
        .await
        .expect("cancel");
    assert_eq!(cancelled.status, "cancelled");

    assert_fully_compensated(&db, user, &order, &cancelled, limited).await;

    let (_title, message) = common::latest_notification(&db, user, "order_status")
        .await
        .expect("cancel notification");
    assert!(message.contains("cancelled"), "message={message}");
}

/// Row 3: an earned-points order whose balance is later wiped can't satisfy
/// the clawback — the `users_points_balance_check` violation surfaces as
/// `Conflict("點數不足")` and rolls the WHOLE compensation back (status,
/// stock, enrolment, subscription untouched; no refund ledger row).
#[sqlx::test]
async fn refund_clawback_insufficient_balance_conflicts_and_rolls_back_all(db: PgPool) {
    let course = seed_course_with_capacity(&db, "Clawback Course", None, 12).await;
    let limited = common::seed_product(&db, "clawback-limited", 10_000, Some(5)).await;
    let membership =
        seed_entitlement_product(&db, "clawback-membership", "membership", 8_000, None, None).await;
    // No points balance → no redeem, only an earn ledger row; restore will be
    // 0, so it can't cover the clawback.
    let user = seed_carted_member(
        &db,
        "clawback-buyer@example.com",
        &[
            SeedCartLine::Product { product_id: limited, quantity: 1 },
            SeedCartLine::Course { course_id: course },
            SeedCartLine::Product { product_id: membership, quantity: 1 },
        ],
        0,
    )
    .await;
    let order = service::checkout(
        &db,
        user,
        None,
        CheckoutRequest::default(),
        None,
        &common::test_server_config(),
        chrono::Utc::now(),
    )
    .await
    .expect("checkout");
    assert!(order.points_earned > 0, "must earn points to have a clawback");

    // Member spent the earned points elsewhere — wipe the balance to 0.
    set_points_balance(&db, user, 0).await;

    let err = service::update_order_status(&db, order.id, "refunded", None)
        .await
        .expect_err("clawback against a zero balance must conflict");
    match err {
        AppError::Conflict(msg) => assert_eq!(msg, "點數不足"),
        other => panic!("expected Conflict(點數不足), got {other:?}"),
    }

    // WHOLE compensation rolled back.
    let status: String = sqlx::query_scalar("SELECT status::text FROM orders WHERE id = $1")
        .bind(order.id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(status, "paid", "status untouched by the rolled-back refund");
    // Stock stays at its post-checkout value (5 - 1 = 4).
    assert_eq!(
        common::product_stock(&db, limited).await,
        Some(4),
        "stock untouched (still post-checkout value)"
    );
    let active_enrolments: i64 =
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM enrolments WHERE order_id = $1 AND status = 'active'::enrolment_status",
        )
            .bind(order.id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(active_enrolments, 1, "enrolment stays active");
    let active_subs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM subscriptions WHERE order_id = $1 AND status = 'active'::subscription_status",
    )
    .bind(order.id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(active_subs, 1, "subscription stays active");
    let refund_ledger: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM point_ledger \
         WHERE order_id = $1 AND reason IN ('refund_restore'::point_reason, 'refund_clawback'::point_reason)",
    )
    .bind(order.id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(refund_ledger, 0, "no refund ledger row survives the rollback");
}

/// Row 4: re-PATCHing the same terminal status is an observable idempotent
/// no-op — the ledger still has exactly the two rows from the first pass, and
/// NO new outbox row / notification is written.
#[sqlx::test]
async fn refunded_same_status_noop_does_not_compensate_twice(db: PgPool) {
    let (user, order, _limited) = checkout_mixed_with_points(&db, "noop-buyer@example.com").await;

    // First refund compensates.
    service::update_order_status(&db, order.id, "refunded", None)
        .await
        .expect("first refund");

    let outbox_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events_outbox")
        .fetch_one(&db)
        .await
        .unwrap();
    let notif_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND type = 'order_status'::notification_type",
    )
    .bind(user)
    .fetch_one(&db)
    .await
    .unwrap();

    // Second PATCH refunded → same-status no-op: returns Ok, writes nothing.
    let again = service::update_order_status(&db, order.id, "refunded", None)
        .await
        .expect("same-status noop is Ok");
    assert_eq!(again.status, "refunded");

    let refund_ledger: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM point_ledger \
         WHERE order_id = $1 AND reason IN ('refund_restore'::point_reason, 'refund_clawback'::point_reason)",
    )
    .bind(order.id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(refund_ledger, 2, "no second compensation");

    let outbox_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events_outbox")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(outbox_after, outbox_before, "no new outbox row");

    let notif_after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND type = 'order_status'::notification_type",
    )
    .bind(user)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(notif_after, notif_before, "no new notification");
}

/// Row 5: terminal states have no outgoing edges — refunded→processing and
/// cancelled→refunded both 400 (before any compensation is considered).
#[sqlx::test]
async fn refunded_terminal_rejects_further_transitions(db: PgPool) {
    // Refund, then try to move on → 400.
    let product_a = common::seed_product(&db, "terminal-a", 1_000, Some(5)).await;
    let user_a = seed_carted_member(
        &db,
        "terminal-a@example.com",
        &[SeedCartLine::Product { product_id: product_a, quantity: 1 }],
        0,
    )
    .await;
    let order_a = service::checkout(
        &db,
        user_a,
        None,
        CheckoutRequest::default(),
        None,
        &common::test_server_config(),
        chrono::Utc::now(),
    )
    .await
    .expect("checkout a");
    service::update_order_status(&db, order_a.id, "refunded", None)
        .await
        .expect("refund a");
    let err = service::update_order_status(&db, order_a.id, "processing", None)
        .await
        .expect_err("refunded is terminal");
    assert!(matches!(err, AppError::BadRequest(_)), "got: {err:?}");

    // Cancel a different order, then cancelled→refunded → 400.
    let product_b = common::seed_product(&db, "terminal-b", 1_000, Some(5)).await;
    let user_b = seed_carted_member(
        &db,
        "terminal-b@example.com",
        &[SeedCartLine::Product { product_id: product_b, quantity: 1 }],
        0,
    )
    .await;
    let order_b = service::checkout(
        &db,
        user_b,
        None,
        CheckoutRequest::default(),
        None,
        &common::test_server_config(),
        chrono::Utc::now(),
    )
    .await
    .expect("checkout b");
    service::update_order_status(&db, order_b.id, "cancelled", None)
        .await
        .expect("cancel b");
    let err_b = service::update_order_status(&db, order_b.id, "refunded", None)
        .await
        .expect_err("cancelled is terminal");
    assert!(matches!(err_b, AppError::BadRequest(_)), "got: {err_b:?}");
}

/// Row 6: pending→cancelled never compensates — Pending isn't a revenue
/// status, so cancelling a directly-built pending order is a pure status
/// flip (stock untouched, no ledger row).
#[sqlx::test]
async fn pending_to_cancelled_does_not_compensate(db: PgPool) {
    let user = common::seed_member(&db, "pending-buyer@example.com", "Password!234").await;
    let product = common::seed_product(&db, "pending-prod", 1_000, Some(5)).await;
    let order_id =
        seed_order_with_item(&db, user, product, "Pending Prod", 1, 1_000, "pending").await;

    let resp = service::update_order_status(&db, order_id, "cancelled", None)
        .await
        .expect("cancel pending");
    assert_eq!(resp.status, "cancelled");

    assert_eq!(
        common::product_stock(&db, product).await,
        Some(5),
        "stock untouched (pending never decremented it)"
    );
    let ledger: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM point_ledger WHERE order_id = $1")
        .bind(order_id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(ledger, 0, "no ledger row for a pending cancel");
}

/// Row 7: an unlimited-stock (NULL) product line records
/// `stock_decremented=false`, so refunding it restores nothing — the column
/// stays NULL.
#[sqlx::test]
async fn refund_keeps_unlimited_stock_null(db: PgPool) {
    let product = common::seed_product(&db, "unlimited-refund", 500, None).await;
    let user = seed_carted_member(
        &db,
        "unlimited-refund@example.com",
        &[SeedCartLine::Product { product_id: product, quantity: 3 }],
        0,
    )
    .await;
    let order = service::checkout(
        &db,
        user,
        None,
        CheckoutRequest::default(),
        None,
        &common::test_server_config(),
        chrono::Utc::now(),
    )
    .await
    .expect("checkout");

    service::update_order_status(&db, order.id, "refunded", None)
        .await
        .expect("refund");
    assert_eq!(
        common::product_stock(&db, product).await,
        None,
        "unlimited stock stays NULL"
    );
}

/// Row 8: sold while unlimited (snapshot false), then an admin switches the
/// product to finite stock. Refund honors the checkout-time snapshot, not the
/// current stock mode — no restock, stock stays 5.
#[sqlx::test]
async fn refund_skips_restock_when_sold_unlimited_then_stock_set(db: PgPool) {
    let product = common::seed_product(&db, "snapshot-refund", 500, None).await;
    let user = seed_carted_member(
        &db,
        "snapshot-refund@example.com",
        &[SeedCartLine::Product { product_id: product, quantity: 2 }],
        0,
    )
    .await;
    let order = service::checkout(
        &db,
        user,
        None,
        CheckoutRequest::default(),
        None,
        &common::test_server_config(),
        chrono::Utc::now(),
    )
    .await
    .expect("checkout");

    // Admin flips the product to finite stock AFTER the sale.
    sqlx::query("UPDATE products SET stock = 5 WHERE id = $1")
        .bind(product)
        .execute(&db)
        .await
        .unwrap();

    service::update_order_status(&db, order.id, "refunded", None)
        .await
        .expect("refund");
    assert_eq!(
        common::product_stock(&db, product).await,
        Some(5),
        "snapshot false ⇒ no restock; stock stays 5"
    );
}

/// Row 9: a directly-built PAID order carries none of the three checkout
/// traces (no stock_decremented, no ledger, no artifacts). Refunding it is a
/// pure status flip — the legacy-data policy: missing traces ⇒ natural no-op.
#[sqlx::test]
async fn refund_of_directly_built_paid_order_is_pure_status_flip(db: PgPool) {
    let user = common::seed_member(&db, "direct-paid@example.com", "Password!234").await;
    set_points_balance(&db, user, 100).await;
    let product = common::seed_product(&db, "direct-paid-prod", 1_000, Some(5)).await;
    let order_id =
        seed_order_with_item(&db, user, product, "Direct Paid", 2, 1_000, "paid").await;

    let resp = service::update_order_status(&db, order_id, "refunded", None)
        .await
        .expect("refund directly-built paid order");
    assert_eq!(resp.status, "refunded");

    assert_eq!(
        common::points_balance_of(&db, user).await,
        100,
        "balance untouched (no ledger trace)"
    );
    assert_eq!(
        common::product_stock(&db, product).await,
        Some(5),
        "stock untouched (no stock_decremented trace)"
    );
    let refund_ledger: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM point_ledger WHERE order_id = $1")
            .bind(order_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(refund_ledger, 0, "no refund ledger row for a zero-trace order");
}

/// Row 10: member self-cancels the enrolment first, then the whole order is
/// refunded. The order-scoped cancel is naturally idempotent (the already-
/// cancelled enrolment is a 0-row no-op), and the points are still reversed
/// in FULL — whole-order semantics don't depend on the enrolment's state.
#[sqlx::test]
async fn refund_after_member_self_cancel_still_succeeds(db: PgPool) {
    let course = seed_course_with_capacity(&db, "Self-Cancel Course", None, 12).await;
    let limited = common::seed_product(&db, "self-cancel-limited", 10_000, Some(5)).await;
    let user = seed_carted_member(
        &db,
        "self-cancel-buyer@example.com",
        &[
            SeedCartLine::Course { course_id: course },
            SeedCartLine::Product { product_id: limited, quantity: 1 },
        ],
        500,
    )
    .await;
    let req = CheckoutRequest {
        coupon_code: None,
        use_points: Some(true),
        payment_method: None,
    };
    let order = service::checkout(
        &db,
        user,
        None,
        req,
        None,
        &common::test_server_config(),
        chrono::Utc::now(),
    )
    .await
    .expect("checkout");
    assert!(
        order.points_used > 0 && order.points_earned > 0,
        "scenario must move points both ways"
    );
    assert_eq!(order.enrolments.len(), 1);

    // Member self-cancels the enrolment before the refund.
    let auth = common::member_auth(user);
    enrolments_service::cancel_enrolment(&db, &auth, order.enrolments[0].id)
        .await
        .expect("self-cancel enrolment");

    // Now refund the whole order — must still succeed.
    let refunded = service::update_order_status(&db, order.id, "refunded", None)
        .await
        .expect("refund after self-cancel");
    assert_eq!(refunded.status, "refunded");
    assert_eq!(
        refunded.enrolments[0].status, "cancelled",
        "enrolment stays cancelled"
    );

    // Points reversed in full: balance back to the original 500.
    assert_eq!(
        common::points_balance_of(&db, user).await,
        500,
        "full points reversal despite prior self-cancel"
    );
    let refund_ledger: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM point_ledger \
         WHERE order_id = $1 AND reason IN ('refund_restore'::point_reason, 'refund_clawback'::point_reason)",
    )
    .bind(order.id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(refund_ledger, 2, "both directions reversed");
}

// ---------------------------------------------------------------------
// W4 (C6): `TxReleased` witness — release/commit the tx BEFORE
// `assemble_response` re-acquires from the pool.
// ---------------------------------------------------------------------

/// The `TxReleased` witness exists so `assemble_response` cannot re-acquire a
/// pooled connection while the checkout/status transaction still holds one — a
/// self-deadlock that only bites when connections are scarce. This pins the
/// discipline on a pool with **exactly one** connection and a short acquire
/// timeout: every order path must release or commit its tx before assembling
/// the response, so each `assemble_response` re-acquire finds the lone
/// connection free. A regression that assembled while still holding the tx
/// would block on the empty pool and fail fast with `PoolTimedOut` (~2s)
/// rather than hang the suite.
///
/// Three of the witness kinds are exercised directly: `commit` (checkout
/// happy path + status transition), `release` (same-status no-op), and
/// `no_open_tx` (the idempotency pre-check on same-key replay). Honest
/// declaration: the other two `release` sites (empty-cart replay,
/// unique-violation replay) are concurrency-race branches — guarded by the
/// existing concurrent tests plus the witness type itself, not re-exercised
/// here.
///
/// The hand-built pool is NOT owned by `#[sqlx::test]`, so it MUST be closed
/// explicitly: dropping a pool doesn't guarantee the server-side connection is
/// gone, and a lingering connection would block the throwaway database's
/// teardown (sqlx-core 0.9 pool docs).
#[sqlx::test]
async fn order_paths_complete_on_a_single_connection_pool(db: PgPool) {
    // Seed through the harness-provided pool (many connections) so setup never
    // contends with the single-connection pool under test.
    let user = common::seed_member(&db, "single-conn-buyer@example.com", "passw0rd!").await;
    let product = common::seed_product(&db, "single-conn-prod", 1500, Some(5)).await;
    common::add_to_cart(&db, user, product, 2).await;

    // A one-connection pool onto the SAME throwaway test database, with a short
    // acquire timeout so a self-deadlock surfaces as PoolTimedOut in ~2s
    // instead of hanging. (sqlx 0.9: derive the connect options from the
    // harness pool and rebuild with max_connections(1).)
    let connect_opts = db.connect_options().as_ref().clone();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect_with(connect_opts)
        .await
        .expect("build single-connection pool on the test database");

    let server = common::test_server_config();
    let key = Some("single-conn-key".to_string());

    // 1. Checkout happy path: commit → assemble. The tx commits (freeing the
    //    lone connection) before `assemble_response` re-reads items/artifacts.
    let first = service::checkout(
        &pool,
        user,
        key.clone(),
        CheckoutRequest::default(),
        None,
        &server,
        chrono::Utc::now(),
    )
    .await
    .expect("checkout must complete on a 1-connection pool (commit→assemble)");
    assert_eq!(first.status, "paid");
    assert_eq!(first.items.len(), 1);

    // 2. Same-status no-op: release → assemble. PATCH paid→paid drops the tx
    //    early, then assembles; on a 1-conn pool this only works if the tx was
    //    released first.
    let noop = service::update_order_status(&pool, first.id, "paid", None)
        .await
        .expect("same-status no-op must complete on a 1-connection pool (release→assemble)");
    assert_eq!(noop.status, "paid");
    assert_eq!(noop.order_number, first.order_number);

    // 3. Status transition: commit → assemble. paid→processing compensates
    //    nothing, updates, commits, then assembles.
    let processing = service::update_order_status(&pool, first.id, "processing", None)
        .await
        .expect("status transition must complete on a 1-connection pool (commit→assemble)");
    assert_eq!(processing.status, "processing");

    // 4. Same-key replay: the idempotency pre-check returns the prior order
    //    without ever opening a tx (`no_open_tx`) — assemble runs with the lone
    //    connection free.
    let replay = service::checkout(
        &pool,
        user,
        key,
        CheckoutRequest::default(),
        None,
        &server,
        chrono::Utc::now(),
    )
    .await
    .expect("same-key replay must complete on a 1-connection pool (no_open_tx pre-check)");
    assert_eq!(
        replay.order_number, first.order_number,
        "replay must return the original order, not a duplicate"
    );

    // Exactly one order despite checkout + replay.
    let order_count = common::order_count(&db, user).await;
    assert_eq!(order_count, 1, "replay must not create a second order");

    // The hand-built pool is not managed by `#[sqlx::test]`; close it so no
    // server-side connection lingers to block the test database teardown.
    pool.close().await;
}
