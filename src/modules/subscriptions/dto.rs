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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::subscriptions::model::SubscriptionStatus;

    /// Golden wire shape: `status` is the read-time `derived_status`, not the
    /// stored column.
    #[test]
    fn response_json_carries_derived_status() {
        let s = SubscriptionWithProduct {
            id: Uuid::nil(),
            product_id: Uuid::nil(),
            product_name: "p".into(),
            status: SubscriptionStatus::Active,
            started_at: DateTime::<Utc>::UNIX_EPOCH,
            expires_at: None,
            total_sessions: Some(10),
            remaining_sessions: Some(0),
            price_cents: 100,
            derived_status: SubscriptionStatus::Expired,
        };
        assert_eq!(
            serde_json::to_string(&SubscriptionResponse::from(s)).unwrap(),
            r#"{"id":"00000000-0000-0000-0000-000000000000","product_id":"00000000-0000-0000-0000-000000000000","product_name":"p","status":"expired","started_at":"1970-01-01T00:00:00Z","expires_at":null,"total_sessions":10,"remaining_sessions":0,"price_cents":100}"#
        );
    }
}
