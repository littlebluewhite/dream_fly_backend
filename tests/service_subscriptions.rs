//! Integration tests for `subscriptions::service`.
//!
//! Covered paths:
//! - `grant_from_purchase_tx`: the three entitlement rules (session-count,
//!   time-based, pure membership), the session+valid_days combo, a
//!   non-entitlement product returning `None`, and the time-based
//!   quantity-must-be-1 validation error.
//! - `redeem`: successful decrement, zero-remaining conflict, expired-by-date
//!   conflict, no-session-quota conflict (exact message), cancelled conflict,
//!   and not-found.
//! - `subscription_derived_status` SQL function (C7): table-driven, one row
//!   per (status, expires_at, remaining_sessions) shape, INSERTed via the
//!   `seed_subscription` fixture and read back through
//!   `repository::find_by_id`'s `derived_status` column — active/expired/
//!   cancelled, the zero-sessions-vs-future-expiry priority case, and
//!   cancelled overriding both an active- and an expired-looking row.

mod common;

use chrono::{Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use common::fixtures::{SeedCartLine, seed_carted_member, seed_entitlement_product, seed_subscription};
use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::orders::dto::CheckoutRequest;
use dream_fly_backend::modules::orders::repository as orders_repo;
use dream_fly_backend::modules::orders::service as orders_service;
use dream_fly_backend::modules::products::repository as products_repo;
use dream_fly_backend::modules::subscriptions::model::SubscriptionStatus;
use dream_fly_backend::modules::subscriptions::repository as subscriptions_repo;
use dream_fly_backend::modules::subscriptions::service;

/// `subscriptions.order_id` is a real FK into `orders`, so grant tests need
/// an actual order row committed in the same transaction rather than a bare
/// random UUID.
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
        "credit_card",
    )
    .await
    .expect("seed order")
    .id
}

// ---------------------------------------------------------------------------
// grant_from_purchase_tx
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn grant_session_count_multiplies_by_quantity(db: PgPool) {
    let user_id = common::seed_member(&db, "grant-a@example.com", "Password!234").await;
    let product_id =
        seed_entitlement_product(&db, "ticket-10", "ticket", 5_000, None, Some(10)).await;
    let product = products_repo::find_by_id(&db, product_id)
        .await
        .expect("query product")
        .expect("product exists");
    let mut tx = db.begin().await.expect("begin tx");
    let order_id = seed_order(&mut tx, user_id, 5_000).await;
    let result = service::grant_from_purchase_tx(&mut tx, user_id, &product, 3, 5_000, order_id)
        .await
        .expect("grant");
    tx.commit().await.expect("commit");

    let sub = result.expect("expected Some(subscription)");
    assert_eq!(sub.total_sessions, Some(30));
    assert_eq!(sub.remaining_sessions, Some(30));
    assert!(sub.expires_at.is_none());
    assert_eq!(sub.price_cents, 5_000);
    assert_eq!(sub.user_id, user_id);
    assert_eq!(sub.product_id, product_id);
    assert_eq!(sub.order_id, Some(order_id));
}

#[sqlx::test]
async fn grant_session_count_with_valid_days_also_sets_expiry(db: PgPool) {
    let user_id = common::seed_member(&db, "grant-b@example.com", "Password!234").await;
    let product_id =
        seed_entitlement_product(&db, "ticket-combo", "ticket", 8_000, Some(90), Some(5)).await;
    let product = products_repo::find_by_id(&db, product_id)
        .await
        .expect("query product")
        .expect("product exists");
    let before = Utc::now();
    let mut tx = db.begin().await.expect("begin tx");
    let order_id = seed_order(&mut tx, user_id, 8_000).await;
    let result = service::grant_from_purchase_tx(&mut tx, user_id, &product, 2, 8_000, order_id)
        .await
        .expect("grant");
    tx.commit().await.expect("commit");

    let sub = result.expect("expected Some(subscription)");
    // Both constraints apply: sessions still drive the quota...
    assert_eq!(sub.total_sessions, Some(10));
    assert_eq!(sub.remaining_sessions, Some(10));
    // ...and expires_at is populated too, since valid_days was also set.
    let expires_at = sub
        .expires_at
        .expect("expires_at should be set when valid_days is also present");
    assert!(expires_at > before + Duration::days(89));
    assert!(expires_at < before + Duration::days(91));
}

