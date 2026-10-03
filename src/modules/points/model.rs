use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type, ts_rs::TS)]
#[sqlx(type_name = "point_reason", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PointReason {
    CheckoutEarn,
    CheckoutRedeem,
    AdminAdjust,
    /// Points spent redeeming a `rewards` catalog item (Round 3 Task 6, 裁決
    /// 7) — added via migration `20260707000004_point_reason_add_redeem.sql`.
    Redeem,
    /// 退款/取消補償沖回 `checkout_redeem` 扣掉的點數——恆正,契約
    /// §1.6「一個 reason ⇒ 固定正負號」invariant。加於 migration
    /// `20260717000002_point_reason_add_refund_reasons.sql`。
    RefundRestore,
    /// 退款/取消補償沖回 `checkout_earn` 賺到的點數——恆負,同上
    /// invariant。加於 migration
    /// `20260717000002_point_reason_add_refund_reasons.sql`。
    RefundClawback,
}

impl PointReason {
    /// Every variant, in declaration (= PG label) order — single owner of the value
    /// domain.
    pub const ALL: [Self; 6] = [
        Self::CheckoutEarn,
        Self::CheckoutRedeem,
        Self::AdminAdjust,
        Self::Redeem,
        Self::RefundRestore,
        Self::RefundClawback,
    ];

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
/// `magnitude: i64`(不是 `u64`),也不回傳 `Result`——三個幅度來源各自已
/// 有 owner 級保證(`orders::pricing::PricingOutcome` 的核測試、
/// [`OrderPointsFlow`] 的 `earned`/`redeemed` doc 與 SQL 讀回測試皆載明恆
/// `>= 0`、`rewards.points_cost` 的 DB `CHECK > 0`;seed 的訂單點數也經前
/// 兩者取得),在型別層再收一次是不必要的重複防線。`debug_assert!(magnitude
/// >= 0, ...)` 只在 debug/測試 build 存在、release build 會被編掉——這是有
/// owner 兜底之後「順手多檢查一次」的 defense-in-depth,不是唯一防線,
/// release build 少了這道斷言不影響正確性(對照 `sessions::calendar` 的
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

/// One order's checkout point flow as recorded in `point_ledger` — the
/// summed `checkout_earn` and `checkout_redeem` magnitudes, both `>= 0`
/// (`repository::find_order_flow_sums_tx`). Read by refund/cancel
/// compensation through `service::reverse_order_tx`, which applies
/// [`OrderPointsFlow::reversal_deltas`] — the ledger trace is points' own,
/// so reversing it is points' job, not the refund orchestrator's.
#[derive(Debug, sqlx::FromRow)]
pub struct OrderPointsFlow {
    pub earned: i64,
    pub redeemed: i64,
}

impl OrderPointsFlow {
    /// 這筆訂單退款/取消補償要寫的點數帳,依套用順序排列:RESTORE(正,沖回
    /// `checkout_redeem`)先、CLAWBACK(負,沖回 `checkout_earn`)後,幅度 0 的
    /// 方向跳過(`apply_delta_tx` 拒收零 delta)。`users_points_balance_check`
    /// 逐語句評估,先加後扣把扣款門檻從 `balance ≥ earned` 放寬成
    /// `balance + restored ≥ earned`(ADR-0007 決策 4)——`reverse_order_tx`
    /// 照 vec 順序套用,vec 順序就是 ledger 列序。
    pub fn reversal_deltas(&self, order_id: Uuid) -> Vec<LedgerDelta> {
        let mut deltas = Vec::new();
        if self.redeemed > 0 {
            deltas.push(LedgerDelta::refund_restore(self.redeemed, order_id));
        }
        if self.earned > 0 {
            deltas.push(LedgerDelta::refund_clawback(self.earned, order_id));
        }
        deltas
    }
}

/// 點數級距 (Points Tier)——會員依 `points_balance` 分入的固定 4 級:
/// `regular`(<500)/`bronze`(500–1999)/`silver`(2000–4999)/`gold`(≥5000)。
/// 本型別是級距規則的 Rust owner;`reports::repository::tier_distribution`
/// 的 SQL `CASE` 是它的 SQL 攣生面(報表在 DB 端分桶),兩者由交叉測試
/// `points_tier_matches_sql_tier_distribution_case`(`tests/service_reports.rs`)
/// 錨定,不是靠手抄保持一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointsTier {
    Regular,
    Bronze,
    Silver,
    Gold,
}

