use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct OrderItem {
    pub id: Uuid,
    pub order_id: Uuid,
    pub product_id: Uuid,
    pub quantity: i32,
    pub unit_price_cents: i64,
    pub created_at: DateTime<Utc>,
}
