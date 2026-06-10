use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::model::{Order, OrderItem};

#[derive(Debug, Serialize)]
pub struct OrderResponse {
    pub id: Uuid,
    pub order_number: String,
    pub status: String,
    pub total_cents: i64,
    pub discount_cents: i64,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub items: Vec<OrderItemResponse>,
}

#[derive(Debug, Serialize)]
pub struct OrderItemResponse {
    pub id: Uuid,
    pub product_id: Uuid,
    pub quantity: i32,
    pub unit_price_cents: i64,
}

impl OrderItemResponse {
    pub fn from_model(item: OrderItem) -> Self {
        Self {
            id: item.id,
            product_id: item.product_id,
            quantity: item.quantity,
            unit_price_cents: item.unit_price_cents,
        }
    }
}

impl OrderResponse {
    pub fn from_order_and_items(order: Order, items: Vec<OrderItem>) -> Self {
        Self {
            id: order.id,
            order_number: order.order_number,
            status: order.status.as_str().to_string(),
            total_cents: order.total_cents,
            discount_cents: order.discount_cents,
            paid_at: order.paid_at,
            created_at: order.created_at,
            items: items.into_iter().map(OrderItemResponse::from_model).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct OrderListResponse {
    pub orders: Vec<OrderSummary>,
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
}

#[derive(Debug, Serialize)]
pub struct OrderSummary {
    pub id: Uuid,
    pub order_number: String,
    pub status: String,
    pub total_cents: i64,
    pub created_at: DateTime<Utc>,
}

impl From<Order> for OrderSummary {
    fn from(o: Order) -> Self {
        Self {
            id: o.id,
            order_number: o.order_number,
            status: o.status.as_str().to_string(),
            total_cents: o.total_cents,
            created_at: o.created_at,
        }
    }
}

#[derive(Debug, Deserialize, validator::Validate)]
pub struct UpdateOrderStatusRequest {
    #[validate(length(min = 1, max = 32))]
    pub status: String,
}
