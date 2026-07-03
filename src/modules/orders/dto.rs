use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use crate::modules::enrolments::dto::EnrolmentResponse;
use crate::modules::subscriptions::dto::SubscriptionResponse;

use super::model::{AdminOrderRow, Order, OrderItem};

/// `POST /orders` body. Every field is optional: an empty body (or `{}`)
/// checks out the cart at full price with no points redeemed — see
/// `handlers::checkout` for how a genuinely-empty request body (no
/// `Content-Type` at all) is handled.
#[derive(Debug, Default, Deserialize)]
pub struct CheckoutRequest {
    pub coupon_code: Option<String>,
    pub use_points: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct OrderResponse {
    pub id: Uuid,
    pub order_number: String,
    pub status: String,
    pub total_cents: i64,
    pub discount_cents: i64,
    pub coupon_code: Option<String>,
    pub points_used: i64,
    pub points_earned: i64,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub items: Vec<OrderItemResponse>,
    pub enrolments: Vec<EnrolmentResponse>,
    pub subscriptions: Vec<SubscriptionResponse>,
}

#[derive(Debug, Serialize)]
pub struct OrderItemResponse {
    pub id: Uuid,
    pub item_type: String,
    pub product_id: Option<Uuid>,
    pub course_id: Option<Uuid>,
    pub quantity: i32,
    pub unit_price_cents: i64,
}

impl OrderItemResponse {
    pub fn from_model(item: OrderItem) -> Self {
        Self {
            id: item.id,
            item_type: item.item_type.as_str().to_string(),
            product_id: item.product_id,
            course_id: item.course_id,
            quantity: item.quantity,
            unit_price_cents: item.unit_price_cents,
        }
    }
}

impl OrderResponse {
    /// Assemble the full response from the order row, its line items, and
    /// the artifacts (enrolments/subscriptions) checkout produced. Callers
    /// look artifacts up by `order_id`, so this same assembly is used for a
    /// fresh checkout, an idempotent replay, and a plain `GET /orders/{id}`
    /// — all three must present identical shapes.
    pub fn assemble(
        order: Order,
        items: Vec<OrderItem>,
        enrolments: Vec<EnrolmentResponse>,
        subscriptions: Vec<SubscriptionResponse>,
    ) -> Self {
        Self {
            id: order.id,
            order_number: order.order_number,
            status: order.status.as_str().to_string(),
            total_cents: order.total_cents,
            discount_cents: order.discount_cents,
            coupon_code: order.coupon_code,
            points_used: order.points_used,
            points_earned: order.points_earned,
            paid_at: order.paid_at,
            created_at: order.created_at,
            items: items
                .into_iter()
                .map(OrderItemResponse::from_model)
                .collect(),
            enrolments,
            subscriptions,
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

#[derive(Debug, Deserialize, Validate)]
pub struct UpdateOrderStatusRequest {
    #[validate(length(min = 1, max = 32))]
    pub status: String,
}

// ---------------------------------------------------------------------------
// Admin order list — GET /orders (admin only)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct AdminOrderSummary {
    pub id: Uuid,
    pub order_number: String,
    pub user_name: String,
    pub user_email: String,
    pub status: String,
    pub total_cents: i64,
    pub points_used: i64,
    pub coupon_code: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<AdminOrderRow> for AdminOrderSummary {
    fn from(o: AdminOrderRow) -> Self {
        Self {
            id: o.id,
            order_number: o.order_number,
            user_name: o.user_name,
            user_email: o.user_email,
            status: o.status.as_str().to_string(),
            total_cents: o.total_cents,
            points_used: o.points_used,
            coupon_code: o.coupon_code,
            created_at: o.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AdminOrderListResponse {
    pub orders: Vec<AdminOrderSummary>,
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
}
