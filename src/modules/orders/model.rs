use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::modules::cart::model::CartItemType;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "order_status", rename_all = "snake_case")]
pub enum OrderStatus {
    Pending,
    Paid,
    Processing,
    Completed,
    Cancelled,
    Refunded,
}

impl OrderStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Paid => "paid",
            Self::Processing => "processing",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Refunded => "refunded",
        }
    }

    /// Allowed transitions. Terminal states (completed, cancelled, refunded)
    /// have no outgoing edges — once an order is refunded it cannot be
    /// shipped, once completed it cannot be reverted.
    pub fn can_transition_to(&self, next: &Self) -> bool {
        use OrderStatus::*;
        match (self, next) {
            (Pending, Paid) | (Pending, Cancelled) => true,
            (Paid, Processing) | (Paid, Refunded) | (Paid, Cancelled) => true,
            (Processing, Completed) | (Processing, Refunded) => true,
            (Completed, Refunded) => true,
            // Idempotent no-op: same-status updates are accepted so retries
            // of a webhook / admin action do not 422.
            (a, b) if a.as_str() == b.as_str() => true,
            _ => false,
        }
    }

    /// 計入營收的狀態。本謂詞是窮盡 match(無 `_` arm)——新增
    /// [`OrderStatus`] 變體時編譯器在此強迫決定算不算營收,不會再靜默漏
    /// 判。[`REVENUE_STATUSES`] 是 SQL 綁定用的攣生陣列(products/reports
    /// 的查詢綁點),不是本函式讀的來源;兩者的一致性改由交叉測試
    /// `revenue_predicate_matches_revenue_statuses_array` 錨定。退款/取消
    /// 補償(`refund::compensation_required`,Step 10d)用它判斷「這筆訂單
    /// 的*現況*算不算已成交」——只有從一個計入營收的狀態轉往終態,才有東
    /// 西需要撤銷。
    pub fn is_revenue(&self) -> bool {
        match self {
            Self::Paid | Self::Processing | Self::Completed => true,
            Self::Pending | Self::Cancelled | Self::Refunded => false,
        }
    }
}

impl std::str::FromStr for OrderStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "paid" => Ok(Self::Paid),
            "processing" => Ok(Self::Processing),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            "refunded" => Ok(Self::Refunded),
            _ => Err(()),
        }
    }
}

/// 計入營收的訂單狀態,SQL 綁定用(reports 的營收彙總查詢、
/// `products::repository::find_sold_counts` 售出件數查詢皆直接綁定本常
/// 數)。謂詞 owner 已是 [`OrderStatus::is_revenue`](窮盡 match)——本陣
/// 列是它的 SQL 綁定攣生面,兩者由交叉測試
/// `revenue_predicate_matches_revenue_statuses_array` 錨定,不是本陣列反
/// 向定義 is_revenue。
pub const REVENUE_STATUSES: [&str; 3] = ["paid", "processing", "completed"];

/// 付款方式值域(應用層,非 DB enum——`orders.payment_method` 只是
/// `VARCHAR(30)`,Round 4 Task P4-B1)。`service::checkout` 缺省時預設
/// `credit_card`(向後相容既有不帶此欄的呼叫者);不在此集合內的值回 422。
/// Round 4 Phase 4 報表依此欄分組付款方式。
pub const PAYMENT_METHODS: [&str; 5] = ["credit_card", "line_pay", "atm", "jkopay", "cash"];

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Order {
    pub id: Uuid,
    pub user_id: Uuid,
    pub order_number: String,
    pub status: OrderStatus,
    pub total_cents: i64,
    pub discount_cents: i64,
    pub coupon_code: Option<String>,
    pub points_used: i64,
    pub points_earned: i64,
    /// Nullable — orders created before this column existed have `NULL`.
    /// Every order created by `service::checkout` from here on always has
    /// a value (defaulted to `credit_card` when the request omits it).
    pub payment_method: Option<String>,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `order_items` row. Exactly one of `product_id`/`course_id` is set,
/// matching `item_type` (enforced by the `order_items_one_target` CHECK) —
/// mirrors `cart::model::CartItem`'s product/course dual-target shape, and
/// reuses the same `cart_item_type` Postgres enum since an order line is
/// just a cart line's frozen snapshot at checkout time.
#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct OrderItem {
    pub id: Uuid,
    pub order_id: Uuid,
    pub item_type: CartItemType,
    pub product_id: Option<Uuid>,
    pub course_id: Option<Uuid>,
    pub quantity: i32,
    pub unit_price_cents: i64,
    /// Whether this line actually decremented `products.stock` at checkout
    /// time — a snapshot of that checkout-time fact, not a read of the
    /// product's *current* stock mode (Step 10a, migration
    /// `20260717000004_order_items_stock_decremented.sql`). `true` only for
    /// product lines whose product had finite stock at checkout; `false`
    /// for course lines (never touch stock) and for product lines whose
    /// product had `stock IS NULL` (unlimited) at checkout time. Refund/
    /// cancel compensation (Step 10d/10e) reads this instead of the
    /// product's current `stock` nullability, since an admin can change a
    /// product's stock mode after the sale and the snapshot must not drift
    /// with that later edit. Deliberately excluded from `OrderItemResponse`
    /// (`dto.rs`) — internal compensation bookkeeping, not buyer-facing.
    pub stock_decremented: bool,
    pub created_at: DateTime<Utc>,
}