#[sqlx::test]
async fn grant_valid_days_only_sets_expiry_and_no_sessions(db: PgPool) {
    let user_id = common::seed_member(&db, "grant-c@example.com", "Password!234").await;
    let product_id =
        seed_entitlement_product(&db, "membership-30d", "membership", 3_000, Some(30), None).await;
    let product = products_repo::find_by_id(&db, product_id)
        .await
        .expect("query product")
        .expect("product exists");
    let before = Utc::now();
    let mut tx = db.begin().await.expect("begin tx");
    let order_id = seed_order(&mut tx, user_id, 3_000).await;
    let result = service::grant_from_purchase_tx(&mut tx, user_id, &product, 1, 3_000, order_id)
        .await
        .expect("grant");
    tx.commit().await.expect("commit");

    let sub = result.expect("expected Some(subscription)");
    assert!(sub.total_sessions.is_none());
    assert!(sub.remaining_sessions.is_none());
    let expires_at = sub.expires_at.expect("expires_at should be set");
    assert!(expires_at > before + Duration::days(29));
    assert!(expires_at < before + Duration::days(31));
}

#[sqlx::test]
async fn grant_no_entitlement_fields_creates_unlimited_membership(db: PgPool) {
    let user_id = common::seed_member(&db, "grant-d@example.com", "Password!234").await;
    let product_id = seed_entitlement_product(
        &db,
        "membership-unlimited",
        "membership",
        20_000,
        None,
        None,
    )
    .await;
    let product = products_repo::find_by_id(&db, product_id)
        .await
        .expect("query product")
        .expect("product exists");
    let mut tx = db.begin().await.expect("begin tx");
    let order_id = seed_order(&mut tx, user_id, 20_000).await;
    let result = service::grant_from_purchase_tx(&mut tx, user_id, &product, 1, 20_000, order_id)
        .await
        .expect("grant");
    tx.commit().await.expect("commit");

    let sub = result.expect("expected Some(subscription)");
    assert!(sub.total_sessions.is_none());
    assert!(sub.remaining_sessions.is_none());
    assert!(sub.expires_at.is_none());
}

#[sqlx::test]
async fn grant_non_entitlement_product_type_returns_none(db: PgPool) {
    let user_id = common::seed_member(&db, "grant-e@example.com", "Password!234").await;
    // `seed_product` (tests/common/mod.rs) hardcodes product_type = 'merchandise'.
    let product_id = common::seed_product(&db, "tshirt-grant", 1_500, Some(50)).await;
    let product = products_repo::find_by_id(&db, product_id)
        .await
        .expect("query product")
        .expect("product exists");
    let mut tx = db.begin().await.expect("begin tx");
    let order_id = seed_order(&mut tx, user_id, 1_500).await;
    let result = service::grant_from_purchase_tx(&mut tx, user_id, &product, 1, 1_500, order_id)
        .await
        .expect("grant should not error for a non-entitlement product");
    tx.rollback().await.expect("rollback");

    assert!(result.is_none());
}

