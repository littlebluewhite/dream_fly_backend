//! `#[sqlx::test]` invariants pinning `dataset::run`'s idempotency and
//! points-ledger settlement shape — every test here only calls
//! `dataset::run` and then reads the seeded rows back with plain SQL,
//! rather than reaching into `dataset`'s private helpers. See that
//! module's doc for the shape being pinned (per-table idempotency keys,
//! the points-tier settlement loop).

use std::collections::HashMap;

use chrono::{Datelike, TimeZone, Utc};
use chrono_tz::Tz;
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::modules::orders::model::OrderStatus;
use dream_fly_backend::modules::orders::service::update_order_status;
use dream_fly_backend::modules::points::model::{LedgerDelta, PointsTier};
use dream_fly_backend::modules::points::repository::sum_earned_in_studio_month;
use dream_fly_backend::modules::points::service::{apply_delta_tx, lock_balance_tx};
use dream_fly_backend::modules::reports::repository::member_stats;
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

/// Ids of the seed's orders: those placed through checkout carry a
/// `DF-SEED-%` idempotency key, the rest are `DF-SEED-%`-numbered.
async fn seed_order_ids(db: &PgPool) -> Vec<Uuid> {
    sqlx::query_scalar(
        "SELECT order_id FROM order_idempotency WHERE idempotency_key LIKE 'DF-SEED-%' \
         UNION \
         SELECT id FROM orders WHERE order_number LIKE 'DF-SEED-%'",
    )
    .fetch_all(db)
    .await
    .expect("load seed order ids")
}

/// Cart lines still held by the 24 reporting members.
async fn seed_member_cart_items(db: &PgPool) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM cart_items ci JOIN users u ON u.id = ci.user_id \
         WHERE u.email LIKE 'seed-member-%@dreamfly.tw'",
    )
    .fetch_one(db)
    .await
    .expect("count seed member cart items")
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

