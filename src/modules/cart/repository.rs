use sqlx::PgPool;
use uuid::Uuid;

use super::model::{CartItem, CartItemJoined, CheckoutLine};

pub async fn find_by_user(db: &PgPool, user_id: Uuid) -> Result<Vec<CartItemJoined>, sqlx::Error> {
    sqlx::query_as::<_, CartItemJoined>(
        "SELECT ci.id, ci.user_id, ci.item_type, ci.product_id, ci.course_id, ci.quantity, \
         COALESCE(p.name, c.name) AS name, COALESCE(p.slug, c.slug) AS slug, \
         COALESCE(p.price_cents, c.price_cents) AS price_cents, \
         ci.created_at, ci.updated_at \
         FROM cart_items ci \
         LEFT JOIN products p ON ci.product_id = p.id \
         LEFT JOIN courses c ON ci.course_id = c.id \
         WHERE ci.user_id = $1 \
         ORDER BY ci.created_at",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// Look up a single cart item by its own id, scoped to `user_id` so a caller
/// can never address (or leak the existence of) another user's row.
pub async fn find_item_by_id(
    db: &PgPool,
    user_id: Uuid,
    item_id: Uuid,
) -> Result<Option<CartItem>, sqlx::Error> {
    sqlx::query_as::<_, CartItem>(
        "SELECT id, user_id, item_type, product_id, course_id, quantity, created_at, updated_at \
         FROM cart_items WHERE id = $1 AND user_id = $2",
    )
    .bind(item_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
}

/// Add (or merge into) a product line. Repeat adds accumulate quantity via
/// `ON CONFLICT ... DO UPDATE`.
pub async fn add_product_item(
    db: &PgPool,
    user_id: Uuid,
    product_id: Uuid,
    quantity: i32,
) -> Result<CartItem, sqlx::Error> {
    sqlx::query_as::<_, CartItem>(
        "INSERT INTO cart_items (id, user_id, item_type, product_id, quantity, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, 'product'::cart_item_type, $2, $3, NOW(), NOW()) \
         ON CONFLICT (user_id, product_id) WHERE product_id IS NOT NULL \
         DO UPDATE SET quantity = cart_items.quantity + $3, updated_at = NOW() \
         RETURNING *",
    )
    .bind(user_id)
    .bind(product_id)
    .bind(quantity)
    .fetch_one(db)
    .await
}

/// Add a course line (quantity is always 1). Unlike products, a repeat add
/// of the same course does NOT merge — it is a no-op conflict, surfaced to
/// the caller as `None` so the service can return 409 "course already in
/// cart".
pub async fn add_course_item(
    db: &PgPool,
    user_id: Uuid,
    course_id: Uuid,
) -> Result<Option<CartItem>, sqlx::Error> {
    sqlx::query_as::<_, CartItem>(
        "INSERT INTO cart_items (id, user_id, item_type, course_id, quantity, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, 'course'::cart_item_type, $2, 1, NOW(), NOW()) \
         ON CONFLICT (user_id, course_id) WHERE course_id IS NOT NULL \
         DO NOTHING \
         RETURNING *",
    )
    .bind(user_id)
    .bind(course_id)
    .fetch_optional(db)
    .await
}

pub async fn update_quantity(
    db: &PgPool,
    user_id: Uuid,
    item_id: Uuid,
    quantity: i32,
) -> Result<Option<CartItem>, sqlx::Error> {
    sqlx::query_as::<_, CartItem>(
        "UPDATE cart_items SET quantity = $3, updated_at = NOW() \
         WHERE id = $1 AND user_id = $2 \
         RETURNING *",
    )
    .bind(item_id)
    .bind(user_id)
    .bind(quantity)
    .fetch_optional(db)
    .await
}

pub async fn remove_item(db: &PgPool, user_id: Uuid, item_id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM cart_items WHERE id = $1 AND user_id = $2")
        .bind(item_id)
        .bind(user_id)
        .execute(db)
        .await?;

    Ok(result.rows_affected() > 0)
}

pub async fn clear_cart(db: &PgPool, user_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM cart_items WHERE user_id = $1")
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn clear_cart_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM cart_items WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Transactional cart-for-checkout read. Locks the cart rows (`FOR UPDATE`)
/// and also the joined product/course rows (`FOR SHARE`) so another request
/// cannot concurrently mutate cart contents or change prices mid-checkout.
///
/// Product and course lines are fetched via two independent queries rather
/// than one `UNION`, because PostgreSQL rejects `FOR UPDATE`/`FOR SHARE` on
/// any branch of a set operation ("FOR UPDATE is not allowed with
/// UNION/INTERSECT/EXCEPT"). Each query preserves the original locking
/// shape (`FOR UPDATE OF ci`, `FOR SHARE OF` the priced table).
pub async fn find_cart_items_for_checkout_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> Result<Vec<CheckoutLine>, sqlx::Error> {
    let mut lines = sqlx::query_as::<_, CheckoutLine>(
        "SELECT 'product'::cart_item_type AS item_type, ci.product_id, NULL::uuid AS course_id, \
         ci.quantity, p.price_cents, p.name \
         FROM cart_items ci \
         JOIN products p ON ci.product_id = p.id \
         WHERE ci.user_id = $1 AND ci.item_type = 'product' AND p.is_active = true \
         ORDER BY ci.created_at \
         FOR UPDATE OF ci \
         FOR SHARE OF p",
    )
    .bind(user_id)
    .fetch_all(&mut **tx)
    .await?;

    let course_lines = sqlx::query_as::<_, CheckoutLine>(
        "SELECT 'course'::cart_item_type AS item_type, NULL::uuid AS product_id, ci.course_id, \
         ci.quantity, c.price_cents, c.name \
         FROM cart_items ci \
         JOIN courses c ON ci.course_id = c.id \
         WHERE ci.user_id = $1 AND ci.item_type = 'course' AND c.is_active = true \
         ORDER BY ci.created_at \
         FOR UPDATE OF ci \
         FOR SHARE OF c",
    )
    .bind(user_id)
    .fetch_all(&mut **tx)
    .await?;

    lines.extend(course_lines);
    Ok(lines)
}
