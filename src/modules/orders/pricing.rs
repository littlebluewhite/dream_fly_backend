//! 訂單定價 (Order Pricing) — the five arithmetic steps of `checkout`, pulled
//! out of the transaction body into a pure function: line subtotal -> coupon
//! discount clamp -> after-coupon total -> points-redemption cap -> final
//! total -> points earned. These are domain rules with contract weight, but
//! none of them need a database or a transaction to compute — only the cart
//! lines, an already-validated coupon, and a points balance the caller
//! already resolved.
//!
//! `checkout` (`orders::service`) still owns everything genuinely
//! transactional/orchestration: loading and validating the coupon code
//! (`find_valid_by_code_tx`, 422 on an unknown/inactive/expired code),
//! locking the points balance (`points::service::lock_balance_tx`, `FOR
//! UPDATE`, 404 on a missing user), lock ordering, stock decrement,
//! order/order_items creation, enrolment/subscription artifacts, the points
//! ledger, idempotency, and the outbox. This module only prices an
//! already-assembled cart — pure function, zero DB, zero async, same shape
//! as `utils::studio_clock`.

use crate::error::AppError;
use crate::modules::cart::model::{CheckoutLine, checked_line_subtotal};
use crate::modules::coupons::model::Coupon;

/// Everything `checkout` needs from pricing to create the order row and
/// drive the points ledger.
#[derive(Debug)]
pub struct PricingOutcome {
    pub subtotal_cents: i64,
    pub discount_cents: i64,
    pub applied_coupon_code: Option<String>,
    pub points_used: i64,
    pub total_cents: i64,
    pub points_earned: i64,
}

