//! 退款方案 (Refund Plan) — pricing/fulfilment 的第三姊妹檔。前兩個把
//! `&[CheckoutLine]` **算成**一張新訂單(定價、行分派);這裡反過來,把已經
//! 存在的 `&[OrderItem]` **算回**退款/取消該撤銷多少(庫存、點數)——輸入
//! 方向相反(undo vs do),所以獨立成檔,不塞進 `fulfilment.rs`。
//!
//! 同 `pricing`/`fulfilment` 的紀律:純函式、零 DB、零 async,只組裝資料給
//! 編排端消費。唯一呼叫端是 `service::update_order_status` 內的私有補償編排
//! `compensate_order_artifacts_tx`:先用 [`compensation_required`] 決定要不要補償,再讀
//! `orders::repository::find_items_by_order_tx` 的行項與
//! `points::service::find_order_flow_sums_tx` 的 ledger 實錄餵給
//! [`plan_refund`],最後把 [`RefundPlan`] 套進
//! `products::service::restore_stock_tx` / `points::service::apply_delta_tx`
//! / `enrolments::service::cancel_by_order_tx` /
//! `subscriptions::service::cancel_by_order_tx`——鎖序、409 是編排端的事;
//! 這裡算「該撤銷多少」,並由 [`RefundPlan::ledger_deltas`] 定下點數反轉的
//! 正負號與先後。

use uuid::Uuid;

use crate::error::AppError;
use crate::modules::cart::model::CartItemType;
use crate::modules::points::model::{LedgerDelta, OrderPointsFlow};

use super::model::{Order, OrderItem, OrderStatus};

/// 一個要回補庫存的商品行:`product_id` + 要加回去的量。刻意只留
/// `products::service::restore_stock_tx` 的 `&[(Uuid, i32)]` 要的
/// 兩個欄位——編排端逐一拆成 tuple 餵給它。
#[derive(Debug)]
pub struct StockRestore {
    pub product_id: Uuid,
    pub quantity: i32,
}

/// `compensate_order_artifacts_tx` 補償編排要撤銷一筆訂單的 checkout 副作用時需要的一切:哪些
/// 商品行要回補庫存、點數要沖回/沖銷多少。
///
/// `restore_points`/`clawback_points` 是**幅度**(恆 ≥ 0),不是簽過名的
/// delta——符號由 [`RefundPlan::ledger_deltas`] 經 `LedgerDelta` 建構子套
/// (`RefundRestore` 恆正、`RefundClawback` 恆負,契約 §1.6),`0` 代表這個方向
/// 這筆訂單沒有東西可沖,`ledger_deltas` 據此跳過該筆 ledger insert。
#[derive(Debug)]
pub struct RefundPlan {
    pub restocks: Vec<StockRestore>,
    pub restore_points: i64,
    pub clawback_points: i64,
}

impl RefundPlan {
    /// 這筆訂單補償要寫的點數帳,依套用順序排列:RESTORE(正,沖回
    /// `checkout_redeem`)先、CLAWBACK(負,沖回 `checkout_earn`)後,幅度 0 的
    /// 方向跳過(`apply_delta_tx` 拒收零 delta)。`users_points_balance_check`
    /// 逐語句評估,先加後扣把扣款門檻從 `balance ≥ earned` 放寬成
    /// `balance + restored ≥ earned`(ADR-0007 決策 4)——編排端照 vec 順序
    /// 套用,vec 順序就是 ledger 列序。
    pub fn ledger_deltas(&self, order_id: Uuid) -> Vec<LedgerDelta> {
        let mut deltas = Vec::new();
        if self.restore_points > 0 {
            deltas.push(LedgerDelta::refund_restore(self.restore_points, order_id));
        }
        if self.clawback_points > 0 {
            deltas.push(LedgerDelta::refund_clawback(self.clawback_points, order_id));
        }
        deltas
    }
}

