use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::modules::cart::model::CartItemType;

use super::fulfilment::OrderLine;
use super::model::{AdminOrderRow, Order, OrderItem, OrderStatus, OrderSummaryRow};

/// The four order-row amount fields of a [`NewOrder`]. Named
/// `OrderAmounts` rather than `OrderMoney` because `points_used`/
/// `points_earned` are points, not money.
#[derive(Clone, Copy)]
pub struct OrderAmounts {
    pub total_cents: i64,
    pub discount_cents: i64,
    pub points_used: i64,
    pub points_earned: i64,
}

/// One `orders` row as `create_order` writes it. A struct rather than
/// eight positional arguments (clippy `too_many_arguments`).
pub struct NewOrder<'a> {
    pub user_id: Uuid,
    pub order_number: &'a str,
    pub status: OrderStatus,
    pub amounts: OrderAmounts,
    pub coupon_code: Option<&'a str>,
    /// `None` is stored NULL; the revenue report shows it as 'unknown'.
    pub payment_method: Option<&'a str>,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Create the order row — the one writer of `orders` rows. Checkout has no
/// separate payment-capture step (succeeding IS the payment), so it passes
/// `status: Paid` with `paid_at = created_at = now`; the dev seed's pending
/// contrast orders and the tests' `OrderSeed` pass their own status/times.
/// `id` is `gen_random_uuid()`. `paid_at`/`created_at` 是業務時間(報表依
/// `paid_at` 分桶),綁呼叫端給的值;`updated_at` 是稽核時間,一律 `NOW()`
/// (ADR-0017)。
pub async fn create_order(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    order: &NewOrder<'_>,
) -> Result<Order, sqlx::Error> {
    sqlx::query_as::<_, Order>(
        "INSERT INTO orders (id, user_id, order_number, status, total_cents, discount_cents, \
         coupon_code, points_used, points_earned, payment_method, paid_at, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW()) \
         RETURNING *",
    )
    .bind(order.user_id)
    .bind(order.order_number)
    .bind(order.status)
    .bind(order.amounts.total_cents)
    .bind(order.amounts.discount_cents)
    .bind(order.coupon_code)
    .bind(order.amounts.points_used)
    .bind(order.amounts.points_earned)
    .bind(order.payment_method)
    .bind(order.paid_at)
    .bind(order.created_at)
    .fetch_one(&mut **tx)
    .await
}

/// Insert order_items for both product and course lines in one bulk
/// INSERT. Each `OrderLine` (`orders::fulfilment::OrderLine`) carries a
/// `LineTarget`; `item_type`, `product_id` and `course_id` are all bound
/// from that one value (its accessors), so the columns can never disagree
/// with each other.
/// `name` is the checkout-time display name (from the cart snapshot) —
/// stored verbatim so later reads never need to join the live
/// product/course catalog. `stock_decremented` is the checkout-time fact of
/// whether this line actually decremented `products.stock` — the callers
/// (`service::checkout`, and the dev seed's
/// `insert_pending_order_if_absent` for pending orders) get it
/// pre-derived per line from `fulfilment::order_lines`; always `false` for
/// course lines.
/// `created_at` 綁呼叫端傳入的時間,與訂單列的 `created_at` 同一刻(已付訂單即 `paid_at`;ADR-0017)。
pub async fn create_order_items(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    order_id: Uuid,
    items: &[OrderLine],
    created_at: DateTime<Utc>,
) -> Result<Vec<OrderItem>, sqlx::Error> {
    let len = items.len();
    let mut ids: Vec<Uuid> = Vec::with_capacity(len);
    let mut item_types: Vec<CartItemType> = Vec::with_capacity(len);
    let mut product_ids: Vec<Option<Uuid>> = Vec::with_capacity(len);
    let mut course_ids: Vec<Option<Uuid>> = Vec::with_capacity(len);
    let mut quantities: Vec<i32> = Vec::with_capacity(len);
    let mut prices: Vec<i64> = Vec::with_capacity(len);
    let mut names: Vec<String> = Vec::with_capacity(len);
    let mut stock_decremented: Vec<bool> = Vec::with_capacity(len);

    for line in items {
        ids.push(Uuid::now_v7());
        item_types.push(line.target.item_type());
        product_ids.push(line.target.product_id());
        course_ids.push(line.target.course_id());
        quantities.push(line.quantity);
        prices.push(line.price_cents);
        names.push(line.name.clone());
        stock_decremented.push(line.stock_decremented);
    }

    sqlx::query_as::<_, OrderItem>(
        "INSERT INTO order_items (id, order_id, item_type, product_id, course_id, quantity, \
         unit_price_cents, name, stock_decremented, created_at) \
         SELECT u.id, $2, u.item_type, \
                u.product_id, u.course_id, u.quantity, u.unit_price_cents, u.name, \
                u.stock_decremented, $10 \
         FROM unnest($1::uuid[], $3::uuid[], $4::uuid[], $5::int[], $6::bigint[], $7::text[], $8::bool[], \
                     $9::cart_item_type[]) \
              AS u(id, product_id, course_id, quantity, unit_price_cents, name, stock_decremented, \
                   item_type) \
         RETURNING *",
    )
    .bind(&ids)
    .bind(order_id)
    .bind(&product_ids)
    .bind(&course_ids)
    .bind(&quantities)
    .bind(&prices)
    .bind(&names)
    .bind(&stock_decremented)
    .bind(&item_types)
    .bind(created_at)
    .fetch_all(&mut **tx)
    .await
}

