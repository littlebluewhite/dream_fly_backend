use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::model::PointLedgerEntry;

#[derive(Debug, Serialize)]
pub struct LedgerEntryResponse {
    pub id: Uuid,
    pub delta: i64,
    pub balance_after: i64,
    pub reason: String,
    pub order_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl From<PointLedgerEntry> for LedgerEntryResponse {
    fn from(e: PointLedgerEntry) -> Self {
        Self {
            id: e.id,
            delta: e.delta,
            balance_after: e.balance_after,
            reason: e.reason.as_str().to_string(),
            order_id: e.order_id,
            created_at: e.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PointsMeResponse {
    pub balance: i64,
    pub ledger: Vec<LedgerEntryResponse>,
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
}
