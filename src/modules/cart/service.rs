use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;

use super::dto::CartResponse;
use super::repository;

pub async fn get_cart(db: &PgPool, user_id: Uuid) -> Result<CartResponse, AppError> {
    let items = repository::find_by_user(db, user_id).await?;
    CartResponse::from_items(items)
}

pub async fn add_item(
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

    repository::add_item(db, user_id, product_id, quantity).await?;

    get_cart(db, user_id).await
}

pub async fn update_quantity(
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

    // Re-check product active + stock on quantity updates; without this, a
    // user could ratchet a cart item past the available stock after a
    // restock/inactivation.
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

    repository::update_quantity(db, user_id, product_id, quantity)
        .await?
        .ok_or_else(|| AppError::NotFound("cart item not found".into()))?;

    get_cart(db, user_id).await
}

pub async fn remove_item(
    db: &PgPool,
    user_id: Uuid,
    product_id: Uuid,
) -> Result<CartResponse, AppError> {
    let removed = repository::remove_item(db, user_id, product_id).await?;
    if !removed {
        return Err(AppError::NotFound("cart item not found".into()));
    }

    get_cart(db, user_id).await
}

pub async fn clear(db: &PgPool, user_id: Uuid) -> Result<(), AppError> {
    repository::clear_cart(db, user_id).await?;
    Ok(())
}