/// D = 2026-03-10T03:00Z (Asia/Taipei) run twice → row counts (checkout's
/// `order_idempotency`/`notifications`/`events_outbox` among them),
/// `point_ledger` row count and the `occupying_bookings` count all unchanged,
/// `settled_members == 0`, and no seed member is left holding cart lines (a
/// rerun replays its keys before refilling any cart). Then manually push
/// seed-member-01 500 points above its points-tier
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
    // Non-vacuous: checkout's side-effect tables are counted, and written.
    for table in ["order_idempotency", "notifications", "events_outbox"] {
        let n = second
            .row_counts
            .iter()
            .find(|(t, _)| *t == table)
            .map(|(_, n)| *n);
        assert!(n.is_some_and(|n| n > 0), "{table}: {n:?}");
    }
    assert_eq!(seed_member_cart_items(&db).await, 0);
    assert_eq!(second.settled_members, 0);
    assert_eq!(ledger_row_count(&db).await, ledger_after_first);
    assert_eq!(total_occupying_bookings(&db).await, occupying_after_first);

    let member_id = user_id_by_email(&db, "seed-member-01@dreamfly.tw").await;
    let mut tx = db.begin().await.expect("begin drift tx");
    let lock = lock_balance_tx(&mut tx, member_id)
        .await
        .expect("lock drift balance");
    apply_delta_tx(
        &mut tx,
        &lock,
        LedgerDelta::admin_adjust(500),
        chrono::Utc::now(),
    )
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
    for &tier in PointsTier::ALL {
        let count = member_balances
            .iter()
            .filter(|&&balance| PointsTier::from_balance(balance) == tier)
            .count();
        assert_eq!(count, 6, "{tier:?}");
    }

    // ③ every seed order's (`seed_order_ids`) `points_earned`/`points_used`
    // match its `point_ledger` rows exactly, a refunded order carries an
    // equal `refund_clawback`, and no ledger row anywhere has `delta = 0`.
    let zero_delta_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM point_ledger WHERE delta = 0")
            .fetch_one(&db)
            .await
            .expect("count zero-delta ledger rows");
    assert_eq!(zero_delta_rows, 0);

    // ④ capacity invariant — the dropped `time_slots_booked_bound` CHECK used
    // to guarantee it: no slot holds more occupying bookings than its capacity.
    let over_capacity: Vec<(Uuid, i32, i64)> = sqlx::query_as(
        "SELECT ts.id, ts.capacity, COUNT(ob.*) FROM time_slots ts \
         JOIN occupying_bookings ob ON ob.time_slot_id = ts.id \
         GROUP BY ts.id, ts.capacity HAVING COUNT(ob.*) > ts.capacity",
    )
    .fetch_all(&db)
    .await
    .expect("load over-capacity slots");
    assert!(
        over_capacity.is_empty(),
        "over-capacity slots: {over_capacity:?}"
    );

    let seed_ids = seed_order_ids(&db).await;
    let orders: Vec<(Uuid, i64, i64, OrderStatus)> = sqlx::query_as(
        "SELECT id, points_earned, points_used, status FROM orders \
         WHERE id = ANY($1)",
    )
    .bind(&seed_ids)
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

    // Keys: paid seed orders are bought through checkout under their own
    // `DF-SEED-%` idempotency key — every non-pending seed order carries
    // one — and no key is recorded for two buyers: a monthly key's buyer
    // derives from the absolute month (a course key `DF-SEED-ENR-…` from its
    // `(i, k)`), so the D+40d run replays the overlapping months instead of
    // buying them again for another member.
    let keys_on_two_buyers: Vec<String> = sqlx::query_scalar(
        "SELECT idempotency_key FROM order_idempotency \
         WHERE idempotency_key LIKE 'DF-SEED-%' \
         GROUP BY idempotency_key HAVING COUNT(DISTINCT user_id) > 1",
    )
    .fetch_all(&db)
    .await
    .expect("load keys recorded for two buyers");
    assert!(
        keys_on_two_buyers.is_empty(),
        "keys on two buyers: {keys_on_two_buyers:?}"
    );

    let (non_pending, unkeyed): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(*) FILTER (WHERE NOT EXISTS ( \
                  SELECT 1 FROM order_idempotency k \
                  WHERE k.order_id = o.id AND k.idempotency_key LIKE 'DF-SEED-%')) \
         FROM orders o \
         WHERE o.id = ANY($1) AND o.status <> 'pending'::order_status",
    )
    .bind(&seed_ids)
    .fetch_one(&db)
    .await
    .expect("count non-pending seed orders without a key");
    assert!(non_pending > 0);
    assert_eq!(unkeyed, 0, "non-pending seed orders without a DF-SEED key");
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

    let checks: [(&str, &str); 9] = [
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
            "point_ledger.created_at",
            "SELECT MAX(created_at) FROM point_ledger",
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

/// Every seed order (`seed_order_ids`) is recorded at its own `paid_at`,
/// not at the run instant — regression: seed stamped its checkout ledger
/// rows `at.now` while the order kept a historical `paid_at`, so a seed member's
/// `GET /points/me` `earned_this_month` counted a whole year of
/// `checkout_earn` as this month's. Each member's `earned_this_month` counts
/// only orders paid in `at`'s studio month, checkout ledger rows equal
/// `paid_at`, refund rows land in `[paid_at, at.now]`, order items share the
/// order's `created_at`.
#[sqlx::test]
async fn paid_seed_orders_record_at_paid_instant(db: PgPool) {
    let at = StudioNow {
        tz: taipei(),
        now: Utc.with_ymd_and_hms(2026, 3, 10, 3, 0, 0).unwrap(),
    };
    dataset::run(&db, at).await.expect("run");

    let seed_ids = seed_order_ids(&db).await;

    // Non-vacuous: the seed orders do write ledger rows.
    let seed_ledger_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM point_ledger pl JOIN orders o ON o.id = pl.order_id \
         WHERE o.id = ANY($1)",
    )
    .bind(&seed_ids)
    .fetch_one(&db)
    .await
    .expect("count seed order ledger rows");
    assert!(seed_ledger_rows > 0);

    // The user-visible symptom: each member's `earned_this_month`
    // (`sum_earned_in_studio_month`, behind `GET /points/me`) sums only the
    // `checkout_earn` of seed orders paid in `at`'s studio month. Expected
    // sums bucket `paid_at` by studio month in Rust, not through the SQL
    // under test.
    let seed_order_earn: Vec<(Uuid, Option<chrono::DateTime<Utc>>, i64)> = sqlx::query_as(
        "SELECT o.user_id, o.paid_at, COALESCE(pl.delta, 0)::BIGINT FROM orders o \
         LEFT JOIN point_ledger pl \
           ON pl.order_id = o.id AND pl.reason = 'checkout_earn'::point_reason \
         WHERE o.id = ANY($1)",
    )
    .bind(&seed_ids)
    .fetch_all(&db)
    .await
    .expect("load seed order checkout_earn");
    let studio_month = |ts: chrono::DateTime<Utc>| {
        let local = ts.with_timezone(&at.tz);
        (local.year(), local.month())
    };
    let mut expected_this_month: HashMap<Uuid, i64> = HashMap::new();
    let mut earlier_month_earn = 0i64;
    for (user_id, paid_at, delta) in seed_order_earn {
        let this_month = expected_this_month.entry(user_id).or_insert(0);
        // Pending/cancelled orders: no `paid_at`, no earn row.
        let Some(paid_at) = paid_at else { continue };
        if studio_month(paid_at) == studio_month(at.now) {
            *this_month += delta;
        } else {
            earlier_month_earn += delta;
        }
    }
    // Non-vacuous: earn from earlier months exists, so stamping it `at.now`
    // would inflate `earned_this_month`.
    assert!(earlier_month_earn > 0);
    for (user_id, expected) in expected_this_month {
        let earned = sum_earned_in_studio_month(&db, user_id, at)
            .await
            .expect("sum earned this studio month");
        assert_eq!(earned, expected, "member {user_id} earned_this_month");
    }

    let checkout_rows_off_paid_at: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM point_ledger pl JOIN orders o ON o.id = pl.order_id \
         WHERE o.id = ANY($1) \
           AND pl.reason IN ('checkout_earn'::point_reason, 'checkout_redeem'::point_reason) \
           AND pl.created_at IS DISTINCT FROM o.paid_at",
    )
    .bind(&seed_ids)
    .fetch_one(&db)
    .await
    .expect("count checkout ledger rows off paid_at");
    assert_eq!(checkout_rows_off_paid_at, 0);

    let refund_rows_out_of_range: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM point_ledger pl JOIN orders o ON o.id = pl.order_id \
         WHERE o.id = ANY($1) \
           AND pl.reason IN ('refund_restore'::point_reason, 'refund_clawback'::point_reason) \
           AND NOT (pl.created_at BETWEEN o.paid_at AND $2)",
    )
    .bind(&seed_ids)
    .bind(at.now)
    .fetch_one(&db)
    .await
    .expect("count refund ledger rows outside [paid_at, at.now]");
    assert_eq!(refund_rows_out_of_range, 0);

    let items_off_order_created_at: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM order_items oi JOIN orders o ON o.id = oi.order_id \
         WHERE o.id = ANY($1) AND oi.created_at <> o.created_at",
    )
    .bind(&seed_ids)
    .fetch_one(&db)
    .await
    .expect("count order items off the order's created_at");
    assert_eq!(items_off_order_created_at, 0);
}

