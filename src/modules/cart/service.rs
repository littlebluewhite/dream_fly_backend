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
    if !(1..=999).contains(&quantity) {
        return Err(AppError::BadRequest(
            "quantity must be between 1 and 999".into(),
        ));
    }

    // Verify product exists and is active
    let product = crate::modules::products::repository::find_by_id(db, product_id)
        .await?
        .ok_or_else(|| AppError::NotFound("product not found".into()))?;

    if !product.is_active {
        return Err(AppError::BadRequest("product is not available".into()));
    }

    // Lightweight stock check: if the product tracks stock, reject
    // additions that obviously exceed it. The authoritative decrement still
    // happens at checkout inside the transaction.
    if let Some(stock) = product.stock {
        if quantity > stock {
            return Err(AppError::Conflict(format!(
                "insufficient stock: only {stock} available"
            )));
        }
    }

    repository::add_product_item(db, user_id, product_id, quantity).await?;

    get_cart(db, user_id).await
}

async fn add_course_item(
    db: &PgPool,
    user_id: Uuid,
    course_id: Uuid,
    quantity: i32,
) -> Result<CartResponse, AppError> {
    if quantity != 1 {
        return Err(AppError::Validation("course quantity must be 1".into()));
    }

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
            if quantity != 1 {
                return Err(AppError::Validation("course quantity must be 1".into()));
            }
        }
        CartItemType::Product => {
            // Re-check product active + stock on quantity updates; without
            // this, a user could ratchet a cart item past the available
            // stock after a restock/inactivation.
            let product_id = item
                .product_id
                .ok_or_else(|| AppError::Validation("cart item missing product_id".into()))?;
            let product = crate::modules::products::repository::find_by_id(db, product_id)
                .await?
                .ok_or_else(|| AppError::NotFound("product not found".into()))?;
            if !product.is_active {
                return Err(AppError::BadRequest("product is not available".into()));
            }
            if let Some(stock) = product.stock {
                if quantity > stock {
                    return Err(AppError::Conflict(format!(
                        "insufficient stock: only {stock} available"
                    )));
                }
            }
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
