use sqlx::PgPool;
use uuid::Uuid;

use super::model::{CartItem, CartItemJoined, CheckoutLine, CheckoutTargets};

pub async fn find_by_user(db: &PgPool, user_id: Uuid) -> Result<Vec<CartItemJoined>, sqlx::Error> {
    sqlx::query_as::<_, CartItemJoined>(
        "SELECT ci.id, ci.user_id, ci.item_type, ci.product_id, ci.course_id, ci.quantity, \
         COALESCE(p.name, c.name) AS name, COALESCE(p.slug, c.slug) AS slug, \
         COALESCE(p.price_cents, c.price_cents) AS price_cents, \
         COALESCE(p.is_active, c.is_active) AS is_active, \
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

/// Add (or merge into) a product line inside the caller's transaction.
/// Repeat adds accumulate quantity via `ON CONFLICT ... DO UPDATE`; the
/// returned row carries the *merged* quantity, which the caller judges
/// before committing. The upsert leaves the line's row locked until the
/// transaction ends, so concurrent adds to the same line queue here and
/// each sees the other's committed total.
pub async fn add_product_item_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
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
    .fetch_one(&mut **tx)
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

/// The product/course ids this user's cart targets — the order lock
/// protocol's (`orders::locks::acquire_checkout_locks`) input for which rows
/// to lock. Takes no lock of its own: it runs after the caller's
/// `lock_balance_tx` `FOR UPDATE` on the `users` row, and every new
/// `cart_items` row needs a `FOR KEY SHARE` on that same row for its
/// `user_id` FK check, so no line can be added to this cart until the
/// caller's transaction ends (pinned by
/// `checkout_locks_block_same_user_cart_insert_until_commit`). A concurrent
/// removal can only shrink the cart, so the later snapshot read is always a
/// subset of these targets. Deliberately not filtered by `is_active`
/// (甲案), exactly like the snapshot read below: a deactivated line is still
/// read (so `fulfilment::ensure_all_purchasable` can name it), so its row is
/// still locked.
pub async fn find_checkout_targets_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> Result<CheckoutTargets, sqlx::Error> {
    sqlx::query_as::<_, CheckoutTargets>(
        "SELECT \
         COALESCE(array_agg(product_id) FILTER (WHERE product_id IS NOT NULL), '{}') AS product_ids, \
         COALESCE(array_agg(course_id) FILTER (WHERE course_id IS NOT NULL), '{}') AS course_ids \
         FROM cart_items WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await
}

/// Transactional cart-for-checkout read. Locks the cart rows (`FOR UPDATE
/// OF ci`) so another request cannot concurrently mutate cart contents
/// mid-checkout. The joined product/course rows are NOT locked here: the
/// caller must already hold them at UPDATE strength via the order lock
/// protocol (`orders::locks::acquire_checkout_locks` — products `FOR NO KEY
/// UPDATE`, courses `FOR UPDATE`, both ascending), which is what keeps
/// prices from changing mid-checkout.
///
/// Product and course lines are fetched via two independent queries rather
/// than one `UNION`, because PostgreSQL rejects `FOR UPDATE`/`FOR SHARE` on
/// any branch of a set operation ("FOR UPDATE is not allowed with
/// UNION/INTERSECT/EXCEPT").
///
/// Returned lines are NOT filtered by `is_active` — every line the cart
/// references comes back, active or not, with `is_active` riding along on
/// each row, so a deactivated line can be named rather than just silently
/// missing. The caller MUST run the result through
/// `orders::fulfilment::ensure_all_purchasable` before treating any line as
/// purchasable; nothing at this layer enforces that.
pub async fn find_cart_items_for_checkout_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> Result<Vec<CheckoutLine>, sqlx::Error> {
    let mut lines = sqlx::query_as::<_, CheckoutLine>(
        "SELECT 'product'::cart_item_type AS item_type, ci.product_id, NULL::uuid AS course_id, \
         ci.quantity, p.price_cents, p.name, p.is_active \
         FROM cart_items ci \
         JOIN products p ON ci.product_id = p.id \
         WHERE ci.user_id = $1 AND ci.item_type = 'product' \
         ORDER BY ci.created_at \
         FOR UPDATE OF ci",
    )
    .bind(user_id)
    .fetch_all(&mut **tx)
    .await?;

    let course_lines = sqlx::query_as::<_, CheckoutLine>(
        "SELECT 'course'::cart_item_type AS item_type, NULL::uuid AS product_id, ci.course_id, \
         ci.quantity, c.price_cents, c.name, c.is_active \
         FROM cart_items ci \
         JOIN courses c ON ci.course_id = c.id \
         WHERE ci.user_id = $1 AND ci.item_type = 'course' \
         ORDER BY ci.created_at \
         FOR UPDATE OF ci",
    )
    .bind(user_id)
    .fetch_all(&mut **tx)
    .await?;

    lines.extend(course_lines);
    Ok(lines)
}