/// `orders` JOINed with `users` for the two fields the admin order list
/// needs (`user_name`, `user_email`). Kept as its own flat row type (rather
/// than nesting an [`Order`] inside it) because sqlx's derived `FromRow`
/// maps one column per field and has no support for nested structs.
#[derive(Debug, sqlx::FromRow)]
pub struct AdminOrderRow {
    pub id: Uuid,
    pub order_number: String,
    pub user_name: String,
    pub user_email: String,
    pub status: OrderStatus,
    pub total_cents: i64,
    pub points_used: i64,
    pub coupon_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub items: sqlx::types::Json<Vec<OrderItemBrief>>,
}

/// `{ name, quantity }` — the minimal per-line summary surfaced by
/// `OrderSummary`/`AdminOrderSummary`'s `items` field. Decoded straight out
/// of a `jsonb_agg(...)` correlated-subquery aggregate (see
/// `repository::find_by_user` / `find_all_with_user`), so it needs
/// `Deserialize` in addition to the `Serialize` every other response type
/// needs. `name` is the `order_items.name` snapshot column (what the buyer
/// purchased at checkout time), never the live product/course catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderItemBrief {
    pub name: String,
    pub quantity: i32,
}

/// Row shape for `repository::find_by_user` — a slimmer projection of
/// `orders` than the full [`Order`] model, plus the aggregated `items`
/// brief. Kept separate from `Order` because most `Order` readers (checkout,
/// `get_order`, status transitions) don't want the extra per-row aggregate
/// subquery this requires.
#[derive(Debug, sqlx::FromRow)]
pub struct OrderSummaryRow {
    pub id: Uuid,
    pub order_number: String,
    pub status: OrderStatus,
    pub total_cents: i64,
    pub created_at: DateTime<Utc>,
    pub items: sqlx::types::Json<Vec<OrderItemBrief>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手列全部 6 個變體(repo 無 EnumIter,不為此加依賴)。固定長度的陣列
    /// 型別本身擋不住「新變體忘了加進來」——真正的防線是下面
    /// `revenue_predicate_matches_revenue_statuses_array` 內的窮盡 match
    /// tripwire,新增變體時那裡先編譯錯誤,把人押回這裡補上一行。
    const ALL_STATUSES: [OrderStatus; 6] = [
        OrderStatus::Pending,
        OrderStatus::Paid,
        OrderStatus::Processing,
        OrderStatus::Completed,
        OrderStatus::Cancelled,
        OrderStatus::Refunded,
    ];

    #[test]
    fn can_transition_pending_to_paid_is_legal() {
        assert!(OrderStatus::Pending.can_transition_to(&OrderStatus::Paid));
    }

    #[test]
    fn can_transition_pending_to_processing_is_illegal() {
        // Pending only opens onto Paid/Cancelled — Processing must be
        // reached via Paid first.
        assert!(!OrderStatus::Pending.can_transition_to(&OrderStatus::Processing));
    }

    #[test]
    fn can_transition_completed_to_paid_is_illegal() {
        // Completed only opens onto Refunded (plus the same-state case
        // below) — it can never revert to an earlier status.
        assert!(!OrderStatus::Completed.can_transition_to(&OrderStatus::Paid));
    }

    #[test]
    fn can_transition_same_state_is_legal_for_every_status() {
        // Idempotent no-op: a retried webhook/admin action re-applying the
        // current status must not 422 — covers every variant, not just one.
        for status in ALL_STATUSES {
            let same = status.clone();
            assert!(
                status.can_transition_to(&same),
                "{status:?} -> itself should be legal"
            );
        }
    }

    #[test]
    fn revenue_predicate_matches_revenue_statuses_array() {
        // 交叉錨定 is_revenue()(窮盡 match,真正的謂詞 owner)與
        // REVENUE_STATUSES(SQL 綁定攣生面)——逐變體相等 + 長度相等,才是
        // 真正的集合相等,不只是「看起來一致」。
        for status in ALL_STATUSES {
            // Tripwire:窮盡 match、無 `_` arm。新增 OrderStatus 變體時本
            // 行編譯錯誤——固定長度的 ALL_STATUSES 本身擋不住新變體被漏
            // 列,靠這裡把人押回本 test mod。
            match status {
                OrderStatus::Pending
                | OrderStatus::Paid
                | OrderStatus::Processing
                | OrderStatus::Completed
                | OrderStatus::Cancelled
                | OrderStatus::Refunded => {}
            }
            assert_eq!(
                status.is_revenue(),
                REVENUE_STATUSES.contains(&status.as_str()),
                "{status:?}: is_revenue() and REVENUE_STATUSES disagree"
            );
        }
        // 長度斷言:逐變體比對防不了 REVENUE_STATUSES 裡混進重複或不對應
        // 任何變體的字串(這類元素不會讓上面任何一次比對失敗)——兩邊集合
        // 大小相等,才真正排除這個殘餘可能性。
        let revenue_variant_count = ALL_STATUSES.into_iter().filter(|s| s.is_revenue()).count();
        assert_eq!(
            revenue_variant_count,
            REVENUE_STATUSES.len(),
            "REVENUE_STATUSES length should equal the number of is_revenue()==true variants"
        );
    }
}
