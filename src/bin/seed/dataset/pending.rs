//! 待付款對照單 (Pending Contrast Orders), seed side: `insert_pending_orders`
//! writes the seed's never-paid orders. Checkout only creates paid orders,
//! so this is the one bypass left on the order path (ADR-0010), but the rows
//! still go through the orders module's own writers — `create_order`
//! (`status: Pending`, `paid_at: None`) and `create_order_items` — priced by
//! `orders::pricing::price`; the seed keeps no order-column list of its own.
//! The rules are in the GLOSSARY entry 「Seed 資料集」.

use std::collections::HashMap;

use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::modules::orders::fulfilment::{self, PurchasableCart};
use dream_fly_backend::modules::orders::model::OrderStatus;
use dream_fly_backend::modules::orders::pricing;
use dream_fly_backend::modules::orders::repository as orders_repository;

/// One never-paid contrast order: `order_number` (`DF-SEED-…`) is its
/// identity across runs; `lines` is the `PurchasableCart` checkout would
/// price (built through `fulfilment::ensure_all_purchasable`), priced with
/// no coupon and no points; `created_at` is its historical instant.
pub(super) struct PendingSeedOrder {
    pub(super) order_number: String,
    pub(super) user_id: Uuid,
    pub(super) created_at: DateTime<Utc>,
    pub(super) payment_method: &'static str,
    pub(super) lines: PurchasableCart,
}

/// Insert every order in `orders` that is not there yet — idempotent by an
/// existence check on `order_number` (order_items has no unique key of its
/// own, so the parent check guards both), whatever status the existing row
/// has now: one an admin cancelled stays cancelled. Each new order is one
/// transaction: the row through `create_order` (`status: Pending`,
/// `paid_at: None`, `points_earned = 0`), its items through
/// `fulfilment::order_lines` and `create_order_items`, both at
/// `created_at`. No ledger row, no grant, no stock decrement.
pub(super) async fn insert_pending_orders(
    db: &PgPool,
    orders: &[PendingSeedOrder],
) -> anyhow::Result<()> {
    for seed in orders {
        insert_if_absent(db, seed).await?;
    }
    Ok(())
}

