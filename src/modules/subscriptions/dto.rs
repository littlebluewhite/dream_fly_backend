use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::model::{Subscription, SubscriptionWithProduct};

#[derive(Debug, Serialize)]
pub struct SubscriptionResponse {
    pub id: Uuid,
    pub product_id: Uuid,
    pub product_name: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub total_sessions: Option<i32>,
    pub remaining_sessions: Option<i32>,
    pub price_cents: i64,
}

impl From<SubscriptionWithProduct> for SubscriptionResponse {
    fn from(s: SubscriptionWithProduct) -> Self {
        let status = s.derived_status.as_str().to_string();
        Self {
            id: s.id,
            product_id: s.product_id,
            product_name: s.product_name,
            status,
            started_at: s.started_at,
            expires_at: s.expires_at,
            total_sessions: s.total_sessions,
            remaining_sessions: s.remaining_sessions,
            price_cents: s.price_cents,
        }
    }
}

impl SubscriptionResponse {
    /// Build from a bare subscription row plus a separately-fetched product
    /// name. Used by the redeem path, which must serialize the exact row its
    /// atomic `UPDATE ... RETURNING` produced — re-reading the subscription
    /// could observe a concurrent redeem's later decrement and misreport
    /// what this call consumed.
    pub fn from_subscription(s: Subscription, product_name: String) -> Self {
        let status = s.derived_status.as_str().to_string();
        Self {
            id: s.id,
            product_id: s.product_id,
            product_name,
            status,
            started_at: s.started_at,
            expires_at: s.expires_at,
            total_sessions: s.total_sessions,
            remaining_sessions: s.remaining_sessions,
            price_cents: s.price_cents,
        }
    }
}
