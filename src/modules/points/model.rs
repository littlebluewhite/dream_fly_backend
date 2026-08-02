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

/// 把一筆 `point_ledger` 寫入的簽過號 delta,與產生它的 [`PointReason`]、以
/// 及(該 reason 要求時)所屬的 `order_id` 焊在一起——把兩條原本只能靠呼叫
/// 端自律的散文前提收進型別系統:
///
/// 1. 契約 §1.6「一個 reason ⇒ 固定正負號」——`CheckoutRedeem`/`Redeem`/
///    `RefundClawback` 恆負,`CheckoutEarn`/`RefundRestore` 恆正。
/// 2. checkout/refund 類 reason 恆帶 `order_id`——`uniq_point_ledger_refund_once`
///    這個 partial unique index(ADR-0007 決策 7)靠 refund 列一定帶
///    `order_id` 才能擋下同一筆訂單被重複退點;`CheckoutEarn`/
///    `CheckoutRedeem`/`RefundRestore`/`RefundClawback` 四個 reason 因此在
///    建構子這一層就強制要 `order_id`,不留「忘記帶」的空隙。
///
/// 六個建構子與六個 [`PointReason`] 變體一對一同名,不另造詞彙——呼叫端讀
/// 名字就知道對應哪個 reason。`admin_adjust` 是唯一帶符號的逃生口:它的語
/// 意本來就是「正負皆可的人工調整」,不是前五者遺漏的第六種固定符號,因此
/// 不收 `magnitude`,也沒有非負斷言。
///
/// **幅度非負是 defense-in-depth,不是型別保證**:五個固定符號建構子收
/// `magnitude: i64`(不是 `u64`),也不回傳 `Result`——四個呼叫端的幅度來源
/// 各自已有 owner 級保證(`orders::pricing::PricingOutcome` 的核測試、
/// `orders::refund::RefundPlan` 的 `restore_points`/`clawback_points` doc
/// 與測試皆載明恆 `>= 0`、`rewards.points_cost` 的 DB `CHECK > 0`、seed 的
/// 字面正值),在型別層再收一次是不必要的重複防線。`debug_assert!(magnitude
/// >= 0, ...)` 只在 debug/測試 build 存在、release build 會被編掉——這是有
/// owner 兜底之後「順手多檢查一次」的 defense-in-depth,不是唯一防線,
/// release build 少了這道斷言不影響正確性(對照 `sessions::repository` 的
/// 單日物化案:那裡的 `MaterializedRange` 單日前提原本同樣是唯一防線,已
/// 改型別化收進 `MaterializedDay`——同判準、相反結論,詳見 ADR-0007 第四
/// 則 Addendum)。斷言用 `>=` 不用 `>`:零幅度被放行通過建構子,交給
/// `apply_delta_tx` 既有的 zero-delta `Validation` guard 處理(該 guard 不
/// 動)——建構子不搶在前面用 panic 攔零。
///
/// 欄位全私有:`points::service` 是本檔(`model`)的兄弟模組,不能透過任何
/// 後門讀到私有欄位,只能經 `delta()`/`reason()`/`order_id()` 三個唯讀存取
/// 子讀值——風格鏡像 `points::service::BalanceLock` 的
/// `user_id()`/`balance()`。
#[derive(Debug)]
pub struct LedgerDelta {
    delta: i64,
    reason: PointReason,
    order_id: Option<Uuid>,
}

impl LedgerDelta {
    /// 結帳賺點——`delta` 恆正(契約 §1.6)。
    pub fn checkout_earn(magnitude: i64, order_id: Uuid) -> Self {
        debug_assert!(
            magnitude >= 0,
            "checkout_earn magnitude must be non-negative"
        );
        Self {
            delta: magnitude,
            reason: PointReason::CheckoutEarn,
            order_id: Some(order_id),
        }
    }

    /// 結帳點數折抵——`delta` 恆負(契約 §1.6)。
    pub fn checkout_redeem(magnitude: i64, order_id: Uuid) -> Self {
        debug_assert!(
            magnitude >= 0,
            "checkout_redeem magnitude must be non-negative"
        );
        Self {
            delta: -magnitude,
            reason: PointReason::CheckoutRedeem,
            order_id: Some(order_id),
        }
    }

