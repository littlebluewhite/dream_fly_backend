//! 訂單狀態轉移決策 (Transition Decision) — 純函式、零 DB、零 async。
//! `service::update_order_status` 在 `FOR UPDATE` 讀到現況之後用
//! [`decide_transition`] 決定這次狀態轉移是 no-op、單純翻轉,還是翻轉加補償。
//!
//! 補償本身不在這裡算:私有編排 `compensate_order_artifacts_tx` 是一張平鋪
//! 的 owner 呼叫清單——`orders::locks::acquire_refund_locks` →
//! `points::service::reverse_order_tx` →
//! `products::service::restore_for_order_tx` → `enrolments`/`subscriptions`
//! 的 `cancel_by_order_tx`——每個 owner 讀自己的結帳痕跡、撤銷自己的副作用
//! (ADR-0007 決策 8)。
//!
//! 退款(Refund)的語意:
//! - Cancelled 與 Refunded 是同一套補償的兩個終態標籤,不是兩種補償。
//! - **整單**:不論已核銷或使用多少,一律全額反轉,不按使用比例折算。
//! - 點數不足時整筆回滾(409「點數不足」),不 clamp;修復迴路是 admin 補點
//!   端點 `POST /points/adjustments`,補完再重送退款。
//! - 訂單與訂單行從不刪除:退款是狀態機轉到終態,原始下單紀錄永久可查。
//!
//! 補償的錯誤優先序(依步驟先後,先發生者決定回應):404 使用者不存在(取鎖)
//! → 500 結帳痕跡行解碼失敗(取鎖時讀一次 `order_items`,`LineTarget` 拒收
//! CHECK 不允許的形狀;今日不可達)→ 409「點數不足」(點數反轉)→
//! `AppError::Internal` 商品不存在(回補庫存)。

use crate::error::AppError;

use super::model::OrderStatus;

/// `service::update_order_status` 對一次狀態轉移的決定。
#[derive(Debug, PartialEq, Eq)]
pub enum TransitionDecision {
    /// 同狀態:原樣回傳訂單——不 UPDATE、不寫 outbox、不通知、不補償,讓重試
    /// 成為可觀測的冪等 no-op(ADR-0007 決策 9)。
    NoOp,
    /// 合法轉移,單純翻轉狀態。
    Flip,
    /// 合法轉移,且要先撤銷結帳副作用再翻轉(Cancelled ≡ Refunded 補償語意)。
    FlipAndCompensate,
}

/// 決定 `current -> target` 這次轉移怎麼處理,優先序:同狀態 → `NoOp`(先於
/// 合法性檢查);[`OrderStatus::can_transition_to`] 不允許 → 400「cannot
/// transition order from '…' to '…'」;`compensation_required` →
/// `FlipAndCompensate`;其餘 `Flip`。純函式——呼叫端在 `FOR UPDATE` 讀到
/// `current` 之後才呼叫,鎖序不受影響。
pub fn decide_transition(
    current: &OrderStatus,
    target: &OrderStatus,
) -> Result<TransitionDecision, AppError> {
    if current == target {
        return Ok(TransitionDecision::NoOp);
    }
    if !current.can_transition_to(target) {
        return Err(AppError::BadRequest(format!(
            "cannot transition order from '{}' to '{}'",
            current.as_str(),
            target.as_str()
        )));
    }
    if compensation_required(current, target) {
        Ok(TransitionDecision::FlipAndCompensate)
    } else {
        Ok(TransitionDecision::Flip)
    }
}

/// 從 `current` 轉往 `target` 是否需要補償(點數/庫存/報名/訂閱撤銷)——
/// `current` 本身已計入營收([`OrderStatus::is_revenue`]:paid/
/// processing/completed)**且** `target` 是終態的「錢要退回去」狀態
/// (cancelled 或 refunded)。一個謂詞同時排除兩個陷阱:
/// - **same-status no-op**——same-status 對(例如 `Cancelled -> Cancelled`)
///   由 [`decide_transition`] 的同狀態 `NoOp` 先擋下,根本不會呼叫到這個
///   謂詞;即使直接呼叫,current 為 Cancelled/Refunded 時 `is_revenue()` 也
///   已經先判 false,兩層防線指向同一個結論。
/// - **pending -> cancelled**——`Pending` 從未成交、不計營收,取消它只是
///   單純的狀態翻轉,沒有東西可撤銷。
fn compensation_required(current: &OrderStatus, target: &OrderStatus) -> bool {
    current.is_revenue() && matches!(target, OrderStatus::Cancelled | OrderStatus::Refunded)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- decide_transition ---

    #[test]
    fn decide_transition_covers_all_36_status_pairs() {
        // Every (current, target) pair over the 6 statuses — `None` means
        // the 400 "cannot transition" rejection. Same-status pairs are the
        // idempotent NoOp (checked before legality); the 4 compensating
        // edges are exactly the legal revenue -> cancelled/refunded ones.
        // `Processing -> Cancelled` and `Completed -> Cancelled` would
        // compensate but are illegal, so they 400 before compensation is
        // ever considered.
        use OrderStatus::*;
        use TransitionDecision::*;
        let table: [(OrderStatus, OrderStatus, Option<TransitionDecision>); 36] = [
            (Pending, Pending, Some(NoOp)),
            (Pending, Paid, None),
            (Pending, Processing, None),
            (Pending, Completed, None),
            (Pending, Cancelled, Some(Flip)),
            (Pending, Refunded, None),
            (Paid, Pending, None),
            (Paid, Paid, Some(NoOp)),
            (Paid, Processing, Some(Flip)),
            (Paid, Completed, None),
            (Paid, Cancelled, Some(FlipAndCompensate)),
            (Paid, Refunded, Some(FlipAndCompensate)),
            (Processing, Pending, None),
            (Processing, Paid, None),
            (Processing, Processing, Some(NoOp)),
            (Processing, Completed, Some(Flip)),
            (Processing, Cancelled, None),
            (Processing, Refunded, Some(FlipAndCompensate)),
            (Completed, Pending, None),
            (Completed, Paid, None),
            (Completed, Processing, None),
            (Completed, Completed, Some(NoOp)),
            (Completed, Cancelled, None),
            (Completed, Refunded, Some(FlipAndCompensate)),
            (Cancelled, Pending, None),
            (Cancelled, Paid, None),
            (Cancelled, Processing, None),
            (Cancelled, Completed, None),
            (Cancelled, Cancelled, Some(NoOp)),
            (Cancelled, Refunded, None),
            (Refunded, Pending, None),
            (Refunded, Paid, None),
            (Refunded, Processing, None),
            (Refunded, Completed, None),
            (Refunded, Cancelled, None),
            (Refunded, Refunded, Some(NoOp)),
        ];

        for (current, target, want) in table {
            let got = decide_transition(&current, &target);
            match want {
                Some(decision) => assert_eq!(
                    got.as_ref().ok(),
                    Some(&decision),
                    "{current:?} -> {target:?}: got {got:?}"
                ),
                None => {
                    let expected = format!(
                        "cannot transition order from '{}' to '{}'",
                        current.as_str(),
                        target.as_str()
                    );
                    assert!(
                        matches!(got, Err(AppError::BadRequest(ref m)) if *m == expected),
                        "{current:?} -> {target:?}: got {got:?}"
                    );
                }
            }
        }
    }
}