/// 算一筆訂單的補償方案。`items` 是 `order` 的 `order_items` 行,`flow` 是
/// `points::service::find_order_flow_sums_tx` 讀回的 [`OrderPointsFlow`]
/// ledger 實錄——**不是** `order.points_earned`/`points_used` 欄位:
/// seed/歷史直建單沒有 ledger 列,讀欄位會沖銷從未發生過的點數流,讀 ledger
/// 則對這種單自然算出全 0(遺留資料政策,ADR-0007)。
///
/// `items` 依 `item_type` 過一個**窮盡** match(無 `_` arm,呼應
/// `fulfilment::plan`):
/// - `Product` 行只在該行 `stock_decremented = true`(checkout 當下是否真的
///   扣過庫存的快照)時才產出一筆 [`StockRestore`]——`false`(無限
///   庫存商品,或 legacy 列)不回補。行上缺 `product_id` 一律
///   `AppError::Internal`,不論是否會產出回補:`order_items_one_target`
///   CHECK 下不可達,同 `fulfilment::plan` 的 belt 守衛,順帶把 `order.id`
///   織進錯誤訊息方便排查是哪張單踩到。
/// - `Course` 行是顯式空 arm——報名/訂閱走 `order_id` 整批 UPDATE
///   (`enrolments::service`/`subscriptions::service` 各自的
///   `cancel_by_order_tx`),不是逐行處理,course 行本身不產生
///   restock。
///
/// **不排序**:`restocks` 保留 `items` 的輸入序。寫鎖的排序紀律屬於真正拿鎖
/// 的那一端——`products::service::restore_stock_tx` 會排序自己收到的副本
/// 再動任何一列,同一個不變式只該有一個 owner。
pub fn plan_refund(
    order: &Order,
    items: &[OrderItem],
    flow: OrderPointsFlow,
) -> Result<RefundPlan, AppError> {
    let mut restocks = Vec::new();

    for item in items {
        match item.item_type {
            CartItemType::Product => {
                let product_id = item.product_id.ok_or_else(|| {
                    AppError::Internal(anyhow::anyhow!(
                        "order {}: product line {} missing product_id",
                        order.id,
                        item.id
                    ))
                })?;
                if item.stock_decremented {
                    restocks.push(StockRestore {
                        product_id,
                        quantity: item.quantity,
                    });
                }
            }
            CartItemType::Course => {
                // 報名/訂閱走 order_id 整批 UPDATE(enrolments::service::
                // cancel_by_order_tx / subscriptions::service::
                // cancel_by_order_tx,由 `compensate_order_artifacts_tx` 呼叫),
                // 非逐行——course 行本身不產生任何 restock。
            }
        }
    }

    Ok(RefundPlan {
        restocks,
        restore_points: flow.redeemed,
        clawback_points: flow.earned,
    })
}

