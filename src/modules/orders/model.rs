use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::modules::cart::model::CartItemType;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "order_status", rename_all = "snake_case")]
pub enum OrderStatus {
    Pending,
    Paid,
    Processing,
    Completed,
    Cancelled,
    Refunded,
}

impl OrderStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Paid => "paid",
            Self::Processing => "processing",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Refunded => "refunded",
        }
    }

    /// Allowed transitions. Terminal states (completed, cancelled, refunded)
    /// have no outgoing edges — once an order is refunded it cannot be
    /// shipped, once completed it cannot be reverted.
    pub fn can_transition_to(&self, next: &Self) -> bool {
        use OrderStatus::*;
        match (self, next) {
            (Pending, Paid) | (Pending, Cancelled) => true,
            (Paid, Processing) | (Paid, Refunded) | (Paid, Cancelled) => true,
            (Processing, Completed) | (Processing, Refunded) => true,
            (Completed, Refunded) => true,
            // Idempotent no-op: same-status updates are accepted so retries
            // of a webhook / admin action do not 422.
            (a, b) if a.as_str() == b.as_str() => true,
            _ => false,
        }
    }
}

impl std::str::FromStr for OrderStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "paid" => Ok(Self::Paid),
            "processing" => Ok(Self::Processing),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            "refunded" => Ok(Self::Refunded),
            _ => Err(()),
        }
    }
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Order {
    pub id: Uuid,
    pub user_id: Uuid,
    pub order_number: String,
    pub status: OrderStatus,
    pub total_cents: i64,
    pub discount_cents: i64,
    pub coupon_code: Option<String>,
    pub points_used: i64,
    pub points_earned: i64,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `order_items` row. Exactly one of `product_id`/`course_id` is set,
/// matching `item_type` (enforced by the `order_items_one_target` CHECK) —
/// mirrors `cart::model::CartItem`'s product/course dual-target shape, and
/// reuses the same `cart_item_type` Postgres enum since an order line is
/// just a cart line's frozen snapshot at checkout time.
#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct OrderItem {
    pub id: Uuid,
    pub order_id: Uuid,
    pub item_type: CartItemType,
    pub product_id: Option<Uuid>,
    pub course_id: Option<Uuid>,
    pub quantity: i32,
    pub unit_price_cents: i64,
    pub created_at: DateTime<Utc>,
}

/// `orders` JOINed with `users` for the two fields the admin order list
/// needs (`user_name`, `user_email`). Kept as its own flat row type (rather
/// than nesting an [`Order`] inside it) because sqlx's derived `FromRow`
/// maps one column per field and has no support for nested structs.
#[derive(Debug, sqlx::FromRow)]
pub struct AdminOrderRow {
    pub id: Uuid,
    pub order_number: String,
    pub user_name: String,
    pub user_email: String,
    pub status: OrderStatus,
    pub total_cents: i64,
    pub points_used: i64,
    pub coupon_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub items: sqlx::types::Json<Vec<OrderItemBrief>>,
}

/// `{ name, quantity }` — the minimal per-line summary surfaced by
/// `OrderSummary`/`AdminOrderSummary`'s `items` field. Decoded straight out
/// of a `jsonb_agg(...)` correlated-subquery aggregate (see
/// `repository::find_by_user` / `find_all_with_user`), so it needs
/// `Deserialize` in addition to the `Serialize` every other response type
/// needs. `name` is the `order_items.name` snapshot column (what the buyer
/// purchased at checkout time), never the live product/course catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderItemBrief {
    pub name: String,
    pub quantity: i32,
}

/// Row shape for `repository::find_by_user` — a slimmer projection of
/// `orders` than the full [`Order`] model, plus the aggregated `items`
/// brief. Kept separate from `Order` because most `Order` readers (checkout,
/// `get_order`, status transitions) don't want the extra per-row aggregate
/// subquery this requires.
#[derive(Debug, sqlx::FromRow)]
pub struct OrderSummaryRow {
    pub id: Uuid,
    pub order_number: String,
    pub status: OrderStatus,
    pub total_cents: i64,
    pub created_at: DateTime<Utc>,
    pub items: sqlx::types::Json<Vec<OrderItemBrief>>,
}
