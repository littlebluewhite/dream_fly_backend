use sqlx::PgPool;
use uuid::Uuid;

use super::model::{Order, OrderItem, OrderStatus};

pub async fn create_order(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    order_number: &str,
    total_cents: i64,
) -> Result<Order, sqlx::Error> {
    sqlx::query_as::<_, Order>(
        "INSERT INTO orders (id, user_id, order_number, status, total_cents, \
         discount_cents, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, 'pending'::order_status, $3, 0, NOW(), NOW()) \
         RETURNING *",
    )
    .bind(user_id)
    .bind(order_number)
    .bind(total_cents)
    .fetch_one(&mut **tx)
    .await
}

pub async fn create_order_items(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    order_id: Uuid,
    items: &[(Uuid, i32, i64)],
) -> Result<Vec<OrderItem>, sqlx::Error> {
    let len = items.len();
    let mut ids: Vec<Uuid> = Vec::with_capacity(len);
    let mut product_ids: Vec<Uuid> = Vec::with_capacity(len);
    let mut quantities: Vec<i32> = Vec::with_capacity(len);
    let mut prices: Vec<i64> = Vec::with_capacity(len);

    for (product_id, quantity, unit_price_cents) in items {
        ids.push(Uuid::now_v7());
        product_ids.push(*product_id);
        quantities.push(*quantity);
        prices.push(*unit_price_cents);
    }

    sqlx::query_as::<_, OrderItem>(
        "INSERT INTO order_items (id, order_id, product_id, quantity, unit_price_cents, created_at) \
         SELECT unnest($1::uuid[]), $2, unnest($3::uuid[]), unnest($4::int[]), unnest($5::bigint[]), NOW() \
         RETURNING *",
    )
    .bind(&ids)
    .bind(order_id)
    .bind(&product_ids)
    .bind(&quantities)
    .bind(&prices)
    .fetch_all(&mut **tx)
    .await
}

pub async fn find_by_id(db: &PgPool, id: Uuid) -> Result<Option<Order>, sqlx::Error> {
    sqlx::query_as::<_, Order>(
        "SELECT id, user_id, order_number, status, total_cents, discount_cents, \
         paid_at, created_at, updated_at \
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
         paid_at, created_at, updated_at \
         FROM orders WHERE id = $1 \
         FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
}

pub async fn find_by_user(
    db: &PgPool,
    user_id: Uuid,
    limit: u32,
    offset: u32,
) -> Result<Vec<Order>, sqlx::Error> {
    sqlx::query_as::<_, Order>(
        "SELECT id, user_id, order_number, status, total_cents, discount_cents, \
         paid_at, created_at, updated_at \
         FROM orders \
         WHERE user_id = $1 \
         ORDER BY created_at DESC \
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
        "SELECT id, order_id, product_id, quantity, unit_price_cents, created_at \
         FROM order_items \
         WHERE order_id = $1 \
         ORDER BY created_at",
    )
    .bind(order_id)
    .fetch_all(db)
    .await
}

pub async fn find_items_by_order_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    order_id: Uuid,
) -> Result<Vec<OrderItem>, sqlx::Error> {
    sqlx::query_as::<_, OrderItem>(
        "SELECT id, order_id, product_id, quantity, unit_price_cents, created_at \
         FROM order_items \
         WHERE order_id = $1 \
         ORDER BY created_at",
    )
    .bind(order_id)
    .fetch_all(&mut **tx)
    .await
}

/// Single atomic UPDATE: changes the status AND, if transitioning into
/// `paid`, stamps `paid_at` in the same statement. Replaces the older
/// split `update_status` + `set_paid_at` sequence that could leave the row
/// in an inconsistent `paid` + `paid_at = NULL` state on partial failure.
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
