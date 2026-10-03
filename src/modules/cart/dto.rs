use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use crate::error::AppError;
use crate::modules::products::model::MAX_LINE_QUANTITY;

use super::model::{CartItemJoined, CartItemType, LineTarget, checked_line_subtotal};

#[derive(Debug, Serialize, ts_rs::TS)]
pub struct CartItemResponse {
    pub id: Uuid,
    pub item_type: CartItemType,
    pub item_id: Uuid,
    pub name: String,
    pub slug: String,
    pub quantity: i32,
    pub unit_price_cents: i64,
    pub subtotal_cents: i64,
    pub is_active: bool,
}

#[derive(Debug, Serialize, ts_rs::TS)]
pub struct CartResponse {
    pub items: Vec<CartItemResponse>,
    pub total_cents: i64,
}

impl CartResponse {
    /// Build a cart response from repository rows. Uses `checked_mul` so a
    /// malicious or corrupted quantity cannot silently wrap the subtotal.
    pub fn from_items(items: Vec<CartItemJoined>) -> Result<Self, AppError> {
        let mut cart_items = Vec::with_capacity(items.len());
        let mut total_cents: i64 = 0;

        for item in items {
            let item_id = match item.target {
                LineTarget::Product(id) | LineTarget::Course(id) => id,
            };
            let subtotal = checked_line_subtotal(item.price_cents, item.quantity)
                .ok_or_else(|| AppError::Validation("cart subtotal overflow".into()))?;
            total_cents = total_cents
                .checked_add(subtotal)
                .ok_or_else(|| AppError::Validation("cart total overflow".into()))?;
            cart_items.push(CartItemResponse {
                id: item.id,
                item_type: item.target.item_type(),
                item_id,
                name: item.name,
                slug: item.slug,
                quantity: item.quantity,
                unit_price_cents: item.price_cents,
                subtotal_cents: subtotal,
                is_active: item.is_active,
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
    /// "product" | "course" — parsed into `CartItemType` by the service,
    /// which returns `AppError::Validation` for any other value.
    pub item_type: String,
    pub item_id: Uuid,
    #[validate(range(min = 1, max = MAX_LINE_QUANTITY))]
    pub quantity: Option<i32>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct UpdateCartItemRequest {
    #[validate(range(min = 1, max = MAX_LINE_QUANTITY))]
    pub quantity: i32,
}
