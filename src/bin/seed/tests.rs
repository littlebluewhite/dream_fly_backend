//! `#[sqlx::test]` invariants pinning `dataset::run`'s idempotency and
//! points-ledger settlement shape — every test here only calls
//! `dataset::run` and then reads the seeded rows back with plain SQL,
//! rather than reaching into `dataset`'s private helpers. See that
//! module's doc for the shape being pinned (per-table idempotency keys,
//! the points-tier settlement loop).

use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::modules::orders::model::OrderStatus;
use dream_fly_backend::modules::points::model::{LedgerDelta, PointsTier};
use dream_fly_backend::modules::points::service::apply_delta_tx;
use dream_fly_backend::utils::studio_clock::StudioNow;

use crate::dataset;

fn taipei() -> Tz {
    "Asia/Taipei".parse().expect("valid IANA name")
}

async fn ledger_row_count(db: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM point_ledger")
        .fetch_one(db)
        .await
        .expect("count point_ledger")
}

async fn total_occupying_bookings(db: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM occupying_bookings")
        .fetch_one(db)
        .await
        .expect("count occupying_bookings")
}

async fn user_id_by_email(db: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(db)
        .await
        .expect("fetch user id")
}

async fn points_balance(db: &PgPool, user_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT points_balance FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
        .expect("fetch points_balance")
}

/// D = 2026-03-10T03:00Z (Asia/Taipei) run twice → row counts, `point_ledger`
/// row count and the `occupying_bookings` count all unchanged,
/// `settled_members == 0`. Then manually push seed-member-01 500 points above its points-tier
/// target (`member_points_target(1) == 150`) — the next run must settle
/// exactly that one member back to 150 (regression d819e21: an earlier seed
/// wrote `points_balance` directly instead of going through the ledger, so
/// a drifted balance never got corrected).
#[sqlx::test]
async fn same_instant_rerun_is_noop_and_settles_drift(db: PgPool) {
    let at = StudioNow {
        tz: taipei(),
        now: Utc.with_ymd_and_hms(2026, 3, 10, 3, 0, 0).unwrap(),
    };

    let first = dataset::run(&db, at).await.expect("first run");
    let ledger_after_first = ledger_row_count(&db).await;
    let occupying_after_first = total_occupying_bookings(&db).await;

    let second = dataset::run(&db, at).await.expect("second run");
    assert_eq!(first.row_counts, second.row_counts);
    assert_eq!(second.settled_members, 0);
    assert_eq!(ledger_row_count(&db).await, ledger_after_first);
    assert_eq!(total_occupying_bookings(&db).await, occupying_after_first);

    let member_id = user_id_by_email(&db, "seed-member-01@dreamfly.tw").await;
    let mut tx = db.begin().await.expect("begin drift tx");
    apply_delta_tx(&mut tx, member_id, LedgerDelta::admin_adjust(500))
        .await
        .expect("apply drift");
    tx.commit().await.expect("commit drift tx");
    assert_eq!(points_balance(&db, member_id).await, 650);

    let third = dataset::run(&db, at).await.expect("third run");
    assert_eq!(third.settled_members, 1);
    assert_eq!(points_balance(&db, member_id).await, 150);
}