pub async fn find_by_id(db: &PgPool, id: Uuid) -> Result<Option<Order>, sqlx::Error> {
    sqlx::query_as::<_, Order>(
        "SELECT id, user_id, order_number, status, total_cents, discount_cents, \
         coupon_code, points_used, points_earned, payment_method, paid_at, created_at, updated_at \
         FROM orders WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

pub async fn find_by_id_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<Order>, sqlx::Error> {
    sqlx::query_as::<_, Order>(
        "SELECT id, user_id, order_number, status, total_cents, discount_cents, \
         coupon_code, points_used, points_earned, payment_method, paid_at, created_at, updated_at \
         FROM orders WHERE id = $1 \
         FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
}

/// Paginated order summaries for a user, `items` aggregated via a single
/// `jsonb_agg` correlated subquery per row (index-backed by
/// `idx_order_items_order_id`) — one query total for the whole page, not
/// one query per order.
pub async fn find_by_user(
    db: &PgPool,
    user_id: Uuid,
    limit: u32,
    offset: u32,
) -> Result<Vec<OrderSummaryRow>, sqlx::Error> {
    sqlx::query_as::<_, OrderSummaryRow>(
        "SELECT o.id, o.order_number, o.status, o.total_cents, o.created_at, \
                COALESCE( \
                  (SELECT jsonb_agg(jsonb_build_object('name', oi.name, 'quantity', oi.quantity) ORDER BY oi.created_at) \
                   FROM order_items oi WHERE oi.order_id = o.id), \
                  '[]'::jsonb \
                ) AS items \
         FROM orders o \
         WHERE o.user_id = $1 \
         ORDER BY o.created_at DESC \
         LIMIT $2 OFFSET $3",
    )
    .bind(user_id)
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(db)
    .await
}

pub async fn count_by_user(db: &PgPool, user_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM orders WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
}

pub async fn find_items_by_order(
    db: &PgPool,
    order_id: Uuid,
) -> Result<Vec<OrderItem>, sqlx::Error> {
    sqlx::query_as::<_, OrderItem>(
        "SELECT id, order_id, item_type, product_id, course_id, quantity, unit_price_cents, \
         stock_decremented, created_at \
         FROM order_items \
         WHERE order_id = $1 \
         ORDER BY created_at",
    )
    .bind(order_id)
    .fetch_all(db)
    .await
}

/// Single atomic UPDATE of the status. `paid_at` is never touched: checkout
/// inserts every order already `paid` with `paid_at` stamped (see
/// `create_order`), and there is no `pending → paid` transition, so a status
/// change never has a payment time to stamp (a refunded/cancelled order
/// keeps its original `paid_at`).
pub async fn update_status_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    status: &OrderStatus,
) -> Result<Option<Order>, sqlx::Error> {
    sqlx::query_as::<_, Order>(
        "UPDATE orders SET \
            status = $2, \
            updated_at = NOW() \
         WHERE id = $1 \
         RETURNING *",
    )
    .bind(id)
    .bind(*status)
    .fetch_optional(&mut **tx)
    .await
}

// ---------------------------------------------------------------------------
// Admin order list
// ---------------------------------------------------------------------------

/// Same `items` aggregation approach as [`find_by_user`] (one `jsonb_agg`
/// correlated subquery per row, no N+1) — the admin order list is a
/// paginated list too, so it gets the same treatment.
pub async fn find_all_with_user(
    db: &PgPool,
    limit: u32,
    offset: u32,
) -> Result<Vec<AdminOrderRow>, sqlx::Error> {
    sqlx::query_as::<_, AdminOrderRow>(
        "SELECT o.id, o.order_number, u.name AS user_name, u.email AS user_email, \
                o.status, o.total_cents, o.points_used, o.coupon_code, o.paid_at, o.created_at, \
                COALESCE( \
                  (SELECT jsonb_agg(jsonb_build_object('name', oi.name, 'quantity', oi.quantity) ORDER BY oi.created_at) \
                   FROM order_items oi WHERE oi.order_id = o.id), \
                  '[]'::jsonb \
                ) AS items \
         FROM orders o \
         JOIN users u ON u.id = o.user_id \
         ORDER BY o.created_at DESC \
         LIMIT $1 OFFSET $2",
    )
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(db)
    .await
}

pub async fn count_all(db: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM orders")
        .fetch_one(db)
        .await
}