/// Every seeded user exists before every seeded row that points at them —
/// regression: `upsert_user`/`upsert_seed_member` stamped `users.created_at`
/// with the run instant while the same users' orders, enrolments, ledger,
/// bookings, attendance marks, posts and inquiry assignments carry
/// historical timestamps, so every seed account "joined today": the admin
/// KPI `new_members_this` and `member_stats`' `new_this_month` counted all
/// of them as this month's sign-ups. The referencing columns are the
/// historical timestamps `seeded_timestamps_never_exceed_run_instant`
/// checks.
#[sqlx::test]
async fn seeded_users_exist_before_rows_that_reference_them(db: PgPool) {
    let at = StudioNow {
        tz: taipei(),
        now: Utc.with_ymd_and_hms(2026, 3, 10, 3, 0, 0).unwrap(),
    };
    dataset::run(&db, at).await.expect("run");

    let referenced: i64 = sqlx::query_scalar(
        "WITH refs (user_id, ts) AS ( \
           SELECT user_id, created_at FROM orders \
           UNION ALL SELECT user_id, created_at FROM enrolments \
           UNION ALL SELECT user_id, created_at FROM point_ledger \
           UNION ALL SELECT user_id, created_at FROM bookings \
           UNION ALL SELECT marked_by, marked_at FROM attendance_records \
           UNION ALL SELECT author_id, published_at FROM posts \
           UNION ALL SELECT assigned_to, created_at FROM contact_inquiries \
         ) \
         SELECT COUNT(DISTINCT user_id) FROM refs WHERE user_id IS NOT NULL AND ts IS NOT NULL",
    )
    .fetch_one(&db)
    .await
    .expect("count referenced users");
    // Non-vacuous: members, coaches and the admin are all referenced.
    assert!(referenced > 24, "only {referenced} users referenced");

    let joined_after_history: i64 = sqlx::query_scalar(
        "WITH refs (user_id, ts) AS ( \
           SELECT user_id, created_at FROM orders \
           UNION ALL SELECT user_id, created_at FROM enrolments \
           UNION ALL SELECT user_id, created_at FROM point_ledger \
           UNION ALL SELECT user_id, created_at FROM bookings \
           UNION ALL SELECT marked_by, marked_at FROM attendance_records \
           UNION ALL SELECT author_id, published_at FROM posts \
           UNION ALL SELECT assigned_to, created_at FROM contact_inquiries \
         ) \
         SELECT COUNT(*) FROM users u \
         WHERE u.created_at > (SELECT MIN(r.ts) FROM refs r WHERE r.user_id = u.id)",
    )
    .fetch_one(&db)
    .await
    .expect("count users created after their earliest referencing row");
    assert_eq!(
        joined_after_history, 0,
        "users created after their own history"
    );

    // The user-visible symptom: no seed account counts as this month's
    // sign-up.
    let (_, new_this_month, _) = member_stats(&db, at).await.expect("member stats");
    assert_eq!(new_this_month, 0);
}

