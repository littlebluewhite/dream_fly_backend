use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "subscription_status", rename_all = "snake_case")]
pub enum SubscriptionStatus {
    Active,
    Expired,
    Cancelled,
}

impl SubscriptionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Bare `subscriptions` table row. This is what `grant_from_purchase_tx`
/// returns and what the atomic redeem `UPDATE ... RETURNING *` produces —
/// it has no `product_name` since those call sites either already hold the
/// `Product` (grant) or don't need the name (the redeem decrement itself).
#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Subscription {
    pub id: Uuid,
    pub user_id: Uuid,
    pub product_id: Uuid,
    pub order_id: Option<Uuid>,
    pub status: SubscriptionStatus,
    pub started_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub total_sessions: Option<i32>,
    pub remaining_sessions: Option<i32>,
    pub price_cents: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Read-time status, computed by the `subscription_derived_status` SQL
    /// function from (`status`, `expires_at`, `remaining_sessions`) at query
    /// time — never persisted, never written back to the `status` column.
    pub derived_status: SubscriptionStatus,
}

/// `subscriptions` JOINed with `products` for the one extra field
/// (`product_name`) every response needs. Kept as its own flat row type
/// (rather than nesting a [`Subscription`] inside it) because sqlx's derived
/// `FromRow` maps one column per field and has no support for nested
/// structs.
#[derive(Debug, sqlx::FromRow)]
pub struct SubscriptionWithProduct {
    pub id: Uuid,
    pub product_id: Uuid,
    pub product_name: String,
    pub status: SubscriptionStatus,
    pub started_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub total_sessions: Option<i32>,
    pub remaining_sessions: Option<i32>,
    pub price_cents: i64,
    /// Read-time status, computed by the `subscription_derived_status` SQL
    /// function — never persisted.
    pub derived_status: SubscriptionStatus,
}