/// 從 `current` 轉往 `target` 是否需要補償(點數/庫存/報名/訂閱撤銷)——
/// `current` 本身已計入營收([`OrderStatus::is_revenue`]:paid/
/// processing/completed)**且** `target` 是終態的「錢要退回去」狀態
/// (cancelled 或 refunded)。一個謂詞同時排除兩個陷阱:
/// - **same-status no-op**——same-status 對(例如 `Cancelled -> Cancelled`)
///   在生產路徑上由 `service::update_order_status` 的同狀態早退擋下,根本
///   不會呼叫到這個謂詞;即使繞開那道早退直接呼叫,current 為
///   Cancelled/Refunded 時 `is_revenue()` 也已經先判 false,兩層防線指向
///   同一個結論。
/// - **pending -> cancelled**——`Pending` 從未成交、不計營收,取消它只是
///   單純的狀態翻轉,沒有東西可撤銷。
pub fn compensation_required(current: &OrderStatus, target: &OrderStatus) -> bool {
    current.is_revenue() && matches!(target, OrderStatus::Cancelled | OrderStatus::Refunded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::modules::points::model::PointReason;

    fn order_fixture() -> Order {
        Order {
            id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            order_number: "TEST-0001".to_string(),
            status: OrderStatus::Paid,
            total_cents: 1_000,
            discount_cents: 0,
            coupon_code: None,
            points_used: 0,
            points_earned: 0,
            payment_method: None,
            paid_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn product_item(
        product_id: Option<Uuid>,
        quantity: i32,
        stock_decremented: bool,
    ) -> OrderItem {
        OrderItem {
            id: Uuid::now_v7(),
            order_id: Uuid::now_v7(),
            item_type: CartItemType::Product,
            product_id,
            course_id: None,
            quantity,
            unit_price_cents: 1_000,
            stock_decremented,
            created_at: Utc::now(),
        }
    }

    fn flow(earned: i64, redeemed: i64) -> OrderPointsFlow {
        OrderPointsFlow { earned, redeemed }
    }

    fn course_item() -> OrderItem {
        OrderItem {
            id: Uuid::now_v7(),
            order_id: Uuid::now_v7(),
            item_type: CartItemType::Course,
            product_id: None,
            course_id: Some(Uuid::now_v7()),
            quantity: 1,
            unit_price_cents: 8_000,
            stock_decremented: false,
            created_at: Utc::now(),
        }
    }

    // --- plan_refund: restocks ---

    #[test]
    fn plan_refund_course_line_produces_no_restock() {
        let order = order_fixture();
        let items = [course_item()];
        let plan = plan_refund(&order, &items, flow(0, 0)).expect("plans");
        assert!(plan.restocks.is_empty());
    }

    #[test]
    fn plan_refund_skips_restock_when_stock_not_decremented() {
        // Unlimited-stock product at checkout time (or a legacy row) — the
        // snapshot says nothing was actually decremented, so nothing gets
        // restored.
        let order = order_fixture();
        let items = [product_item(Some(Uuid::now_v7()), 2, false)];
        let plan = plan_refund(&order, &items, flow(0, 0)).expect("plans");
        assert!(
            plan.restocks.is_empty(),
            "stock_decremented=false must not restock"
        );
    }

    #[test]
    fn plan_refund_restocks_product_line_when_stock_was_decremented() {
        let order = order_fixture();
        let product_id = Uuid::now_v7();
        let items = [product_item(Some(product_id), 3, true)];
        let plan = plan_refund(&order, &items, flow(0, 0)).expect("plans");
        assert_eq!(plan.restocks.len(), 1);
        assert_eq!(plan.restocks[0].product_id, product_id);
        assert_eq!(plan.restocks[0].quantity, 3);
    }

    #[test]
    fn plan_refund_mixed_cart_only_restocks_the_product_line() {
        let order = order_fixture();
        let product_id = Uuid::now_v7();
        let items = [product_item(Some(product_id), 1, true), course_item()];
        let plan = plan_refund(&order, &items, flow(0, 0)).expect("plans");
        assert_eq!(plan.restocks.len(), 1);
        assert_eq!(plan.restocks[0].product_id, product_id);
    }

    #[test]
    fn plan_refund_preserves_input_order() {
        // Not sorted here — see the module doc on why ordering is the write
        // lock owner's job (`products::service::restore_stock_tx`).
        //
        // Adversarial construction: UUIDv7 is time-ordered, so two
        // back-to-back `now_v7()` calls would almost always already satisfy
        // id_a <= id_b — making "preserves input order" and "sorts by
        // product_id ascending" produce the *same* `[id_a, id_b]` output,
        // so this test wouldn't catch a future
        // `restocks.sort_by_key(|r| r.product_id)` regression. Force the
        // larger id into items[0] instead: "preserves order" then outputs
        // [larger, smaller] while "sorts ascending" would output [smaller,
        // larger] — the two behaviors become distinguishable.
        let order = order_fixture();
        let (x, y) = (Uuid::now_v7(), Uuid::now_v7());
        let (id_a, id_b) = (x.max(y), x.min(y));
        let items = [
            product_item(Some(id_a), 1, true),
            product_item(Some(id_b), 1, true),
        ];
        let plan = plan_refund(&order, &items, flow(0, 0)).expect("plans");
        assert_eq!(plan.restocks[0].product_id, id_a, "first (larger id) stays first");
        assert_eq!(plan.restocks[1].product_id, id_b, "second (smaller id) stays second");
    }

    #[test]
    fn plan_refund_empty_items_yields_empty_restocks() {
        let order = order_fixture();
        let plan = plan_refund(&order, &[], flow(0, 0)).expect("plans");
        assert!(plan.restocks.is_empty());
    }

    // --- plan_refund: points ---

    #[test]
    fn plan_refund_copies_points_magnitudes_from_flow() {
        // earned=7, redeemed=3 deliberately distinct so a swapped mapping
        // (restore<->clawback) would be caught: restore_points reverses
        // checkout_redeem (the redeemed amount), clawback_points reverses
        // checkout_earn (the earned amount).
        let order = order_fixture();
        let plan = plan_refund(&order, &[], flow(7, 3)).expect("plans");
        assert_eq!(plan.restore_points, 3);
        assert_eq!(plan.clawback_points, 7);
    }

    #[test]
    fn plan_refund_zero_flow_yields_zero_magnitudes() {
        let order = order_fixture();
        let plan = plan_refund(&order, &[], flow(0, 0)).expect("plans");
        assert_eq!(plan.restore_points, 0);
        assert_eq!(plan.clawback_points, 0);
    }

    // --- RefundPlan::ledger_deltas ---

    #[test]
    fn ledger_deltas_restore_before_clawback() {
        // ADR-0007 決策 4: RESTORE (+redeemed) before CLAWBACK (-earned) —
        // `users_points_balance_check` is evaluated per statement, so this
        // order relaxes the clawback's condition from `balance >= earned` to
        // `balance + restored >= earned`. The caller applies the deltas in
        // vec order, so the vec order *is* the ledger order.
        let order = order_fixture();
        let plan = plan_refund(&order, &[], flow(7, 3)).expect("plans");
        let deltas = plan.ledger_deltas(order.id);
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].reason(), PointReason::RefundRestore);
        assert_eq!(deltas[0].delta(), 3);
        assert_eq!(deltas[0].order_id(), Some(order.id));
        assert_eq!(deltas[1].reason(), PointReason::RefundClawback);
        assert_eq!(deltas[1].delta(), -7);
        assert_eq!(deltas[1].order_id(), Some(order.id));

        // Each direction is skipped when its magnitude is 0.
        let earn_only = plan_refund(&order, &[], flow(7, 0)).expect("plans");
        let deltas = earn_only.ledger_deltas(order.id);
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].reason(), PointReason::RefundClawback);
        let none = plan_refund(&order, &[], flow(0, 0)).expect("plans");
        assert!(none.ledger_deltas(order.id).is_empty());
    }

    // --- plan_refund: belt guard ---

    #[test]
    fn plan_refund_missing_product_id_is_internal_error() {
        let order = order_fixture();
        let items = [product_item(None, 1, true)];
        let err = plan_refund(&order, &items, flow(0, 0)).expect_err("must be Internal");
        assert!(matches!(err, AppError::Internal(_)), "got: {err:?}");
    }

    #[test]
    fn plan_refund_missing_product_id_is_internal_even_when_stock_not_decremented() {
        // The belt guard protects the `order_items_one_target` CHECK
        // invariant, which has nothing to do with `stock_decremented` — a
        // product line is missing its product_id regardless of whether it
        // would have produced a restock.
        let order = order_fixture();
        let items = [product_item(None, 1, false)];
        let err = plan_refund(&order, &items, flow(0, 0)).expect_err("must be Internal");
        assert!(matches!(err, AppError::Internal(_)), "got: {err:?}");
    }

    // --- compensation_required ---

    #[test]
    fn compensation_required_is_true_for_exactly_revenue_to_terminal_pairs() {
        // 6x6 = 36 (current, target) combinations. True for exactly the 3
        // revenue statuses (paid/processing/completed) x 2 terminal targets
        // (cancelled/refunded) = 6 — see the function doc for the two traps
        // this single predicate excludes.
        //
        // Business note (not enforced by this predicate alone): intersected
        // with `OrderStatus::can_transition_to`'s legal edges, only 4 of
        // these 6 are actually reachable — `Processing -> Cancelled` and
        // `Completed -> Cancelled` are compensation_required=true but
        // illegal transitions (can_transition_to 400s before compensation
        // is ever considered), leaving Paid->Cancelled, Paid->Refunded,
        // Processing->Refunded, Completed->Refunded.
        use OrderStatus::*;
        let statuses = [Pending, Paid, Processing, Completed, Cancelled, Refunded];
        let expected_true: [(&str, &str); 6] = [
            ("paid", "cancelled"),
            ("paid", "refunded"),
            ("processing", "cancelled"),
            ("processing", "refunded"),
            ("completed", "cancelled"),
            ("completed", "refunded"),
        ];

        let mut true_count = 0;
        for current in &statuses {
            for target in &statuses {
                let got = compensation_required(current, target);
                let want = expected_true.contains(&(current.as_str(), target.as_str()));
                assert_eq!(
                    got, want,
                    "compensation_required({current:?}, {target:?}) = {got}, want {want}"
                );
                if got {
                    true_count += 1;
                }
            }
        }
        assert_eq!(
            true_count, 6,
            "expected exactly 6 true combinations (3 revenue statuses x 2 terminal targets)"
        );
    }
}
