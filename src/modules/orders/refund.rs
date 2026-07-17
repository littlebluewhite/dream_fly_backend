//! 退款方案 (Refund Plan) — pricing/fulfilment 的第三姊妹檔。前兩個把
//! `&[CheckoutLine]` **算成**一張新訂單(定價、行分派);這裡反過來,把已經
//! 存在的 `&[OrderItem]` **算回**退款/取消該撤銷多少(庫存、點數)——輸入
//! 方向相反(undo vs do),所以獨立成檔,不塞進 `fulfilment.rs`。
//!
//! 同 `pricing`/`fulfilment` 的紀律:純函式、零 DB、零 async,只組裝資料給
//! 編排端消費。唯一呼叫端是 Step 10e——`service::update_order_status` 內的
//! 私有補償編排:先用 [`compensation_required`] 決定要不要補償,再讀
//! `orders::repository::find_items_by_order_tx` 的行項與
//! `points::service::find_order_flow_sums_tx` 的 ledger 實錄餵給
//! [`plan_refund`],最後把 [`RefundPlan`] 套進
//! `products::service::restore_stock_tx` / `points::service::apply_delta_tx`
//! / `enrolments::service::cancel_by_order_tx` /
//! `subscriptions::service::cancel_by_order_tx`——正負號、鎖序、409 全是編排
//! 端的事,這裡只算「該撤銷多少」。

use uuid::Uuid;

use crate::error::AppError;
use crate::modules::cart::model::CartItemType;

use super::model::{Order, OrderItem, OrderStatus};

/// 一個要回補庫存的商品行:`product_id` + 要加回去的量。刻意只留
/// `products::service::restore_stock_tx`(Step 10c)的 `&[(Uuid, i32)]` 要的
/// 兩個欄位——編排端逐一拆成 tuple 餵給它。
#[derive(Debug)]
pub struct StockRestore {
    pub product_id: Uuid,
    pub quantity: i32,
}

/// Step 10e 的補償編排要撤銷一筆訂單的 checkout 副作用時需要的一切:哪些
/// 商品行要回補庫存、點數要沖回/沖銷多少。
///
/// `restore_points`/`clawback_points` 是**幅度**(恆 ≥ 0),不是簽過名的
/// delta——符號由編排端套(`RefundRestore` 恆正、`RefundClawback` 恆負,契約
/// §1.6),`0` 代表這個方向這筆訂單沒有東西可沖,編排端據此跳過該筆 ledger
/// insert。
#[derive(Debug)]
pub struct RefundPlan {
    pub restocks: Vec<StockRestore>,
    pub restore_points: i64,
    pub clawback_points: i64,
}

/// 算一筆訂單的補償方案。`items` 是 `order` 的 `order_items` 行,`flow` 是
/// `points::service::find_order_flow_sums_tx` 讀回的 `(earned, redeemed)`
/// ledger 實錄——**不是** `order.points_earned`/`points_used` 欄位:
/// seed/歷史直建單沒有 ledger 列,讀欄位會沖銷從未發生過的點數流,讀 ledger
/// 則對這種單自然算出全 0(遺留資料政策,ADR-0007)。
///
/// `items` 依 `item_type` 過一個**窮盡** match(無 `_` arm,呼應
/// `fulfilment::plan`):
/// - `Product` 行只在該行 `stock_decremented = true`(checkout 當下是否真的
///   扣過庫存的快照,Step 10a)時才產出一筆 [`StockRestore`]——`false`(無限
///   庫存商品,或 legacy 列)不回補。行上缺 `product_id` 一律
///   `AppError::Internal`,不論是否會產出回補:`order_items_one_target`
///   CHECK 下不可達,同 `fulfilment::plan` 的 belt 守衛,順帶把 `order.id`
///   織進錯誤訊息方便排查是哪張單踩到。
/// - `Course` 行是顯式空 arm——報名/訂閱走 `order_id` 整批 UPDATE(Step 10c
///   的 `cancel_by_order_tx` 一對),不是逐行處理,course 行本身不產生
///   restock。
///
/// **不排序**:`restocks` 保留 `items` 的輸入序。寫鎖的排序紀律屬於真正拿鎖
/// 的那一端——`products::service::restore_stock_tx` 會排序自己收到的副本
/// 再動任何一列(Step 10c),同一個不變式只該有一個 owner。
pub fn plan_refund(
    order: &Order,
    items: &[OrderItem],
    flow: (i64, i64),
) -> Result<RefundPlan, AppError> {
    let (earned, redeemed) = flow;
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
                // cancel_by_order_tx,Step 10e 呼叫),非逐行——course 行本身
                // 不產生任何 restock。
            }
        }
    }

    Ok(RefundPlan {
        restocks,
        restore_points: redeemed,
        clawback_points: earned,
    })
}