/// Price a checkout. `coupon` must already be the validated row for a
/// caller-supplied code — an unknown/inactive/expired code is a
/// checkout-time 422 at load, before this function is ever called (see
/// `orders::service::checkout`). `points_balance` is the caller's `FOR
/// UPDATE`-locked balance when `use_points`, or `0` when not: passing `0`
/// alongside `use_points = false` is exactly equivalent to skipping
/// redemption, since `points_used` only reads `points_balance` inside the
/// `use_points` branch below.
pub fn price(
    lines: &[CheckoutLine],
    coupon: Option<&Coupon>,
    points_balance: i64,
    use_points: bool,
) -> Result<PricingOutcome, AppError> {
    // 1. Subtotal: checked per-line multiply + checked running sum.
    let mut subtotal_cents: i64 = 0;
    for item in lines {
        let line = checked_line_subtotal(item.price_cents, item.quantity)
            .ok_or_else(|| AppError::Validation("order total overflow".into()))?;
        subtotal_cents = subtotal_cents
            .checked_add(line)
            .ok_or_else(|| AppError::Validation("order total overflow".into()))?;
    }

    // 2 & 3. Coupon discount, clamped to the subtotal so a coupon larger
    //    than the cart can never drive the payable amount below zero. This
    //    application-level clamp is now the *only* upper bound: the
    //    `orders` table's CHECK constraint (`orders_discount_nonneg`) only
    //    enforces `discount_cents >= 0`. The old `discount_cents <=
    //    total_cents` bound was dropped by migration
    //    20260704000002_relax_discount_bound.sql because it compared
    //    against the post-discount total and rejected legitimate 100%-off
    //    coupons.
    let mut discount_cents: i64 = 0;
    let mut applied_coupon_code: Option<String> = None;
    if let Some(coupon) = coupon {
        discount_cents = coupon.discount_cents.min(subtotal_cents);
        applied_coupon_code = Some(coupon.code.clone());
    }
    let after_coupon_cents = subtotal_cents - discount_cents;

    // 4. Points redemption cap: at most what the (post-coupon) payable
    //    amount is worth in points, at most the caller's balance.
    let mut points_used: i64 = 0;
    if use_points {
        let max_points_by_amount = after_coupon_cents / 100;
        points_used = points_balance.min(max_points_by_amount);
    }

    // 5. Total, then earn — 5% of the final total, rounded to the nearest
    //    point.
    let total_cents = after_coupon_cents - points_used * 100;
    let total_nt = total_cents / 100;
    let points_earned = (total_nt * 5 + 50) / 100;

    Ok(PricingOutcome {
        subtotal_cents,
        discount_cents,
        applied_coupon_code,
        points_used,
        total_cents,
        points_earned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    use crate::modules::cart::model::CartItemType;

    fn line(price_cents: i64, quantity: i32) -> CheckoutLine {
        CheckoutLine {
            item_type: CartItemType::Product,
            product_id: Some(Uuid::now_v7()),
            course_id: None,
            quantity,
            price_cents,
            name: "Test Item".to_string(),
        }
    }

    fn coupon(discount_cents: i64) -> Coupon {
        Coupon {
            id: Uuid::now_v7(),
            code: "TESTCODE".to_string(),
            discount_cents,
            is_active: true,
            expires_at: None,
            created_at: Utc::now(),
        }
    }

    // --- golden cases (expected values read from tests/service_orders.rs) ---

    #[test]
    fn no_coupon_no_points_basic_case() {
        // checkout_creates_order_and_clears_cart (tests/service_orders.rs:28-49):
        // price 1500 x2 = 3000, no coupon, use_points=false (checkout passes
        // points_balance=0 in this case) -> total 3000, earns
        // (30*5+50)/100 = 2.
        let lines = [line(1500, 2)];
        let outcome = price(&lines, None, 0, false).expect("prices");
        assert_eq!(outcome.subtotal_cents, 3000);
        assert_eq!(outcome.discount_cents, 0);
        assert_eq!(outcome.applied_coupon_code, None);
        assert_eq!(outcome.points_used, 0);
        assert_eq!(outcome.total_cents, 3000);
        assert_eq!(outcome.points_earned, 2);
    }

    #[test]
    fn large_coupon_clamps_to_subtotal_and_earns_nothing() {
        // checkout_coupon_at_or_above_subtotal_clamps_to_free_order
        // (tests/service_orders.rs:248-273): subtotal 5000, coupon 10000
        // (double the subtotal) -> discount clamps to 5000, total 0, and a
        // free order earns 0 points.
        let lines = [line(5_000, 1)];
        let c = coupon(10_000);
        let outcome = price(&lines, Some(&c), 0, false).expect("prices");
        assert_eq!(outcome.subtotal_cents, 5_000);
        assert_eq!(
            outcome.discount_cents, 5_000,
            "discount clamps to the subtotal"
        );
        assert_eq!(outcome.applied_coupon_code, Some("TESTCODE".to_string()));
        assert_eq!(outcome.total_cents, 0);
        assert_eq!(outcome.points_earned, 0, "a free order earns no points");
    }

    #[test]
    fn points_cap_at_balance_when_balance_is_the_binding_constraint() {
        // checkout_use_points_caps_at_balance (tests/service_orders.rs:329-352):
        // subtotal 300_000, no coupon, balance 500 < max_points_by_amount
        // (3000) -> points_used caps at the balance, not the amount.
        let lines = [line(300_000, 1)];
        let outcome = price(&lines, None, 500, true).expect("prices");
        assert_eq!(outcome.points_used, 500);
        assert_eq!(outcome.total_cents, 300_000 - 50_000);
    }

    #[test]
    fn points_cap_at_after_coupon_amount_when_balance_exactly_covers_it() {
        // checkout_coupon_plus_points_can_reach_zero_total
        // (tests/service_orders.rs:277-303): subtotal 20_000, coupon 10_000
        // -> after_coupon 10_000, balance 100 == max_points_by_amount (100)
        // -> points_used 100, total 0, earns 0.
        let lines = [line(20_000, 1)];
        let c = coupon(10_000);
        let outcome = price(&lines, Some(&c), 100, true).expect("prices");
        assert_eq!(outcome.discount_cents, 10_000);
        assert_eq!(outcome.points_used, 100);
        assert_eq!(outcome.total_cents, 0);
        assert_eq!(outcome.points_earned, 0);
    }

    #[test]
    fn points_cap_at_amount_when_balance_is_strictly_ample() {
        // Constructed (not sourced from an integration test — none of the
        // existing coupon/points cases leave the balance strictly above the
        // amount cap): balance 1000 is well above max_points_by_amount
        // (100), so the amount caps points_used, not the ample balance —
        // the mirror image of the "balance is the binding constraint" case
        // above.
        let lines = [line(10_000, 1)];
        let outcome = price(&lines, None, 1_000, true).expect("prices");
        assert_eq!(
            outcome.points_used, 100,
            "capped at after_coupon/100, not the ample balance"
        );
        assert_eq!(outcome.total_cents, 0);
    }

    #[test]
    fn use_points_false_ignores_a_nonzero_balance_passed_in() {
        // `checkout` always passes points_balance=0 when use_points=false
        // (it never locks the row), but the function itself must not rely
        // on callers' discipline: a nonzero balance is still ignored when
        // use_points is false.
        let lines = [line(1000, 1)];
        let outcome = price(&lines, None, 999, false).expect("prices");
        assert_eq!(outcome.points_used, 0);
        assert_eq!(outcome.total_cents, 1000);
    }

    // --- overflow ---

    #[test]
    fn line_multiply_overflow_is_rejected() {
        let lines = [line(i64::MAX, 2)];
        let err = price(&lines, None, 0, false).expect_err("must overflow");
        assert!(
            matches!(err, AppError::Validation(ref m) if m == "order total overflow"),
            "got: {err:?}"
        );
    }

    #[test]
    fn running_sum_overflow_is_rejected() {
        let lines = [line(i64::MAX, 1), line(1, 1)];
        let err = price(&lines, None, 0, false).expect_err("must overflow");
        assert!(
            matches!(err, AppError::Validation(ref m) if m == "order total overflow"),
            "got: {err:?}"
        );
    }

    // --- boundaries ---

    #[test]
    fn points_used_integer_division_floors_at_boundary() {
        // after_coupon = 150 -> 150/100 = 1 (integer division floors, it
        // does not round) — a sufficient balance (5) means the division is
        // the binding constraint, not the balance.
        let lines = [line(150, 1)];
        let outcome = price(&lines, None, 5, true).expect("prices");
        assert_eq!(outcome.points_used, 1);
    }

    #[test]
    fn points_earned_rounds_half_up_at_the_boundary() {
        // total_nt=29 -> 5% = 1.45, the "+50" formula rounds this down to
        // 1; total_nt=30 -> 5% = 1.5, the same formula rounds this up to 2
        // (the "30" side is also covered end-to-end by the basic-case
        // golden test above; this isolates the boundary pair on the
        // formula alone).
        let below = price(&[line(2_900, 1)], None, 0, false).expect("prices");
        assert_eq!(below.points_earned, 1, "29 NT * 5% = 1.45 rounds down to 1");

        let at = price(&[line(3_000, 1)], None, 0, false).expect("prices");
        assert_eq!(at.points_earned, 2, "30 NT * 5% = 1.5 rounds up to 2");
    }
}
