use sqlx::PgPool;
use uuid::Uuid;

use super::model::{AdminOrderRow, Order, OrderItem, OrderStatus, OrderSummaryRow};

/// Create the order row. Checkout in this application has no separate
/// payment-capture step — succeeding IS the payment — so the row is
/// inserted already `status = 'paid'` with `paid_at` stamped, rather than
/// starting `pending` and needing a follow-up transition.
#[allow(clippy::too_many_arguments)]
pub async fn create_order(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    order_number: &str,
    total_cents: i64,
    discount_cents: i64,
    coupon_code: Option<&str>,
    points_used: i64,
    points_earned: i64,
    payment_method: &str,
) -> Result<Order, sqlx::Error> {
    sqlx::query_as::<_, Order>(
        "INSERT INTO orders (id, user_id, order_number, status, total_cents, discount_cents, \
         coupon_code, points_used, points_earned, payment_method, paid_at, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, 'paid'::order_status, $3, $4, $5, $6, $7, $8, NOW(), \
         NOW(), NOW()) \
         RETURNING *",
    )
    .bind(user_id)
    .bind(order_number)
    .bind(total_cents)
    .bind(discount_cents)
    .bind(coupon_code)
    .bind(points_used)
    .bind(points_earned)
    .bind(payment_method)
    .fetch_one(&mut **tx)
    .await
}

/// Insert order_items for both product and course lines in one bulk
/// INSERT. Each tuple is `(product_id, course_id, quantity,
/// unit_price_cents, name)` with exactly one of `product_id`/`course_id`
/// set; `item_type` is derived server-side from which one is present
/// (rather than passed as a separate value) so the column can never
/// disagree with the ids that actually got stored. `name` is the
/// checkout-time display name (from the cart snapshot) — stored verbatim so
/// later reads never need to join the live product/course catalog.
pub async fn create_order_items(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    order_id: Uuid,
    items: &[(Option<Uuid>, Option<Uuid>, i32, i64, String)],
) -> Result<Vec<OrderItem>, sqlx::Error> {
    let len = items.len();
    let mut ids: Vec<Uuid> = Vec::with_capacity(len);
    let mut product_ids: Vec<Option<Uuid>> = Vec::with_capacity(len);
    let mut course_ids: Vec<Option<Uuid>> = Vec::with_capacity(len);
    let mut quantities: Vec<i32> = Vec::with_capacity(len);
    let mut prices: Vec<i64> = Vec::with_capacity(len);
    let mut names: Vec<String> = Vec::with_capacity(len);

    for (product_id, course_id, quantity, unit_price_cents, name) in items {
        ids.push(Uuid::now_v7());
        product_ids.push(*product_id);
        course_ids.push(*course_id);
        quantities.push(*quantity);
        prices.push(*unit_price_cents);
        names.push(name.clone());
    }

    sqlx::query_as::<_, OrderItem>(
        "INSERT INTO order_items (id, order_id, item_type, product_id, course_id, quantity, \
         unit_price_cents, name, created_at) \
         SELECT u.id, $2, \
                CASE WHEN u.product_id IS NOT NULL THEN 'product'::cart_item_type \
                     ELSE 'course'::cart_item_type END, \
                u.product_id, u.course_id, u.quantity, u.unit_price_cents, u.name, NOW() \
         FROM unnest($1::uuid[], $3::uuid[], $4::uuid[], $5::int[], $6::bigint[], $7::text[]) \
              AS u(id, product_id, course_id, quantity, unit_price_cents, name) \
         RETURNING *",
    )
    .bind(&ids)
    .bind(order_id)
    .bind(&product_ids)
    .bind(&course_ids)
    .bind(&quantities)
    .bind(&prices)
    .bind(&names)
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
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM orders WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(db)
    .await
}

pub async fn find_items_by_order(
    db: &PgPool,
    order_id: Uuid,
) -> Result<Vec<OrderItem>, sqlx::Error> {
    sqlx::query_as::<_, OrderItem>(
        "SELECT id, order_id, item_type, product_id, course_id, quantity, unit_price_cents, created_at \
         FROM order_items \
         WHERE order_id = $1 \
         ORDER BY created_at",
    )
    .bind(order_id)
    .fetch_all(db)
    .await
}

/// Single atomic UPDATE: changes the status AND, if transitioning into
/// `paid`, stamps `paid_at` in the same statement. Replaces the older
/// split `update_status` + `set_paid_at` sequence that could leave the row
/// in an inconsistent `paid` + `paid_at = NULL` state on partial failure.
/// (In practice every order is already `paid` from creation — see
/// `create_order` — so this branch is now only relevant if a future status
/// ever needs `paid_at` semantics again; kept as-is since it's still
/// correct and harmless.)
pub async fn update_status_and_paid_at_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    status: &OrderStatus,
) -> Result<Option<Order>, sqlx::Error> {
    sqlx::query_as::<_, Order>(
        "UPDATE orders SET \
            status = $2::order_status, \
            paid_at = CASE \
                WHEN $2::order_status = 'paid' AND paid_at IS NULL THEN NOW() \
                ELSE paid_at \
            END, \
            updated_at = NOW() \
         WHERE id = $1 \
         RETURNING *",
    )
    .bind(id)
    .bind(status.as_str())
    .fetch_optional(&mut **tx)
    .await
}

// ---------------------------------------------------------------------------
// Idempotency table
// ---------------------------------------------------------------------------

/// Return the order id associated with a prior (user_id, key) pair, if any.
/// Used by the checkout flow to short-circuit duplicate retries.
pub async fn find_idempotency(
    db: &PgPool,
    user_id: Uuid,
    key: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT order_id FROM order_idempotency \
         WHERE user_id = $1 AND idempotency_key = $2",
    )
    .bind(user_id)
    .bind(key)
    .fetch_optional(db)
    .await
}

/// Insert the idempotency row inside the checkout tx so either both the
/// order and the key persist, or neither does.
pub async fn insert_idempotency_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    key: &str,
    order_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO order_idempotency (user_id, idempotency_key, order_id, created_at) \
         VALUES ($1, $2, $3, NOW())",
    )
    .bind(user_id)
    .bind(key)
    .bind(order_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
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
                o.status, o.total_cents, o.points_used, o.coupon_code, o.created_at, \
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