    /// 兌換獎勵扣點(`POST /rewards/{id}/redeem`)——`delta` 恆負,`order_id`
    /// 恆 `None`(與訂單無關,契約 §1.6/§3.23)。
    pub fn redeem(magnitude: i64) -> Self {
        debug_assert!(magnitude >= 0, "redeem magnitude must be non-negative");
        Self {
            delta: -magnitude,
            reason: PointReason::Redeem,
            order_id: None,
        }
    }

    /// 退款/取消補償——沖回 `checkout_redeem` 扣掉的點數,`delta` 恆正
    /// (ADR-0007)。
    pub fn refund_restore(magnitude: i64, order_id: Uuid) -> Self {
        debug_assert!(
            magnitude >= 0,
            "refund_restore magnitude must be non-negative"
        );
        Self {
            delta: magnitude,
            reason: PointReason::RefundRestore,
            order_id: Some(order_id),
        }
    }

    /// 退款/取消補償——沖回 `checkout_earn` 賺到的點數,`delta` 恆負
    /// (ADR-0007)。
    pub fn refund_clawback(magnitude: i64, order_id: Uuid) -> Self {
        debug_assert!(
            magnitude >= 0,
            "refund_clawback magnitude must be non-negative"
        );
        Self {
            delta: -magnitude,
            reason: PointReason::RefundClawback,
            order_id: Some(order_id),
        }
    }

    /// admin 手動調整(`POST /points/adjustments`)——唯一帶符號的逃生口:
    /// 呼叫端本來就要表達「正負皆可」的調整,不收 `magnitude`,也沒有非負
    /// 斷言。`order_id` 恆 `None`——人工調整不隸屬任何訂單。
    pub fn admin_adjust(signed_delta: i64) -> Self {
        Self {
            delta: signed_delta,
            reason: PointReason::AdminAdjust,
            order_id: None,
        }
    }

    pub fn delta(&self) -> i64 {
        self.delta
    }

    pub fn reason(&self) -> PointReason {
        self.reason
    }

    pub fn order_id(&self) -> Option<Uuid> {
        self.order_id
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_earn_pairs_positive_delta_with_reason_and_order_id() {
        let order_id = Uuid::now_v7();
        let ld = LedgerDelta::checkout_earn(50, order_id);
        assert_eq!(ld.delta(), 50);
        assert_eq!(ld.reason(), PointReason::CheckoutEarn);
        assert_eq!(ld.order_id(), Some(order_id));
    }

    #[test]
    fn checkout_redeem_pairs_negative_delta_with_reason_and_order_id() {
        let order_id = Uuid::now_v7();
        let ld = LedgerDelta::checkout_redeem(30, order_id);
        assert_eq!(ld.delta(), -30);
        assert_eq!(ld.reason(), PointReason::CheckoutRedeem);
        assert_eq!(ld.order_id(), Some(order_id));
    }

    #[test]
    fn redeem_pairs_negative_delta_with_reason_and_no_order_id() {
        let ld = LedgerDelta::redeem(20);
        assert_eq!(ld.delta(), -20);
        assert_eq!(ld.reason(), PointReason::Redeem);
        assert_eq!(ld.order_id(), None);
    }

    #[test]
    fn refund_restore_pairs_positive_delta_with_reason_and_order_id() {
        let order_id = Uuid::now_v7();
        let ld = LedgerDelta::refund_restore(15, order_id);
        assert_eq!(ld.delta(), 15);
        assert_eq!(ld.reason(), PointReason::RefundRestore);
        assert_eq!(ld.order_id(), Some(order_id));
    }

    #[test]
    fn refund_clawback_pairs_negative_delta_with_reason_and_order_id() {
        let order_id = Uuid::now_v7();
        let ld = LedgerDelta::refund_clawback(15, order_id);
        assert_eq!(ld.delta(), -15);
        assert_eq!(ld.reason(), PointReason::RefundClawback);
        assert_eq!(ld.order_id(), Some(order_id));
    }

    #[test]
    fn admin_adjust_passes_signed_delta_through_with_reason_and_no_order_id() {
        let negative = LedgerDelta::admin_adjust(-40);
        assert_eq!(negative.delta(), -40);
        assert_eq!(negative.reason(), PointReason::AdminAdjust);
        assert_eq!(negative.order_id(), None);

        let positive = LedgerDelta::admin_adjust(40);
        assert_eq!(positive.delta(), 40);
        assert_eq!(positive.reason(), PointReason::AdminAdjust);
        assert_eq!(positive.order_id(), None);
    }
}
