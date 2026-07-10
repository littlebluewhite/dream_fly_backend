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

/// 計入營收的訂單狀態(reports 的營收彙總用)。「哪些狀態算營收」的單一
/// 歸屬點——改這裡,報表跟著變;`products::repository::find_sold_counts`
/// (售出件數用)直接綁定本常數一併跟著變,不再是另一份手抄的攣生清單。
pub const REVENUE_STATUSES: [&str; 3] = ["paid", "processing", "completed"];

/// 付款方式值域(應用層,非 DB enum——`orders.payment_method` 只是
/// `VARCHAR(30)`,Round 4 Task P4-B1)。`service::checkout` 缺省時預設
/// `credit_card`(向後相容既有不帶此欄的呼叫者);不在此集合內的值回 422。
/// Round 4 Phase 4 報表依此欄分組付款方式。
pub const PAYMENT_METHODS: [&str; 5] = ["credit_card", "line_pay", "atm", "jkopay", "cash"];

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
    /// Nullable — orders created before this column existed have `NULL`.
    /// Every order created by `service::checkout` from here on always has
    /// a value (defaulted to `credit_card` when the request omits it).
    pub payment_method: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_transition_pending_to_paid_is_legal() {
        assert!(OrderStatus::Pending.can_transition_to(&OrderStatus::Paid));
    }

    #[test]
    fn can_transition_pending_to_processing_is_illegal() {
        // Pending only opens onto Paid/Cancelled — Processing must be
        // reached via Paid first.
        assert!(!OrderStatus::Pending.can_transition_to(&OrderStatus::Processing));
    }

    #[test]
    fn can_transition_completed_to_paid_is_illegal() {
        // Completed only opens onto Refunded (plus the same-state case
        // below) — it can never revert to an earlier status.
        assert!(!OrderStatus::Completed.can_transition_to(&OrderStatus::Paid));
    }

    #[test]
    fn can_transition_same_state_is_legal_for_every_status() {
        // Idempotent no-op: a retried webhook/admin action re-applying the
        // current status must not 422 — covers every variant, not just one.
        for status in [
            OrderStatus::Pending,
            OrderStatus::Paid,
            OrderStatus::Processing,
            OrderStatus::Completed,
            OrderStatus::Cancelled,
            OrderStatus::Refunded,
        ] {
            let same = status.clone();
            assert!(
                status.can_transition_to(&same),
                "{status:?} -> itself should be legal"
            );
        }
    }
}