#[sqlx::test]
async fn grant_time_based_with_quantity_other_than_one_is_validation_error(db: PgPool) {
    let user_id = common::seed_member(&db, "grant-f@example.com", "Password!234").await;
    let product_id =
        seed_entitlement_product(&db, "membership-90d", "membership", 6_000, Some(90), None).await;
    let product = products_repo::find_by_id(&db, product_id)
        .await
        .expect("query product")
        .expect("product exists");
    let mut tx = db.begin().await.expect("begin tx");
    let order_id = seed_order(&mut tx, user_id, 6_000).await;
    let err = service::grant_from_purchase_tx(&mut tx, user_id, &product, 2, 6_000, order_id)
        .await
        .expect_err("quantity=2 for a time-based product must fail");
    tx.rollback().await.expect("rollback");

    assert!(
        matches!(err, AppError::Validation(_)),
        "expected Validation, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// redeem
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn redeem_decrements_remaining_sessions(db: PgPool) {
    let user_id = common::seed_member(&db, "redeem-a@example.com", "Password!234").await;
    let product_id =
        seed_entitlement_product(&db, "ticket-redeem-a", "ticket", 5_000, None, Some(10)).await;
    let sub_id = seed_subscription(
        &db, user_id, product_id, "active", None, Some(3), Some(3), 5_000, Utc::now(),
    )
    .await;

    let resp = service::redeem(&db, sub_id).await.expect("redeem");
    assert_eq!(resp.remaining_sessions, Some(2));
    assert_eq!(resp.total_sessions, Some(3));
    assert_eq!(resp.status, "active");
    assert_eq!(resp.product_id, product_id);
}

#[sqlx::test]
async fn redeem_with_zero_remaining_returns_conflict(db: PgPool) {
    let user_id = common::seed_member(&db, "redeem-b@example.com", "Password!234").await;
    let product_id =
        seed_entitlement_product(&db, "ticket-redeem-b", "ticket", 5_000, None, Some(10)).await;
    let sub_id = seed_subscription(
        &db, user_id, product_id, "active", None, Some(3), Some(0), 5_000, Utc::now(),
    )
    .await;

    let err = service::redeem(&db, sub_id)
        .await
        .expect_err("zero remaining sessions must not redeem");
    assert!(matches!(err, AppError::Conflict(_)), "got {err:?}");
}

#[sqlx::test]
async fn redeem_expired_by_date_returns_conflict(db: PgPool) {
    let user_id = common::seed_member(&db, "redeem-c@example.com", "Password!234").await;
    let product_id =
        seed_entitlement_product(&db, "ticket-redeem-c", "ticket", 5_000, None, Some(10)).await;
    let sub_id = seed_subscription(
        &db,
        user_id,
        product_id,
        "active",
        Some(Utc::now() - Duration::days(1)),
        Some(3),
        Some(3),
        5_000,
        Utc::now(),
    )
    .await;

    let err = service::redeem(&db, sub_id)
        .await
        .expect_err("expired-by-date subscription must not redeem");
    assert!(matches!(err, AppError::Conflict(_)), "got {err:?}");
}

#[sqlx::test]
async fn redeem_with_no_session_quota_returns_specific_conflict_message(db: PgPool) {
    let user_id = common::seed_member(&db, "redeem-d@example.com", "Password!234").await;
    let product_id = seed_entitlement_product(
        &db,
        "membership-redeem-d",
        "membership",
        20_000,
        None,
        None,
    )
    .await;
    let sub_id = seed_subscription(
        &db, user_id, product_id, "active", None, None, None, 20_000, Utc::now(),
    )
    .await;

    let err = service::redeem(&db, sub_id)
        .await
        .expect_err("a subscription with no session quota must not redeem");
    match err {
        AppError::Conflict(msg) => assert_eq!(msg, "subscription has no session quota"),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[sqlx::test]
async fn redeem_cancelled_returns_conflict(db: PgPool) {
    let user_id = common::seed_member(&db, "redeem-e@example.com", "Password!234").await;
    let product_id =
        seed_entitlement_product(&db, "ticket-redeem-e", "ticket", 5_000, None, Some(10)).await;
    let sub_id = seed_subscription(
        &db, user_id, product_id, "cancelled", None, Some(3), Some(3), 5_000, Utc::now(),
    )
    .await;

    let err = service::redeem(&db, sub_id)
        .await
        .expect_err("a cancelled subscription must not redeem");
    assert!(matches!(err, AppError::Conflict(_)), "got {err:?}");
}

#[sqlx::test]
async fn redeem_nonexistent_id_returns_not_found(db: PgPool) {
    let err = service::redeem(&db, Uuid::now_v7())
        .await
        .expect_err("a nonexistent subscription id must 404");
    assert!(matches!(err, AppError::NotFound(_)), "got {err:?}");
}

#[sqlx::test]
async fn concurrent_redeems_each_report_their_own_decrement(db: PgPool) {
    // Five concurrent redeems on remaining=5. The atomic UPDATE serializes
    // them on the row lock, so the five RETURNING rows carry the distinct
    // values 4,3,2,1,0 — and each call's RESPONSE must carry the value its
    // own UPDATE produced. If the service re-read the subscription between
    // its UPDATE and response assembly, a sibling call's later decrement
    // could leak into the response (duplicate/missing values here).
    let user_id = common::seed_member(&db, "redeem-f@example.com", "Password!234").await;
    let product_id =
        seed_entitlement_product(&db, "ticket-redeem-f", "ticket", 5_000, None, Some(5)).await;
    let sub_id = seed_subscription(
        &db, user_id, product_id, "active", None, Some(5), Some(5), 5_000, Utc::now(),
    )
    .await;

    let mut handles = Vec::new();
    for _ in 0..5 {
        let pool = db.clone();
        handles.push(tokio::spawn(
            async move { service::redeem(&pool, sub_id).await },
        ));
    }

    let mut reported: Vec<i32> = Vec::new();
    for handle in handles {
        let resp = handle
            .await
            .expect("join")
            .expect("all 5 redeems fit within the quota and must succeed");
        reported.push(resp.remaining_sessions.expect("session-based subscription"));
    }
    reported.sort_unstable();
    assert_eq!(
        reported,
        vec![0, 1, 2, 3, 4],
        "each response must reflect its own call's decrement, not a sibling's later state"
    );

    let final_remaining: i32 =
        sqlx::query_scalar("SELECT remaining_sessions FROM subscriptions WHERE id = $1")
            .bind(sub_id)
            .fetch_one(&db)
            .await
            .expect("fetch remaining");
    assert_eq!(final_remaining, 0);
}

// ---------------------------------------------------------------------------
// subscription_derived_status SQL function — table-driven, one seeded row
// per (status, expires_at, remaining_sessions) shape, read back through
// `repository::find_by_id`'s `derived_status` column. This exercises the
// real DB function (migration `20260708000007_subscription_derived_status.sql`)
// rather than a Rust re-implementation of it — there is no Rust twin left to
// compare against. Per R13, `expires_at` is always clearly past or clearly
// future (never "now"), avoiding a flaky boundary pinned at the instant the
// test runs vs. the instant SQL's `NOW()` samples the clock.
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn derived_status_matches_expected_shape(db: PgPool) {
    struct Case {
        name: &'static str,
        status: &'static str,
        expires_at: Option<chrono::DateTime<Utc>>,
        remaining_sessions: Option<i32>,
        expected: SubscriptionStatus,
    }

    let now = Utc::now();
    let past = now - Duration::days(1);
    let future = now + Duration::days(30);

    let cases = [
        Case {
            name: "active unlimited: expires_at NULL, remaining_sessions NULL",
            status: "active",
            expires_at: None,
            remaining_sessions: None,
            expected: SubscriptionStatus::Active,
        },
        Case {
            name: "active: unexpired validity with sessions remaining",
            status: "active",
            expires_at: Some(future),
            remaining_sessions: Some(5),
            expected: SubscriptionStatus::Active,
        },
        Case {
            name: "expired: past expires_at",
            status: "active",
            expires_at: Some(past),
            remaining_sessions: Some(5),
            expected: SubscriptionStatus::Expired,
        },
        Case {
            name: "expired: remaining_sessions hit zero",
            status: "active",
            expires_at: None,
            remaining_sessions: Some(0),
            expected: SubscriptionStatus::Expired,
        },
        Case {
            name: "expired: zero remaining_sessions takes priority over future expires_at",
            status: "active",
            expires_at: Some(future),
            remaining_sessions: Some(0),
            expected: SubscriptionStatus::Expired,
        },
        Case {
            name: "expired: past expires_at alone (remaining_sessions NULL)",
            status: "active",
            expires_at: Some(past),
            remaining_sessions: None,
            expected: SubscriptionStatus::Expired,
        },
        Case {
            name: "cancelled overrides an active-looking row",
            status: "cancelled",
            expires_at: Some(future),
            remaining_sessions: Some(5),
            expected: SubscriptionStatus::Cancelled,
        },
        Case {
            name: "cancelled overrides an expired-looking row",
            status: "cancelled",
            expires_at: Some(past),
            remaining_sessions: Some(5),
            expected: SubscriptionStatus::Cancelled,
        },
    ];

    for (i, case) in cases.into_iter().enumerate() {
        let user_id = common::seed_member(
            &db,
            &format!("derived-status-{i}@example.com"),
            "Password!234",
        )
        .await;
        let product_id = seed_entitlement_product(
            &db,
            &format!("ticket-derived-status-{i}"),
            "ticket",
            5_000,
            None,
            Some(10),
        )
        .await;
        let sub_id = seed_subscription(
            &db,
            user_id,
            product_id,
            case.status,
            case.expires_at,
            case.remaining_sessions.map(|_| 10),
            case.remaining_sessions,
            5_000,
            now,
        )
        .await;

        let row = subscriptions_repo::find_by_id(&db, sub_id)
            .await
            .expect("query subscription")
            .expect("subscription exists");
        assert_eq!(
            row.derived_status, case.expected,
            "case {:?}: expected {:?}, got {:?}",
            case.name, case.expected, row.derived_status
        );
    }
}

// ---------------------------------------------------------------------------
// Step 10e row 11: refunding the order that granted a subscription cancels it,
// so a later redeem must 409. The subscription is created by a REAL checkout
// (only checkout writes `subscriptions.order_id`, which the refund reverses
// by `order_id`).
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn redeem_after_refund_is_conflict(db: PgPool) {
    let ticket =
        seed_entitlement_product(&db, "refund-redeem-ticket", "ticket", 5_000, None, Some(5)).await;
    let user = seed_carted_member(
        &db,
        "refund-redeem@example.com",
        &[SeedCartLine::Product { product_id: ticket, quantity: 1 }],
        0,
    )
    .await;
    let order = orders_service::checkout(
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
    assert_eq!(order.subscriptions.len(), 1, "a ticket grants a subscription");
    let sub_id = order.subscriptions[0].id;

    // Refund the order → cancels the subscription (order-scoped).
    orders_service::update_order_status(&db, order.id, "refunded", None)
        .await
        .expect("refund");

    // Redeem now must conflict — a cancelled subscription is not redeemable.
    let err = service::redeem(&db, sub_id)
        .await
        .expect_err("a cancelled subscription must not redeem");
    assert!(matches!(err, AppError::Conflict(_)), "got {err:?}");
}