async fn insert_if_absent(db: &PgPool, seed: &PendingSeedOrder) -> anyhow::Result<()> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM orders WHERE order_number = $1)")
            .bind(&seed.order_number)
            .fetch_one(db)
            .await
            .with_context(|| format!("check existing order '{}'", seed.order_number))?;
    if exists {
        return Ok(());
    }

    let pricing = pricing::price(&seed.lines, None, 0, false)
        .with_context(|| format!("price seed order '{}'", seed.order_number))?;
    let mut tx = db.begin().await.context("begin order tx")?;
    // Seed never decrements stock, so no line is marked `stock_decremented`.
    let lines = fulfilment::order_lines(&seed.lines, &HashMap::new());

    let order = orders_repository::create_order(
        &mut tx,
        &orders_repository::NewOrder {
            user_id: seed.user_id,
            order_number: &seed.order_number,
            status: OrderStatus::Pending,
            amounts: orders_repository::OrderAmounts {
                total_cents: pricing.total_cents,
                discount_cents: pricing.discount_cents,
                points_used: 0,
                points_earned: 0,
            },
            coupon_code: pricing.applied_coupon_code.as_deref(),
            payment_method: Some(seed.payment_method),
            paid_at: None,
            created_at: seed.created_at,
        },
    )
    .await
    .with_context(|| format!("insert order '{}'", seed.order_number))?;

    orders_repository::create_order_items(&mut tx, order.id, &lines, seed.created_at)
        .await
        .with_context(|| format!("insert order_items for '{}'", seed.order_number))?;

    tx.commit().await.context("commit order tx")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, NaiveDate, TimeZone, Utc};
    use sqlx::PgPool;
    use uuid::Uuid;

    use dream_fly_backend::modules::cart::model::{CheckoutLine, LineTarget};
    use dream_fly_backend::modules::orders::fulfilment::ensure_all_purchasable;
    use dream_fly_backend::modules::orders::model::PAYMENT_METHODS;
    use dream_fly_backend::modules::orders::service::update_order_status;

    use super::{PendingSeedOrder, insert_pending_orders};
    use crate::dataset::{SeedAccount, upsert_account};

    /// The run instant every test here writes at: 2026-03-10T03:00Z.
    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 10, 3, 0, 0).unwrap()
    }

    /// The historical instant every pending order here is created at.
    fn created_at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 2, 4, 4, 0, 0).unwrap()
    }

    /// The smallest catalog a pending order can be priced from: one seed
    /// member and one tee with finite stock (5), so a decrement would show.
    struct Catalog {
        member: Uuid,
        tee: Uuid,
    }

    async fn minimal_catalog(db: &PgPool) -> Catalog {
        let member = upsert_account(
            db,
            &SeedAccount {
                email: "seed-member-01@dreamfly.tw",
                name: "示範會員",
                password_hash: "not-a-real-hash",
                birth_date: Some(NaiveDate::from_ymd_opt(2000, 1, 1).unwrap()),
                points: 0,
                joined_at: Utc.with_ymd_and_hms(2025, 1, 1, 4, 0, 0).unwrap(),
            },
            at(),
        )
        .await
        .expect("upsert seed member");
        let tee: Uuid = sqlx::query_scalar(
            "INSERT INTO products (name, slug, product_type, price_cents, stock) \
             VALUES ('學苑紀念 T 恤', 'academy-tee', 'merchandise'::product_type, 68000, 5) \
             RETURNING id",
        )
        .fetch_one(db)
        .await
        .expect("insert tee");
        Catalog { member, tee }
    }

    /// Two tees at 680 NTD each, by credit card.
    fn two_tees(c: &Catalog) -> PendingSeedOrder {
        PendingSeedOrder {
            order_number: "DF-SEED-202602-07".to_owned(),
            user_id: c.member,
            created_at: created_at(),
            payment_method: PAYMENT_METHODS[0],
            lines: ensure_all_purchasable(vec![CheckoutLine {
                target: LineTarget::Product(c.tee),
                quantity: 2,
                price_cents: 68_000,
                name: "學苑紀念 T 恤".to_owned(),
                is_active: true,
            }])
            .expect("tee is purchasable"),
        }
    }

    #[derive(sqlx::FromRow)]
    struct OrderRow {
        id: Uuid,
        status: String,
        paid_at: Option<DateTime<Utc>>,
        created_at: DateTime<Utc>,
        total_cents: i64,
        discount_cents: i64,
        points_earned: i64,
    }

    async fn order_rows(db: &PgPool) -> Vec<OrderRow> {
        sqlx::query_as(
            "SELECT id, status::TEXT AS status, paid_at, created_at, total_cents, discount_cents, \
             points_earned::BIGINT AS points_earned FROM orders \
             WHERE order_number = 'DF-SEED-202602-07'",
        )
        .fetch_all(db)
        .await
        .expect("fetch orders")
    }

    #[derive(Debug, PartialEq, sqlx::FromRow)]
    struct ItemRow {
        product_id: Option<Uuid>,
        quantity: i32,
        unit_price_cents: i64,
        stock_decremented: bool,
        created_at: DateTime<Utc>,
    }

    /// A pending order lands as one never-paid row priced like checkout
    /// would (2 × 68000), its line at the same `created_at`, and writes no
    /// points ledger row and leaves stock alone.
    #[sqlx::test]
    async fn writes_a_never_paid_order_priced_like_checkout(db: PgPool) {
        let c = minimal_catalog(&db).await;

        insert_pending_orders(&db, &[two_tees(&c)])
            .await
            .expect("insert pending orders");

        let rows = order_rows(&db).await;
        assert_eq!(rows.len(), 1);
        let order = &rows[0];
        assert_eq!(order.status, "pending");
        assert_eq!(order.paid_at, None);
        assert_eq!(order.created_at, created_at());
        assert_eq!(order.total_cents, 136_000);
        assert_eq!(order.discount_cents, 0);
        assert_eq!(order.points_earned, 0);

        let items: Vec<ItemRow> = sqlx::query_as(
            "SELECT product_id, quantity, unit_price_cents, stock_decremented, created_at \
             FROM order_items WHERE order_id = $1",
        )
        .bind(order.id)
        .fetch_all(&db)
        .await
        .expect("fetch order_items");
        assert_eq!(
            items,
            vec![ItemRow {
                product_id: Some(c.tee),
                quantity: 2,
                unit_price_cents: 68_000,
                stock_decremented: false,
                created_at: created_at(),
            }]
        );

        let ledger: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM point_ledger")
            .fetch_one(&db)
            .await
            .expect("count ledger");
        assert_eq!(ledger, 0);
        let stock: Option<i32> = sqlx::query_scalar("SELECT stock FROM products WHERE id = $1")
            .bind(c.tee)
            .fetch_one(&db)
            .await
            .expect("fetch stock");
        assert_eq!(stock, Some(5));
    }

    /// Drift: once an admin cancelled the pending order (the legal
    /// Pending → Cancelled edge), a rerun finds it by `order_number` and
    /// leaves it cancelled — no second row, no status reset.
    #[sqlx::test]
    async fn rerun_after_an_admin_cancelled_it_keeps_it_cancelled(db: PgPool) {
        let c = minimal_catalog(&db).await;
        insert_pending_orders(&db, &[two_tees(&c)])
            .await
            .expect("first insert");
        let id = order_rows(&db).await[0].id;
        update_order_status(&db, id, "cancelled", None, at())
            .await
            .expect("admin cancels");

        insert_pending_orders(&db, &[two_tees(&c)])
            .await
            .expect("rerun insert");

        let rows = order_rows(&db).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].status, "cancelled");
        let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM order_items WHERE order_id = $1")
            .bind(id)
            .fetch_one(&db)
            .await
            .expect("count order_items");
        assert_eq!(items, 1);
    }
}
