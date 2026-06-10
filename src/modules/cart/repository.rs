use sqlx::PgPool;
use uuid::Uuid;

use super::model::{CartItem, CartItemWithProduct};

pub async fn find_by_user(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<CartItemWithProduct>, sqlx::Error> {
    sqlx::query_as::<_, CartItemWithProduct>(
        "SELECT ci.id, ci.user_id, ci.product_id, ci.quantity, \
         p.name AS product_name, p.slug AS product_slug, p.price_cents, \
         ci.created_at, ci.updated_at \
         FROM cart_items ci \
         JOIN products p ON ci.product_id = p.id \
         WHERE ci.user_id = $1 \
         ORDER BY ci.created_at",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

pub async fn add_item(
    db: &PgPool,
    user_id: Uuid,
    product_id: Uuid,
    quantity: i32,
) -> Result<CartItem, sqlx::Error> {
    sqlx::query_as::<_, CartItem>(
        "INSERT INTO cart_items (id, user_id, product_id, quantity, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, NOW(), NOW()) \
         ON CONFLICT (user_id, product_id) \
         DO UPDATE SET quantity = cart_items.quantity + $3, updated_at = NOW() \
         RETURNING *",
    )
    .bind(user_id)
    .bind(product_id)
    .bind(quantity)
    .fetch_one(db)
    .await
}

pub async fn update_quantity(
    db: &PgPool,
    user_id: Uuid,
    product_id: Uuid,
    quantity: i32,
) -> Result<Option<CartItem>, sqlx::Error> {
    sqlx::query_as::<_, CartItem>(
        "UPDATE cart_items SET quantity = $3, updated_at = NOW() \
         WHERE user_id = $1 AND product_id = $2 \
         RETURNING *",
    )
    .bind(user_id)
    .bind(product_id)
    .bind(quantity)
    .fetch_optional(db)
    .await
}

pub async fn remove_item(
    db: &PgPool,
    user_id: Uuid,
    product_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM cart_items WHERE user_id = $1 AND product_id = $2",
    )
    .bind(user_id)
    .bind(product_id)
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
/// and also the joined product rows (`FOR SHARE`) so another request cannot
/// concurrently mutate cart contents or change prices mid-checkout.
pub async fn find_cart_items_for_checkout_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> Result<Vec<CartItemWithProduct>, sqlx::Error> {
    sqlx::query_as::<_, CartItemWithProduct>(
        "SELECT ci.id, ci.user_id, ci.product_id, ci.quantity, \
         p.name AS product_name, p.slug AS product_slug, p.price_cents, \
         ci.created_at, ci.updated_at \
         FROM cart_items ci \
         JOIN products p ON ci.product_id = p.id \
         WHERE ci.user_id = $1 AND p.is_active = true \
         ORDER BY ci.created_at \
         FOR UPDATE OF ci \
         FOR SHARE OF p",
    )
    .bind(user_id)
    .fetch_all(&mut **tx)
    .await
}
