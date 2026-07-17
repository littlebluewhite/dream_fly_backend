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
    /// 退款/取消補償(Step 10)沖回 `checkout_redeem` 扣掉的點數——恆正,契約
    /// §1.6「一個 reason ⇒ 固定正負號」invariant。加於 migration
    /// `20260717000002_point_reason_add_refund_reasons.sql`。
    RefundRestore,
    /// 退款/取消補償(Step 10)沖回 `checkout_earn` 賺到的點數——恆負,同上
    /// invariant。加於 migration
    /// `20260717000002_point_reason_add_refund_reasons.sql`。
    RefundClawback,
}

impl PointReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CheckoutEarn => "checkout_earn",
            Self::CheckoutRedeem => "checkout_redeem",
            Self::AdminAdjust => "admin_adjust",
            Self::Redeem => "redeem",
            Self::RefundRestore => "refund_restore",
            Self::RefundClawback => "refund_clawback",
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
