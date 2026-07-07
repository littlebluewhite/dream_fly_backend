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
}

impl Subscription {
    /// Read-time status derivation — see [`derive_status`].
    pub fn derived_status(&self) -> &'static str {
        derive_status(self.status, self.expires_at, self.remaining_sessions)
    }
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
}

impl SubscriptionWithProduct {
    /// Read-time status derivation — see [`derive_status`].
    pub fn derived_status(&self) -> &'static str {
        derive_status(self.status, self.expires_at, self.remaining_sessions)
    }
}

/// Single implementation of the read-time status derivation rule, shared by
/// [`Subscription::derived_status`] and [`SubscriptionWithProduct::derived_status`]
/// so the DTO layer never has to re-implement it. The stored `status` column
/// is never mutated by this — a subscription that has lapsed by date or run
/// out of sessions stays `active` in the database; this only affects what
/// gets serialized.
///
/// - `cancelled` (DB status) → `"cancelled"`.
/// - otherwise, expired by date (`expires_at` in the past) or by session
///   quota (`remaining_sessions == 0`) → `"expired"`.
/// - otherwise → `"active"`.
///
/// SQL-side twin: [`super::repository::redeem_one_session`]'s `WHERE` clause
/// encodes this same expiry/session-quota predicate for the atomic redeem
/// path; `tests/service_subscriptions.rs` guards the two staying in sync.
fn derive_status(
    status: SubscriptionStatus,
    expires_at: Option<DateTime<Utc>>,
    remaining_sessions: Option<i32>,
) -> &'static str {
    if status == SubscriptionStatus::Cancelled {
        return "cancelled";
    }
    let expired_by_date = expires_at.is_some_and(|exp| exp <= Utc::now());
    let expired_by_sessions = remaining_sessions == Some(0);
    if expired_by_date || expired_by_sessions {
        "expired"
    } else {
        "active"
    }
}