/// Every course line of a seed order (`seed_order_ids`) that checkout
/// placed enrols its buyer: exactly one enrolment carrying the order's id,
/// buyer and course, enrolled and created at the order's `paid_at`, and
/// cancelled exactly when the order is refunded; no enrolment anywhere lacks
/// an order. The enrolment dataset is 39 course purchases, 3 of them
/// refunded. Regression: the seed wrote its enrolments directly with
/// `order_id = NULL`, so a seed member sat in a course nobody had bought.
#[sqlx::test]
async fn seed_course_orders_enrol_their_buyer(db: PgPool) {
    let at = StudioNow {
        tz: taipei(),
        now: Utc.with_ymd_and_hms(2026, 3, 10, 3, 0, 0).unwrap(),
    };
    dataset::run(&db, at).await.expect("run");

    let seed_ids = seed_order_ids(&db).await;

    // Non-vacuous: every enrolment pick is a course line, refunds included.
    let (course_lines, refunded_course_lines): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(*) FILTER (WHERE o.status = 'refunded'::order_status) \
         FROM order_items oi JOIN orders o ON o.id = oi.order_id \
         WHERE o.id = ANY($1) AND o.status <> 'pending'::order_status \
           AND oi.course_id IS NOT NULL",
    )
    .bind(&seed_ids)
    .fetch_one(&db)
    .await
    .expect("count seed course lines");
    assert_eq!((course_lines, refunded_course_lines), (39, 3));

    let lines_without_matching_enrolment: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM order_items oi \
         JOIN orders o ON o.id = oi.order_id \
         WHERE o.id = ANY($1) AND o.status <> 'pending'::order_status \
           AND oi.course_id IS NOT NULL \
           AND (SELECT COUNT(*) FROM enrolments e \
                WHERE e.order_id = o.id AND e.course_id = oi.course_id \
                  AND e.user_id = o.user_id \
                  AND e.enrolled_at = o.paid_at AND e.created_at = o.paid_at \
                  AND (e.status = 'cancelled'::enrolment_status) \
                      = (o.status = 'refunded'::order_status)) <> 1",
    )
    .bind(&seed_ids)
    .fetch_one(&db)
    .await
    .expect("count course lines without their enrolment");
    assert_eq!(lines_without_matching_enrolment, 0);

    let enrolments_without_order: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM enrolments WHERE order_id IS NULL")
            .fetch_one(&db)
            .await
            .expect("count enrolments without an order");
    assert_eq!(enrolments_without_order, 0);
}

