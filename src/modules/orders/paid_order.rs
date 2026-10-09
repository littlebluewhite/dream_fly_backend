//! 已付訂單落帳 (Paid Order Record) — 一張已付訂單的訂單列、訂單行與結帳
//! 點數帳，在同一個付款時間 `paid_at` 下一次寫齊。
//!
//! 「三者同一個時間」過去只活在 `orders::service::checkout` 的語句順序裡；
//! [`record_paid_order_tx`] 把它收成單一入口，呼叫端只決定付款時間（業務
//! 時間，ADR-0017），不再各自拼訂單列、訂單行與點數帳。報名與訂閱授權不在
//! 這裡：它們依賴鎖 witness 與庫存預留結果，仍由 checkout 編排。
//!
//! 買家以 [`BalanceLock`] 傳入而不是 `user_id`：點數帳只動買家的 `users`
//! 列，要求持有那把鎖的證明，「替沒鎖住的會員記帳」在型別上就做不到。
//!
//! 錯誤：sqlx 錯誤經 `AppError::Database` 上拋。`apply_delta_tx` 的業務錯誤
//! （零 delta、點數不足）在這裡走不到——`PricingOutcome::ledger_deltas` 跳過
//! 零幅度，`pricing::price` 以鎖到的餘額封頂折抵。任何錯誤都由呼叫端回滾整個
//! 交易，本函式不 commit。

use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

use crate::error::AppError;
use crate::modules::points::service::{self as points_service, BalanceLock};

use super::fulfilment::OrderLine;
use super::model::Order;
use super::pricing::PricingOutcome;
use super::repository::{self, OrderAmounts};

/// 寫入一張已付訂單：訂單列（`status = 'paid'`）→ 訂單行 → 結帳點數帳
/// （`outcome.ledger_deltas`，折抵先、賺點後），`paid_at` 同時是訂單列的
/// `paid_at`/`created_at`、訂單行與每筆點數帳的 `created_at`。
pub async fn record_paid_order_tx(
    tx: &mut Transaction<'_, Postgres>,
    buyer: &BalanceLock,
    order_number: &str,
    payment_method: &str,
    lines: &[OrderLine],
    outcome: &PricingOutcome,
    paid_at: DateTime<Utc>,
) -> Result<Order, AppError> {
    let order = repository::create_order(
        tx,
        buyer.user_id(),
        order_number,
        OrderAmounts {
            total_cents: outcome.total_cents,
            discount_cents: outcome.discount_cents,
            points_used: outcome.points_used,
            points_earned: outcome.points_earned,
        },
        outcome.applied_coupon_code.as_deref(),
        payment_method,
        paid_at,
    )
    .await?;

    repository::create_order_items(tx, order.id, lines, paid_at).await?;

    for delta in outcome.ledger_deltas(order.id) {
        points_service::apply_delta_tx(tx, buyer.user_id(), delta, paid_at).await?;
    }

    Ok(order)
}
