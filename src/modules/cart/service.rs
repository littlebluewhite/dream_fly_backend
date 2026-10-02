use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::modules::points::service::BalanceLock;

use super::dto::CartResponse;
use super::model::{CartItemType, CheckoutLine, CheckoutTargets, LineTarget};
use super::repository;

pub async fn get_cart(db: &PgPool, user_id: Uuid) -> Result<CartResponse, AppError> {
    let items = repository::find_by_user(db, user_id).await?;
    CartResponse::from_items(items)
}

pub async fn add_item(
    db: &PgPool,
    user_id: Uuid,
    item_type: &str,
    item_id: Uuid,
    quantity: i32,
) -> Result<CartResponse, AppError> {
    let item_type: CartItemType = item_type
        .parse()
        .map_err(|_| AppError::Validation(format!("invalid item_type: {item_type}")))?;

    match item_type {
        CartItemType::Product => add_product_item(db, user_id, item_id, quantity).await,
        CartItemType::Course => add_course_item(db, user_id, item_id, quantity).await,
    }
}

async fn add_product_item(
    db: &PgPool,
    user_id: Uuid,
    product_id: Uuid,
    quantity: i32,
) -> Result<CartResponse, AppError> {
    // The increment's own range, so a bad request is 400 before the
    // product lookup's 404.
    CartItemType::Product.validate_quantity(quantity)?;

    let product = crate::modules::products::repository::find_by_id(db, product_id)
        .await?
        .ok_or_else(|| AppError::NotFound("product not found".into()))?;

    // Merge first, then judge the merged line (active, line quantity,
    // stock — `Product::ensure_purchasable`); a rejection drops the tx,
    // rolling the merge back. The upsert's row lock serializes concurrent
    // adds to the same line, so two adds can't each pass against the
    // pre-merge quantity.
    let mut tx = db.begin().await?;
    let merged = repository::add_product_item_tx(&mut tx, user_id, product_id, quantity).await?;
    product.ensure_purchasable(merged.quantity)?;
    tx.commit().await?;

    get_cart(db, user_id).await
}

async fn add_course_item(
    db: &PgPool,
    user_id: Uuid,
    course_id: Uuid,
    quantity: i32,
) -> Result<CartResponse, AppError> {
    CartItemType::Course.validate_quantity(quantity)?;

    // Verify course exists and is active
    let course = crate::modules::courses::repository::find_by_id(db, course_id)
        .await?
        .ok_or_else(|| AppError::NotFound("course not found".into()))?;

    if !course.is_active {
        return Err(AppError::BadRequest("course is not available".into()));
    }

    let inserted = repository::add_course_item(db, user_id, course_id).await?;
    if inserted.is_none() {
        return Err(AppError::Conflict("course already in cart".into()));
    }

    get_cart(db, user_id).await
}

pub async fn update_quantity(
    db: &PgPool,
    user_id: Uuid,
    item_id: Uuid,
    quantity: i32,
) -> Result<CartResponse, AppError> {
    // Wire-compat guard — kept in place ahead of the item lookup, not
    // deferred into `CartItemType::validate_quantity` below. A product
    // line's legal quantity is owned by `Product::ensure_line_quantity`
    // (reached via `ensure_purchasable` below); this inline copy exists
    // only to preserve error-code priority (codex r2). Moving it entirely
    // after the lookup would change observable behavior: "qty out of range
    // + item doesn't exist" would flip 400->404, and "course qty outside
    // 1..=999" would flip 400->422.
    if !(1..=999).contains(&quantity) {
        return Err(AppError::BadRequest(
            "quantity must be between 1 and 999".into(),
        ));
    }

    let item = repository::find_item_by_id(db, user_id, item_id)
        .await?
        .ok_or_else(|| AppError::NotFound("cart item not found".into()))?;

    match item.target {
        LineTarget::Course(_) => {
            CartItemType::Course.validate_quantity(quantity)?;
        }
        LineTarget::Product(product_id) => {
            // Re-check the product on quantity updates (active, line
            // quantity incl. the time-based rule, stock); without this, a
            // user could ratchet a cart item past the available stock after
            // a restock/inactivation. `quantity` is the item's final value,
            // the same thing `add_product_item` judges after its merge.
            let product = crate::modules::products::repository::find_by_id(db, product_id)
                .await?
                .ok_or_else(|| AppError::NotFound("product not found".into()))?;
            product.ensure_purchasable(quantity)?;
        }
    }

    repository::update_quantity(db, user_id, item_id, quantity)
        .await?
        .ok_or_else(|| AppError::NotFound("cart item not found".into()))?;

    get_cart(db, user_id).await
}

pub async fn remove_item(db: &PgPool, user_id: Uuid, item_id: Uuid) -> Result<CartResponse, AppError> {
    let removed = repository::remove_item(db, user_id, item_id).await?;
    if !removed {
        return Err(AppError::NotFound("cart item not found".into()));
    }

    get_cart(db, user_id).await
}

pub async fn clear(db: &PgPool, user_id: Uuid) -> Result<(), AppError> {
    repository::clear_cart(db, user_id).await?;
    Ok(())
}

/// The ids this user's cart targets — step two of the order lock protocol
/// (`orders::locks::acquire_checkout_locks`), read right after the
/// `BalanceLock` is taken and before those ids are locked. Strict
/// pass-through (ADR-0005 轉手層); why no lock of its own is needed lives on
/// `repository::find_checkout_targets_tx`. Takes `&BalanceLock` for the same
/// pairing reason as [`find_cart_items_for_checkout_tx`] below.
pub async fn find_checkout_targets_tx(
    tx: &mut Transaction<'_, Postgres>,
    lock: &BalanceLock,
) -> Result<CheckoutTargets, AppError> {
    Ok(repository::find_checkout_targets_tx(tx, lock.user_id()).await?)
}

/// Transactional cart-for-checkout read seam (ADR-0005 轉手層). Locks the
/// cart rows and returns the snapshot `orders::service::checkout` prices
/// and plans against; the priced product/course rows must already be locked
/// by the order lock protocol (`orders::locks`) — see
/// `repository::find_cart_items_for_checkout_tx` for the exact locking
/// shape. Strict pass-through with no error mapping, so checkout's error
/// contract stays exactly the repository's.
///
/// The `is_active` MUST clause (same requirement as the repository layer
/// below) lives solely on `repository::find_cart_items_for_checkout_tx`'s
/// doc comment — see that function, not repeated here.
///
/// Takes `&BalanceLock` (`points::service`) rather than a bare `user_id` —
/// the caller must already hold that user's points-balance row lock (the
/// user-first half of ADR-0007 決策 5) before reading their cart, and the
/// `user_id` used below comes from the lock itself (`lock.user_id()`), so
/// locking one user's balance and reading a different user's cart cannot
/// type-check.
pub async fn find_cart_items_for_checkout_tx(
    tx: &mut Transaction<'_, Postgres>,
    lock: &BalanceLock,
) -> Result<Vec<CheckoutLine>, AppError> {
    Ok(repository::find_cart_items_for_checkout_tx(tx, lock.user_id()).await?)
}

/// Clear the cart inside the caller's transaction — `orders::service::checkout`.
/// Distinct from the pool-based [`clear`] above (the `_tx` suffix marks the
/// transactional variant); the two coexist. Strict pass-through, no error
/// mapping.
pub async fn clear_cart_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<(), AppError> {
    Ok(repository::clear_cart_tx(tx, user_id).await?)
}
