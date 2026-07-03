use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::model::SubscriptionWithProduct;

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
        let status = s.derived_status().to_string();
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