impl PointsTier {
    /// 由低到高——與 `tier_distribution` 的 `tiers(bucket, ord)` 同序。
    pub const ALL: [PointsTier; 4] = [Self::Regular, Self::Bronze, Self::Silver, Self::Gold];

    /// 該級距的下限(含)。`Regular` 的 0 是 `points_balance` 的 DB `CHECK
    /// (points_balance >= 0)` 下限。
    pub fn floor(self) -> i64 {
        match self {
            Self::Regular => 0,
            Self::Bronze => 500,
            Self::Silver => 2_000,
            Self::Gold => 5_000,
        }
    }

    /// 餘額所屬級距:`floor() <= balance` 的最高一級。負數(DB CHECK 擋掉的
    /// 理論值)落 `Regular`,同 SQL `CASE` 的 `ELSE`。
    pub fn from_balance(balance: i64) -> Self {
        Self::ALL
            .into_iter()
            .rev()
            .find(|tier| balance >= tier.floor())
            .unwrap_or(Self::Regular)
    }

    /// 報表桶名(`tier_distribution` 的 `bucket` 值)。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Bronze => "bronze",
            Self::Silver => "silver",
            Self::Gold => "gold",
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

    #[test]
    fn reversal_deltas_restore_before_clawback_skips_zero() {
        // ADR-0007 決策 4: RESTORE (+redeemed) before CLAWBACK (-earned) —
        // `users_points_balance_check` is evaluated per statement, so this
        // order relaxes the clawback's condition from `balance >= earned` to
        // `balance + restored >= earned`. `reverse_order_tx` applies the
        // deltas in vec order, so the vec order *is* the ledger order.
        // earned=7, redeemed=3 deliberately distinct so a swapped mapping
        // (restore<->clawback) would be caught.
        let order_id = Uuid::now_v7();
        let flow = OrderPointsFlow {
            earned: 7,
            redeemed: 3,
        };
        let deltas = flow.reversal_deltas(order_id);
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].reason(), PointReason::RefundRestore);
        assert_eq!(deltas[0].delta(), 3);
        assert_eq!(deltas[0].order_id(), Some(order_id));
        assert_eq!(deltas[1].reason(), PointReason::RefundClawback);
        assert_eq!(deltas[1].delta(), -7);
        assert_eq!(deltas[1].order_id(), Some(order_id));

        // Each direction is skipped when its magnitude is 0.
        let earn_only = OrderPointsFlow {
            earned: 7,
            redeemed: 0,
        };
        let deltas = earn_only.reversal_deltas(order_id);
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].reason(), PointReason::RefundClawback);
        let redeem_only = OrderPointsFlow {
            earned: 0,
            redeemed: 3,
        };
        let deltas = redeem_only.reversal_deltas(order_id);
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].reason(), PointReason::RefundRestore);
        let none = OrderPointsFlow {
            earned: 0,
            redeemed: 0,
        };
        assert!(none.reversal_deltas(order_id).is_empty());
    }

    #[test]
    fn points_tier_from_balance_splits_at_each_floor() {
        // <500 regular / 500–1999 bronze / 2000–4999 silver / ≥5000 gold —
        // each floor itself belongs to its tier, one point below it to the
        // tier underneath.
        let cases = [
            (0, PointsTier::Regular),
            (499, PointsTier::Regular),
            (500, PointsTier::Bronze),
            (1_999, PointsTier::Bronze),
            (2_000, PointsTier::Silver),
            (4_999, PointsTier::Silver),
            (5_000, PointsTier::Gold),
            (i64::MAX, PointsTier::Gold),
            // DB CHECK 擋掉的理論值——同 SQL CASE 的 ELSE,落 regular。
            (-1, PointsTier::Regular),
        ];
        for (balance, tier) in cases {
            assert_eq!(PointsTier::from_balance(balance), tier, "balance {balance}");
        }
    }

    #[test]
    fn points_tier_all_is_ascending_and_each_floor_maps_back_to_its_tier() {
        let floors: Vec<i64> = PointsTier::ALL.iter().map(|t| t.floor()).collect();
        assert_eq!(floors, [0, 500, 2_000, 5_000]);
        for tier in PointsTier::ALL {
            assert_eq!(PointsTier::from_balance(tier.floor()), tier);
        }
    }

    #[test]
    fn points_tier_as_str_matches_report_bucket_names() {
        let names: Vec<&str> = PointsTier::ALL.iter().map(|t| t.as_str()).collect();
        assert_eq!(names, ["regular", "bronze", "silver", "gold"]);
    }
}