/// Run at D, then again at D+40d (still same-month → different-month, so
/// the order loop adds new `checkout_earn` rows for existing members).
/// Every invariant below must still hold after the second run.
#[sqlx::test]
async fn later_run_keeps_invariants(db: PgPool) {
    let at = StudioNow {
        tz: taipei(),
        now: Utc.with_ymd_and_hms(2026, 3, 10, 3, 0, 0).unwrap(),
    };
    dataset::run(&db, at).await.expect("first run");

    let later = StudioNow {
        tz: taipei(),
        now: at.now + chrono::Duration::days(40),
    };
    dataset::run(&db, later).await.expect("later run");

    // ① every user's points_balance == COALESCE(SUM(point_ledger.delta), 0).
    let balances: Vec<(Uuid, i64, Option<i64>)> = sqlx::query_as(
        "SELECT u.id, u.points_balance, SUM(pl.delta)::bigint \
         FROM users u LEFT JOIN point_ledger pl ON pl.user_id = u.id \
         GROUP BY u.id, u.points_balance",
    )
    .fetch_all(&db)
    .await
    .expect("load balances");
    for (user_id, balance, ledger_sum) in balances {
        assert_eq!(balance, ledger_sum.unwrap_or(0), "user {user_id}");
    }

    // ② the 24 seed members land 6-per-tier under `PointsTier::from_balance`.
    let member_balances: Vec<i64> = sqlx::query_scalar(
        "SELECT points_balance FROM users WHERE email LIKE 'seed-member-%@dreamfly.tw'",
    )
    .fetch_all(&db)
    .await
    .expect("load seed member balances");
    assert_eq!(member_balances.len(), 24);
    for tier in PointsTier::ALL {
        let count = member_balances
            .iter()
            .filter(|&&balance| PointsTier::from_balance(balance) == tier)
            .count();
        assert_eq!(count, 6, "{tier:?}");
    }

    // ③ every `DF-SEED-%` order's `points_earned`/`points_used` match its
    // `point_ledger` rows exactly, a refunded order carries an equal
    // `refund_clawback`, and no ledger row anywhere has `delta = 0`.
    let zero_delta_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM point_ledger WHERE delta = 0")
            .fetch_one(&db)
            .await
            .expect("count zero-delta ledger rows");
    assert_eq!(zero_delta_rows, 0);

    let orders: Vec<(Uuid, i64, i64, OrderStatus)> = sqlx::query_as(
        "SELECT id, points_earned, points_used, status FROM orders \
         WHERE order_number LIKE 'DF-SEED-%'",
    )
    .fetch_all(&db)
    .await
    .expect("load seed orders");
    assert!(!orders.is_empty());
    for (order_id, points_earned, points_used, status) in orders {
        let earn_sum: Option<i64> = sqlx::query_scalar(
            "SELECT SUM(delta)::bigint FROM point_ledger \
             WHERE order_id = $1 AND reason = 'checkout_earn'::point_reason",
        )
        .bind(order_id)
        .fetch_one(&db)
        .await
        .expect("sum checkout_earn");
        assert_eq!(
            points_earned,
            earn_sum.unwrap_or(0),
            "order {order_id} points_earned"
        );

        let redeem_sum: Option<i64> = sqlx::query_scalar(
            "SELECT SUM(delta)::bigint FROM point_ledger \
             WHERE order_id = $1 AND reason = 'checkout_redeem'::point_reason",
        )
        .bind(order_id)
        .fetch_one(&db)
        .await
        .expect("sum checkout_redeem");
        assert_eq!(
            points_used,
            -redeem_sum.unwrap_or(0),
            "order {order_id} points_used"
        );

        if status == OrderStatus::Refunded {
            let clawback_sum: Option<i64> = sqlx::query_scalar(
                "SELECT SUM(delta)::bigint FROM point_ledger \
                 WHERE order_id = $1 AND reason = 'refund_clawback'::point_reason",
            )
            .bind(order_id)
            .fetch_one(&db)
            .await
            .expect("sum refund_clawback");
            assert_eq!(
                -clawback_sum.unwrap_or(0),
                earn_sum.unwrap_or(0),
                "order {order_id} refund_clawback magnitude"
            );
        }
    }
}

/// `at = 2026-03-10T20:00Z` is already 2026-03-11 04:00 Asia/Taipei — every
/// timestamp `dataset::run` writes must still clamp to `at.now`, the run
/// instant, not the studio-local wall clock it derives dates/hours from.
/// Regression: `attendance_records.marked_at` (seed.rs:1777) used to treat a
/// session's wall-clock `end_time` as if it were already UTC, with no clamp
/// — a 2026-03-10 19:00–20:30 session's `marked_at` came out as
/// 2026-03-10T20:30Z, 30 minutes *after* this `at.now`.
#[sqlx::test]
async fn seeded_timestamps_never_exceed_run_instant(db: PgPool) {
    let at = StudioNow {
        tz: taipei(),
        now: Utc.with_ymd_and_hms(2026, 3, 10, 20, 0, 0).unwrap(),
    };
    dataset::run(&db, at).await.expect("run");

    let checks: [(&str, &str); 8] = [
        ("posts.published_at", "SELECT MAX(published_at) FROM posts"),
        (
            "enrolments.created_at",
            "SELECT MAX(created_at) FROM enrolments",
        ),
        ("orders.created_at", "SELECT MAX(created_at) FROM orders"),
        ("orders.paid_at", "SELECT MAX(paid_at) FROM orders"),
        (
            "order_items.created_at",
            "SELECT MAX(created_at) FROM order_items",
        ),
        (
            "bookings.created_at",
            "SELECT MAX(created_at) FROM bookings",
        ),
        (
            "attendance_records.marked_at",
            "SELECT MAX(marked_at) FROM attendance_records",
        ),
        (
            "contact_inquiries.created_at",
            "SELECT MAX(created_at) FROM contact_inquiries",
        ),
    ];
    for (label, sql) in checks {
        let max_ts: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(sql)
            .fetch_one(&db)
            .await
            .expect("max timestamp query");
        if let Some(max_ts) = max_ts {
            assert!(
                max_ts <= at.now,
                "{label} max {max_ts} exceeds run instant {}",
                at.now
            );
        }
    }
}