/// 從 `current` 轉往 `target` 是否需要補償(點數/庫存/報名/訂閱撤銷,Step
/// 10e)——`current` 本身已計入營收([`OrderStatus::is_revenue`]:paid/
/// processing/completed)**且** `target` 是終態的「錢要退回去」狀態
/// (cancelled 或 refunded)。一個謂詞同時排除兩個陷阱:
/// - **same-status no-op**——`model.rs` `can_transition_to` 的冪等自環
///   (例如 `Cancelled -> Cancelled`)永遠不會落在這裡:自環的 `current` 是
///   Cancelled/Refunded,`is_revenue()` 就先擋掉了。
/// - **pending -> cancelled**——`Pending` 從未成交、不計營收,取消它只是
///   單純的狀態翻轉,沒有東西可撤銷。
pub fn compensation_required(current: &OrderStatus, target: &OrderStatus) -> bool {
    current.is_revenue() && matches!(target, OrderStatus::Cancelled | OrderStatus::Refunded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

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
        let plan = plan_refund(&order, &items, (0, 0)).expect("plans");
        assert!(plan.restocks.is_empty());
    }

    #[test]
    fn plan_refund_skips_restock_when_stock_not_decremented() {
        // Unlimited-stock product at checkout time (or a legacy row) — the
        // snapshot says nothing was actually decremented, so nothing gets
        // restored.
        let order = order_fixture();
        let items = [product_item(Some(Uuid::now_v7()), 2, false)];
        let plan = plan_refund(&order, &items, (0, 0)).expect("plans");
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
        let plan = plan_refund(&order, &items, (0, 0)).expect("plans");
        assert_eq!(plan.restocks.len(), 1);
        assert_eq!(plan.restocks[0].product_id, product_id);
        assert_eq!(plan.restocks[0].quantity, 3);
    }

    #[test]
    fn plan_refund_mixed_cart_only_restocks_the_product_line() {
        let order = order_fixture();
        let product_id = Uuid::now_v7();
        let items = [product_item(Some(product_id), 1, true), course_item()];
        let plan = plan_refund(&order, &items, (0, 0)).expect("plans");
        assert_eq!(plan.restocks.len(), 1);
        assert_eq!(plan.restocks[0].product_id, product_id);
    }

    #[test]
    fn plan_refund_preserves_input_order() {
        // Not sorted here — see the module doc on why ordering is the write
        // lock owner's job (`products::service::restore_stock_tx`).
        let order = order_fixture();
        let id_a = Uuid::now_v7();
        let id_b = Uuid::now_v7();
        let items = [
            product_item(Some(id_a), 1, true),
            product_item(Some(id_b), 1, true),
        ];
        let plan = plan_refund(&order, &items, (0, 0)).expect("plans");
        assert_eq!(plan.restocks[0].product_id, id_a, "first stays first");
        assert_eq!(plan.restocks[1].product_id, id_b, "second stays second");
    }

    #[test]
    fn plan_refund_empty_items_yields_empty_restocks() {
        let order = order_fixture();
        let plan = plan_refund(&order, &[], (0, 0)).expect("plans");
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
        let plan = plan_refund(&order, &[], (7, 3)).expect("plans");
        assert_eq!(plan.restore_points, 3);
        assert_eq!(plan.clawback_points, 7);
    }

    #[test]
    fn plan_refund_zero_flow_yields_zero_magnitudes() {
        let order = order_fixture();
        let plan = plan_refund(&order, &[], (0, 0)).expect("plans");
        assert_eq!(plan.restore_points, 0);
        assert_eq!(plan.clawback_points, 0);
    }

    // --- plan_refund: belt guard ---

    #[test]
    fn plan_refund_missing_product_id_is_internal_error() {
        let order = order_fixture();
        let items = [product_item(None, 1, true)];
        let err = plan_refund(&order, &items, (0, 0)).expect_err("must be Internal");
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
        let err = plan_refund(&order, &items, (0, 0)).expect_err("must be Internal");
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
