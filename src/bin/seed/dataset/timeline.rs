//! 種子購買時間線 (Seed Purchase Timeline): `buy_and_walk` buys the seed's
//! paid purchases through the real cart and checkout and walks each through
//! the order state machine. The rules (replay before the cart, buy in `ts`
//! order, step times, orders moved off their path) are in the GLOSSARY entry
//! 「種子購買時間線」 and ADR-0010 Addendum 2026-10-15.

use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::modules::cart::model::LineTarget;
use dream_fly_backend::modules::cart::service as cart_service;
use dream_fly_backend::modules::orders::dto::{CheckoutRequest, OrderResponse};
use dream_fly_backend::modules::orders::idempotency::{self, IdempotencyKey};
use dream_fly_backend::modules::orders::model::OrderStatus;
use dream_fly_backend::modules::orders::service as orders_service;
use dream_fly_backend::utils::studio_clock::StudioNow;

/// One paid seed order as its buyer places it — what to buy, for whom, when
/// — bought by `buy` through the real cart and `orders::service::checkout`,
/// so pricing, order lines, the points ledger and every artifact come from
/// checkout itself, not a seed-side copy of its steps. `key` is the
/// checkout `Idempotency-Key` (`DF-SEED-…`) and the purchase's identity
/// across runs; `buyer` is `seed-member-{member_idx:02}`; `ts` is the
/// instant it is paid at. `target` is where `advance_status` then walks the
/// order: paid, completed or refunded.
pub(super) struct SeedPurchase {
    pub(super) key: String,
    pub(super) buyer: Uuid,
    pub(super) member_idx: usize,
    pub(super) ts: DateTime<Utc>,
    pub(super) lines: Vec<(LineTarget, i32)>,
    pub(super) coupon: Option<&'static str>,
    pub(super) payment_method: &'static str,
    pub(super) target: OrderStatus,
}

/// One purchase as `buy_and_walk` left it: its buyer's `member_idx` and the
/// order (checkout's, or its replay) after the walk.
pub(super) struct Walked {
    pub(super) member_idx: usize,
    pub(super) order: OrderResponse,
}

/// Buy every purchase oldest first (a stable sort on `ts`, so outbox events
/// and notifications come out in purchase order) and walk each straight
/// after buying it. The write order does not matter to the points ledger:
/// `balance_after` is derived at read time (ADR-0018), and a refund's
/// clawback always follows its own earn while the seed spends no points, so
/// it never finds the balance short (`Conflict("點數不足")`).
/// Returns one `Walked` per purchase, in the order bought. Idempotent on
/// each purchase's `key`: a rerun buys nothing again and only finishes an
/// interrupted walk.
pub(super) async fn buy_and_walk(
    db: &PgPool,
    at: StudioNow,
    mut purchases: Vec<SeedPurchase>,
) -> anyhow::Result<Vec<Walked>> {
    purchases.sort_by_key(|purchase| purchase.ts);
    let mut walked = Vec::with_capacity(purchases.len());
    for purchase in &purchases {
        let order = buy(db, at, purchase).await?;
        walked.push(Walked {
            member_idx: purchase.member_idx,
            order: advance_status(db, at, purchase, order).await?,
        });
    }
    Ok(walked)
}

/// Buy one `SeedPurchase`; returns the order as checkout (or its replay)
/// left it, for `advance_status` to walk. Idempotent on `key`: `idempotency::replay` is
/// asked first and a hit is that order — before the cart is touched, since
/// a replayed checkout returns without clearing the cart and would leave
/// re-added lines behind. On a miss the buyer's cart is cleared, every line
/// goes through `cart::service::add_item`, and `checkout` runs at
/// `StudioNow { now: ts }` — the order row, its items, its `checkout_earn`
/// row, its grants and its enrolments all carry `ts` as their business time
/// (ADR-0017). Checkout also writes its outbox event and notification, once
/// per key.
async fn buy(db: &PgPool, at: StudioNow, purchase: &SeedPurchase) -> anyhow::Result<OrderResponse> {
    let SeedPurchase {
        key,
        buyer,
        member_idx,
        ..
    } = purchase;
    let parsed = IdempotencyKey::parse(key)
        .with_context(|| format!("parse seed idempotency key '{key}'"))?;
    let replayed = idempotency::replay(db, *buyer, Some(&parsed))
        .await
        .with_context(|| format!("replay seed purchase '{key}'"))?;
    let order = match replayed {
        Some(order) => order,
        None => {
            cart_service::clear(db, *buyer).await.with_context(|| {
                format!("clear cart of seed-member-{member_idx:02} for '{key}'")
            })?;
            for &(target, quantity) in &purchase.lines {
                let (LineTarget::Product(id) | LineTarget::Course(id)) = target;
                cart_service::add_item(db, *buyer, target.item_type().as_str(), id, quantity)
                    .await
                    .with_context(|| format!("add {target:?} to the cart for '{key}'"))?;
            }
            orders_service::checkout(
                db,
                *buyer,
                Some(parsed),
                CheckoutRequest {
                    coupon_code: purchase.coupon.map(str::to_owned),
                    use_points: Some(false),
                    payment_method: Some(purchase.payment_method.to_owned()),
                },
                None,
                StudioNow {
                    tz: at.tz,
                    now: purchase.ts,
                },
            )
            .await
            .with_context(|| format!("checkout seed purchase '{key}'"))?
        }
    };
    Ok(order)
}

