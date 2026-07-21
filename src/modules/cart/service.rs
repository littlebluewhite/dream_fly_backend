use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;

use super::dto::CartResponse;
use super::model::{CartItemType, CheckoutLine};
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
    CartItemType::Product.validate_quantity(quantity)?;

    // Verify product exists and is active
    let product = crate::modules::products::repository::find_by_id(db, product_id)
        .await?
        .ok_or_else(|| AppError::NotFound("product not found".into()))?;

    // `quantity` here is this add's increment, not the cart's final total —
    // see `Product::ensure_purchasable`'s doc comment for the boundary with
    // `reserve_stock_tx`.
    product.ensure_purchasable(quantity)?;

    repository::add_product_item(db, user_id, product_id, quantity).await?;

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
    // deferred into `CartItemType::validate_quantity` below. The *semantic*
    // owner of this `1..=999` range is still `validate_quantity`'s `Product`
    // branch; this inline copy exists only to preserve error-code priority
    // (codex r2). Moving it entirely after the lookup would change observable
    // behavior: "qty out of range + item doesn't exist" would flip 400->404,
    // and "course qty outside 1..=999" would flip 400->422.
    if !(1..=999).contains(&quantity) {
        return Err(AppError::BadRequest(
            "quantity must be between 1 and 999".into(),
        ));
    }

    let item = repository::find_item_by_id(db, user_id, item_id)
        .await?
        .ok_or_else(|| AppError::NotFound("cart item not found".into()))?;

    match item.item_type {
        CartItemType::Course => {
            item.item_type.validate_quantity(quantity)?;
        }
        CartItemType::Product => {
            // Always `Ok` here — the inline 1..=999 guard above already
            // rejected out-of-range input — but this is still the semantic
            // owner's call site (`validate_quantity`'s `Product` arm).
            // Deliberately kept, not simplified away: removing it would
            // break the symmetry with the `Course` arm above.
            item.item_type.validate_quantity(quantity)?;

            // Re-check product active + stock on quantity updates; without
            // this, a user could ratchet a cart item past the available
            // stock after a restock/inactivation. `quantity` here is the
            // item's final value — see `Product::ensure_purchasable`'s doc
            // comment for the boundary with `reserve_stock_tx`.
            let product_id = item
                .product_id
                .ok_or_else(|| AppError::Validation("cart item missing product_id".into()))?;
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

/// Transactional cart-for-checkout read seam (ADR-0005 轉手層). Locks the
/// cart rows + priced product/course rows and returns the snapshot
/// `orders::service::checkout` prices and plans against — see
/// `repository::find_cart_items_for_checkout_tx` for the exact locking
/// shape. Strict pass-through with no error mapping, so checkout's error
/// contract stays exactly the repository's.
pub async fn find_cart_items_for_checkout_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Vec<CheckoutLine>, AppError> {
    Ok(repository::find_cart_items_for_checkout_tx(tx, user_id).await?)
}

/// Clear the cart inside the caller's transaction — checkout's step 11.
/// Distinct from the pool-based [`clear`] above (the `_tx` suffix marks the
/// transactional variant); the two coexist. Strict pass-through, no error
/// mapping.
pub async fn clear_cart_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<(), AppError> {
    Ok(repository::clear_cart_tx(tx, user_id).await?)
}
