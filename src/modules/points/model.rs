use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "point_reason", rename_all = "snake_case")]
pub enum PointReason {
    CheckoutEarn,
    CheckoutRedeem,
    AdminAdjust,
    /// Points spent redeeming a `rewards` catalog item (Round 3 Task 6, 裁決
    /// 7) — added via migration `20260707000004_point_reason_add_redeem.sql`.
    Redeem,
}

impl PointReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CheckoutEarn => "checkout_earn",
            Self::CheckoutRedeem => "checkout_redeem",
            Self::AdminAdjust => "admin_adjust",
            Self::Redeem => "redeem",
        }
    }
}

/// Bare `point_ledger` table row.
#[derive(Debug, sqlx::FromRow)]
pub struct PointLedgerEntry {
    pub id: Uuid,
    pub user_id: Uuid,
    pub delta: i64,
    pub balance_after: i64,
    pub reason: PointReason,
    pub order_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}
