use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use crate::error::AppError;

use super::model::CartItemWithProduct;

#[derive(Debug, Serialize)]
pub struct CartItemResponse {
    pub id: Uuid,
    pub product_id: Uuid,
    pub product_name: String,
    pub product_slug: String,
    pub quantity: i32,
    pub unit_price_cents: i64,
    pub subtotal_cents: i64,
}

#[derive(Debug, Serialize)]
pub struct CartResponse {
    pub items: Vec<CartItemResponse>,
    pub total_cents: i64,
}

impl CartResponse {
    /// Build a cart response from repository rows. Uses `checked_mul` so a
    /// malicious or corrupted quantity cannot silently wrap the subtotal.
    pub fn from_items(items: Vec<CartItemWithProduct>) -> Result<Self, AppError> {
        let mut cart_items = Vec::with_capacity(items.len());
        let mut total_cents: i64 = 0;

        for item in items {
            let subtotal = item
                .price_cents
                .checked_mul(i64::from(item.quantity))
                .ok_or_else(|| AppError::Validation("cart subtotal overflow".into()))?;
            total_cents = total_cents
                .checked_add(subtotal)
                .ok_or_else(|| AppError::Validation("cart total overflow".into()))?;
            cart_items.push(CartItemResponse {
                id: item.id,
                product_id: item.product_id,
                product_name: item.product_name,
                product_slug: item.product_slug,
                quantity: item.quantity,
                unit_price_cents: item.price_cents,
                subtotal_cents: subtotal,
            });
        }

        Ok(Self {
            items: cart_items,
            total_cents,
        })
    }
}

#[derive(Debug, Deserialize, Validate)]
pub struct AddCartItemRequest {
    pub product_id: Uuid,
    #[validate(range(min = 1, max = 999))]
    pub quantity: Option<i32>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct UpdateCartItemRequest {
    #[validate(range(min = 1, max = 999))]
    pub quantity: i32,
}