/// `(status, days after paid_at)` from checkout's `paid` to
/// `purchase.target`: completed is processing (+1 day) then completed
/// (+3 days), refunded is refunded (+3 days), paid stays put.
fn status_path(purchase: &SeedPurchase) -> anyhow::Result<&'static [(OrderStatus, i64)]> {
    Ok(match purchase.target {
        OrderStatus::Paid => &[(OrderStatus::Paid, 0)],
        OrderStatus::Completed => &[
            (OrderStatus::Paid, 0),
            (OrderStatus::Processing, 1),
            (OrderStatus::Completed, 3),
        ],
        OrderStatus::Refunded => &[(OrderStatus::Paid, 0), (OrderStatus::Refunded, 3)],
        OrderStatus::Pending | OrderStatus::Processing | OrderStatus::Cancelled => {
            anyhow::bail!(
                "seed purchase '{}' cannot target {}",
                purchase.key,
                purchase.target.as_str()
            )
        }
    })
}

/// Walk a seed order from its current status to `purchase.target` through
/// `orders::service::update_order_status` — the real transition rules,
/// outbox event and notification, and for a refund the full refund
/// compensation (`refund_clawback`, grant and enrolment cancellation). Each
/// step of `status_path` runs at `min(paid_at + offset, at.now)` (`ts` when
/// no `paid_at`; the two are equal on a fresh run, and `paid_at` keeps a
/// rerun on a later day from stamping a step before the payment).
/// Steps already passed are skipped, so a rerun only finishes an
/// interrupted walk. An order whose status is off its path — a developer
/// moved it, say from the admin UI, to processing, refunded or cancelled —
/// is left as it is.
async fn advance_status(
    db: &PgPool,
    at: StudioNow,
    purchase: &SeedPurchase,
    mut order: OrderResponse,
) -> anyhow::Result<OrderResponse> {
    let path = status_path(purchase)?;
    let Some(reached) = path.iter().position(|&(status, _)| status == order.status) else {
        return Ok(order);
    };
    let base = order.paid_at.unwrap_or(purchase.ts);
    for &(status, days) in &path[reached + 1..] {
        let now = (base + Duration::days(days)).min(at.now);
        order = orders_service::update_order_status(db, order.id, status.as_str(), None, now)
            .await
            .with_context(|| {
                format!("move seed order '{}' to {}", purchase.key, status.as_str())
            })?;
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, NaiveDate, TimeZone, Utc};
    use sqlx::PgPool;
    use uuid::Uuid;

    use dream_fly_backend::modules::cart::model::LineTarget;
    use dream_fly_backend::modules::enrolments::model::EnrolmentStatus;
    use dream_fly_backend::modules::orders::model::{OrderStatus, PAYMENT_METHODS};
    use dream_fly_backend::modules::orders::service::update_order_status;
    use dream_fly_backend::utils::studio_clock::StudioNow;

    use super::{SeedPurchase, Walked, buy_and_walk};
    use crate::dataset::{
        ProductSeed, SeedAccount, collect_row_counts, insert_product, product_id_by_slug,
        upsert_account,
    };

    /// The run instant every test here buys at: 2026-03-10T03:00Z, Asia/Taipei.
    fn at() -> StudioNow {
        StudioNow {
            tz: "Asia/Taipei".parse().expect("valid IANA name"),
            now: Utc.with_ymd_and_hms(2026, 3, 10, 3, 0, 0).unwrap(),
        }
    }

    /// 2026-`month`-`day` 04:00Z, the hour the seed dates its purchases at.
    fn day(month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, month, day, 4, 0, 0).unwrap()
    }

    /// The smallest catalog a purchase can be bought from: two seed members,
    /// one merchandise product and one course.
    struct Catalog {
        members: [Uuid; 2],
        tee: LineTarget,
        course: LineTarget,
    }

    async fn minimal_catalog(db: &PgPool) -> Catalog {
        let mut members = [Uuid::nil(); 2];
        for (i, member) in members.iter_mut().enumerate() {
            *member = upsert_account(
                db,
                &SeedAccount {
                    email: &format!("seed-member-{:02}@dreamfly.tw", i + 1),
                    name: "示範會員",
                    password_hash: "not-a-real-hash",
                    birth_date: Some(NaiveDate::from_ymd_opt(2000, 1, 1).unwrap()),
                    points: 0,
                    joined_at: Utc.with_ymd_and_hms(2025, 1, 1, 4, 0, 0).unwrap(),
                },
                at().now,
            )
            .await
            .expect("upsert seed member");
        }
        insert_product(
            db,
            &ProductSeed {
                name: "學苑紀念 T 恤",
                slug: "academy-tee",
                product_type: "merchandise",
                description: "紀念 T 恤",
                price_cents: 68_000,
                features: &[],
                is_highlighted: false,
                badge: None,
                valid_days: None,
                session_count: None,
            },
        )
        .await
        .expect("insert tee");
        let tee = product_id_by_slug(db, "academy-tee").await.expect("tee id");
        let course: Uuid = sqlx::query_scalar(
            "INSERT INTO courses (name, slug, level, duration_minutes, price_cents, max_students) \
             VALUES ('跑酷入門', 'parkour-intro', 'beginner'::course_level, 60, 50000, 12) \
             RETURNING id",
        )
        .fetch_one(db)
        .await
        .expect("insert course");
        Catalog {
            members,
            tee: LineTarget::Product(tee),
            course: LineTarget::Course(course),
        }
    }

    /// One line of `target`, quantity 1, paid by credit card, no coupon;
    /// `member_idx` is 1-based into `Catalog::members`.
    fn purchase(
        c: &Catalog,
        key: &str,
        member_idx: usize,
        ts: DateTime<Utc>,
        line: LineTarget,
        target: OrderStatus,
    ) -> SeedPurchase {
        SeedPurchase {
            key: key.to_owned(),
            buyer: c.members[member_idx - 1],
            member_idx,
            ts,
            lines: vec![(line, 1)],
            coupon: None,
            payment_method: PAYMENT_METHODS[0],
            target,
        }
    }

    /// Purchases handed over out of time order come back bought and walked
    /// in `ts` order: each order paid at its `ts` and moved to its target
    /// (a refund cancels the course's enrolment), each step at
    /// `min(paid_at + offset, at.now)` — a refund's `refund_clawback` three
    /// days after the payment, or at the run instant when that is sooner.
    #[sqlx::test]
    async fn buys_in_time_order_and_walks_each_purchase(db: PgPool) {
        let c = minimal_catalog(&db).await;
        let at = at();
        use OrderStatus::{Completed, Paid, Refunded};

        let walked = buy_and_walk(
            &db,
            at,
            vec![
                purchase(&c, "T-TEE-COMPLETED", 1, day(2, 20), c.tee, Completed),
                purchase(&c, "T-TEE-REFUNDED", 1, day(3, 9), c.tee, Refunded),
                purchase(&c, "T-TEE-PAID", 2, day(2, 1), c.tee, Paid),
                purchase(&c, "T-COURSE-REFUNDED", 2, day(1, 15), c.course, Refunded),
            ],
        )
        .await
        .expect("buy and walk");

        let got: Vec<(usize, Option<DateTime<Utc>>, OrderStatus)> = walked
            .iter()
            .map(|w| (w.member_idx, w.order.paid_at, w.order.status))
            .collect();
        assert_eq!(
            got,
            vec![
                (2, Some(day(1, 15)), Refunded),
                (2, Some(day(2, 1)), Paid),
                (1, Some(day(2, 20)), Completed),
                (1, Some(day(3, 9)), Refunded),
            ]
        );
        let course_enrolments: Vec<EnrolmentStatus> = walked[0]
            .order
            .enrolments
            .iter()
            .map(|e| e.status)
            .collect();
        assert_eq!(course_enrolments, vec![EnrolmentStatus::Cancelled]);

        let clawbacks: Vec<(Option<Uuid>, DateTime<Utc>)> = sqlx::query_as(
            "SELECT order_id, created_at FROM point_ledger \
             WHERE reason = 'refund_clawback'::point_reason ORDER BY created_at",
        )
        .fetch_all(&db)
        .await
        .expect("load refund_clawback rows");
        assert_eq!(
            clawbacks,
            vec![
                (Some(walked[0].order.id), day(1, 18)),
                (Some(walked[3].order.id), at.now),
            ]
        );
    }

    /// The seed's own `collect_row_counts` plus the two tables it leaves
    /// out that a purchase touches: `point_ledger` and `cart_items`.
    async fn row_counts(db: &PgPool) -> Vec<(&'static str, i64)> {
        let mut counts = collect_row_counts(db).await.expect("collect row counts");
        for (table, sql) in [
            ("point_ledger", "SELECT COUNT(*) FROM point_ledger"),
            ("cart_items", "SELECT COUNT(*) FROM cart_items"),
        ] {
            let n: i64 = sqlx::query_scalar(sql)
                .fetch_one(db)
                .await
                .expect("count rows");
            counts.push((table, n));
        }
        counts
    }

    fn outcome(walked: &[Walked]) -> Vec<(usize, Uuid, OrderStatus)> {
        walked
            .iter()
            .map(|w| (w.member_idx, w.order.id, w.order.status))
            .collect()
    }

    /// A second call with the same purchases replays every key: the same
    /// orders come back in the same states, and no row is added anywhere —
    /// no order, ledger, enrolment, outbox or notification row, and no line
    /// left in a buyer's cart (the key is replayed before the cart is
    /// refilled).
    #[sqlx::test]
    async fn rerun_is_a_noop(db: PgPool) {
        let c = minimal_catalog(&db).await;
        let at = at();
        use OrderStatus::{Completed, Paid, Refunded};
        let purchases = || {
            vec![
                purchase(&c, "T-TEE-COMPLETED", 1, day(2, 1), c.tee, Completed),
                purchase(&c, "T-COURSE-REFUNDED", 2, day(2, 10), c.course, Refunded),
                purchase(&c, "T-TEE-PAID", 2, day(2, 20), c.tee, Paid),
            ]
        };

        let first = buy_and_walk(&db, at, purchases())
            .await
            .expect("first call");
        let after_first = row_counts(&db).await;
        let second = buy_and_walk(&db, at, purchases())
            .await
            .expect("second call");

        assert_eq!(outcome(&second), outcome(&first));
        assert_eq!(row_counts(&db).await, after_first);
        // Non-vacuous: the first call wrote every kind of row a rerun must
        // not write again.
        for table in [
            "orders",
            "order_idempotency",
            "enrolments",
            "point_ledger",
            "notifications",
            "events_outbox",
        ] {
            let n = after_first
                .iter()
                .find(|(t, _)| *t == table)
                .map(|(_, n)| *n);
            assert!(n.is_some_and(|n| n > 0), "{table}: {n:?}");
        }
        assert!(after_first.contains(&("cart_items", 0)));
    }

    /// An order a developer already moved through the state machine (say
    /// from the admin UI) sits off its walk; the next call leaves it as it
    /// is instead of failing, and hands it back in that state. A
    /// completed-target order is refunded, a paid-target order cancelled,
    /// and another paid-target order shipped to processing — a non-terminal
    /// status off its `[paid]` path.
    #[sqlx::test]
    async fn leaves_orders_a_developer_moved_off_their_path(db: PgPool) {
        let c = minimal_catalog(&db).await;
        let at = at();
        use OrderStatus::{Cancelled, Completed, Paid, Processing, Refunded};
        let purchases = || {
            vec![
                purchase(&c, "T-TEE-COMPLETED", 1, day(2, 1), c.tee, Completed),
                purchase(&c, "T-COURSE-PAID", 2, day(2, 10), c.course, Paid),
                purchase(&c, "T-TEE-PAID", 2, day(2, 20), c.tee, Paid),
            ]
        };
        let first = buy_and_walk(&db, at, purchases())
            .await
            .expect("first call");
        let reached: Vec<OrderStatus> = first.iter().map(|w| w.order.status).collect();
        assert_eq!(reached, vec![Completed, Paid, Paid]);

        let moved: Vec<(usize, Uuid, OrderStatus)> = first
            .iter()
            .zip([Refunded, Cancelled, Processing])
            .map(|(w, moved_to)| (w.member_idx, w.order.id, moved_to))
            .collect();
        for &(_, order_id, moved_to) in &moved {
            update_order_status(&db, order_id, moved_to.as_str(), None, at.now)
                .await
                .expect("move the order by hand");
        }

        let second = buy_and_walk(&db, at, purchases())
            .await
            .expect("rerun beside orders a developer moved off their path");
        assert_eq!(outcome(&second), moved);
    }
}