/// A database an older seed wrote is refused before `run` writes anything:
/// a seed member's enrolment with no order, or a `DF-SEED-%`-numbered order
/// that was paid, is a row the old seed wrote directly, and checkout would
/// buy it a second time. Neither a hand-made enrolment of a non-seed account
/// nor a seed pending order an admin cancelled through the state machine is
/// legacy data, so neither trips the guard.
#[sqlx::test]
async fn run_refuses_a_database_seeded_by_an_older_seed(db: PgPool) {
    let at = StudioNow {
        tz: taipei(),
        now: Utc.with_ymd_and_hms(2026, 3, 10, 3, 0, 0).unwrap(),
    };
    dataset::run(&db, at).await.expect("first run");

    sqlx::query(
        "INSERT INTO enrolments (user_id, course_id, order_id) \
         SELECT u.id, c.id, NULL FROM users u, courses c \
         WHERE u.email = 'member@dreamfly.tw' AND c.slug = 'parkour-intro'",
    )
    .execute(&db)
    .await
    .expect("enrol the test member by hand");
    dataset::run(&db, at)
        .await
        .expect("a non-seed enrolment without an order is not legacy seed data");

    // An admin cancels a seed pending order: Pending → Cancelled is legal.
    let pending_order: Uuid = sqlx::query_scalar(
        "SELECT id FROM orders \
         WHERE order_number LIKE 'DF-SEED-%' AND status = 'pending'::order_status LIMIT 1",
    )
    .fetch_one(&db)
    .await
    .expect("load a seed pending order");
    update_order_status(&db, pending_order, "cancelled", None, at.now)
        .await
        .expect("cancel the seed pending order");
    dataset::run(&db, at)
        .await
        .expect("a seed pending order an admin cancelled is not legacy seed data");

    // An old seed's enrolment: a seed member's, with no order.
    let legacy_enrolment: Uuid = sqlx::query_scalar(
        "INSERT INTO enrolments (user_id, course_id, order_id, status) \
         SELECT u.id, c.id, NULL, 'cancelled'::enrolment_status FROM users u, courses c \
         WHERE u.email = 'seed-member-01@dreamfly.tw' AND c.slug = 'parkour-intro' \
         RETURNING id",
    )
    .fetch_one(&db)
    .await
    .expect("plant a legacy seed enrolment");
    let err = dataset::run(&db, at)
        .await
        .err()
        .expect("a seed enrolment without an order is refused");
    assert!(err.to_string().contains("reset the dev DB"), "{err:#}");
    sqlx::query("DELETE FROM enrolments WHERE id = $1")
        .bind(legacy_enrolment)
        .execute(&db)
        .await
        .expect("remove the legacy enrolment");

    // An old seed's paid order: written directly under a DF-SEED number.
    sqlx::query(
        "UPDATE orders SET status = 'paid'::order_status \
         WHERE id = (SELECT id FROM orders \
                     WHERE order_number LIKE 'DF-SEED-%' AND status = 'pending'::order_status \
                     LIMIT 1)",
    )
    .execute(&db)
    .await
    .expect("plant a legacy paid seed order");
    let err = dataset::run(&db, at)
        .await
        .err()
        .expect("a non-pending DF-SEED-numbered order is refused");
    assert!(err.to_string().contains("reset the dev DB"), "{err:#}");
}

/// A seed checkout order a developer already moved through the state machine
/// (say from the admin UI) sits off its walk; the next run leaves it as it is
/// instead of failing. One completed-target order is refunded, one
/// paid-target order cancelled — each picked so the buyer's balance covers
/// its `refund_clawback` — and another paid-target order shipped to
/// processing, a non-terminal status off its `[paid]` path.
#[sqlx::test]
async fn rerun_leaves_seed_orders_a_developer_moved_off_their_path(db: PgPool) {
    let at = StudioNow {
        tz: taipei(),
        now: Utc.with_ymd_and_hms(2026, 3, 10, 3, 0, 0).unwrap(),
    };
    dataset::run(&db, at).await.expect("first run");

    let mut moved: Vec<(Uuid, OrderStatus)> = Vec::new();
    for (reached, moved_to) in [
        (OrderStatus::Completed, OrderStatus::Refunded),
        (OrderStatus::Paid, OrderStatus::Cancelled),
        (OrderStatus::Paid, OrderStatus::Processing),
    ] {
        let taken: Vec<Uuid> = moved.iter().map(|(id, _)| *id).collect();
        let order_id: Uuid = sqlx::query_scalar(
            "SELECT o.id FROM orders o \
             JOIN order_idempotency k ON k.order_id = o.id \
             JOIN users u ON u.id = o.user_id \
             WHERE k.idempotency_key LIKE 'DF-SEED-%' AND o.status = $1 \
               AND o.points_earned <= u.points_balance \
               AND o.id <> ALL($2) \
             ORDER BY o.paid_at, o.id LIMIT 1",
        )
        .bind(reached)
        .bind(&taken)
        .fetch_one(&db)
        .await
        .expect("load a seed checkout order");
        update_order_status(&db, order_id, moved_to.as_str(), None, at.now)
            .await
            .expect("move the seed order by hand");
        moved.push((order_id, moved_to));
    }

    dataset::run(&db, at)
        .await
        .expect("rerun beside seed orders a developer moved off their path");
    for (order_id, moved_to) in moved {
        let status: OrderStatus = sqlx::query_scalar("SELECT status FROM orders WHERE id = $1")
            .bind(order_id)
            .fetch_one(&db)
            .await
            .expect("load the order's status");
        assert_eq!(status, moved_to, "order {order_id}");
    }
}
