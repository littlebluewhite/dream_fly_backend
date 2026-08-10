//! 行計畫 (Line Fulfilment) — the item_type dispatch of `checkout`, pulled
//! out of the transaction body into a pure function. `checkout` used to walk
//! the cart snapshot twice with two mutually-exclusive
//! `.filter(matches!(item.item_type, ...))` passes — one to gather product
//! lines for stock reservation, one to gather course lines for enrolment.
//! `plan()` replaces both with a single **exhaustive** `match` over
//! `CartItemType`, splitting the lines into a pre-shaped
//! [`FulfilmentPlan`] the caller consumes without ever matching on
//! `item_type` again. Same "owned struct output" shape as
//! [`super::pricing`]'s `PricingOutcome`: the fields checkout needs
//! (`product_id`, `quantity`, `price_cents`, `name` for products;
//! `course_id` for courses) are copied out here so no `item_type` branch —
//! and no `Option` unwrap — survives downstream.
//!
//! The match is exhaustive on purpose (no `_` arm): a future `CartItemType`
//! variant is a compile error *here*, at the one place that must decide how
//! a new kind of line gets fulfilled, rather than silently falling through a
//! wildcard into "reserved as nothing, enrolled as nothing". A `Product`
//! line missing its `product_id`, or a `Course` line missing its
//! `course_id`, is `AppError::Internal` — the `cart_items_one_target` CHECK
//! (migration `20260704000001`) makes this unreachable today, so this
//! upgrades the old `.expect()` panic to a 500 without changing any
//! reachable behavior.
//!
//! **Ordering is deliberately NOT this function's job.** The write-reservation
//! order discipline (product lines sorted by `product_id` before the stock
//! UPDATE; course lines sorted by `course_id` before the enrolment lock) is
//! owned by `products::service::reserve_stock_tx` and
//! `enrolments::service::enrol_batch_from_purchase_tx` respectively — each
//! sorts its own copy right before it takes the write locks that the order
//! exists to serialize. `plan()` preserves the input slice's order verbatim
//! (cart-creation order, product lines then course lines, per
//! `cart::repository::find_cart_items_for_checkout_tx`). One invariant with
//! two owners would be worse than none: the sort lives with the lock it
//! protects, not scattered here as well. Pure function, zero DB, zero async
//! — same shape as `super::pricing` and `utils::studio_clock`.

use std::collections::HashMap;

use uuid::Uuid;

use crate::error::AppError;
use crate::modules::cart::model::{CartItemType, CheckoutLine};
use crate::modules::products::model::Product;

/// One product line resolved for fulfilment: `product_id` unwrapped from the
/// cart snapshot's `Option`, plus the `quantity`/`price_cents`/`name` the
/// reservation and subscription-grant steps need. `name` is owned (`String`)
/// rather than borrowed for the same reason `pricing` returns owned values:
/// a borrow would infect every downstream signature with a lifetime
/// parameter.
#[derive(Debug)]
pub struct ProductFulfilment {
    pub product_id: Uuid,
    pub quantity: i32,
    pub price_cents: i64,
    pub name: String,
}

/// Everything `checkout` needs to drive fulfilment after pricing: product
/// lines to reserve stock for (and grant subscriptions from), and course ids
/// to enrol. Both are in the cart snapshot's original order — see the module
/// doc on why ordering is not resolved here.
#[derive(Debug)]
pub struct FulfilmentPlan {
    pub products: Vec<ProductFulfilment>,
    pub course_ids: Vec<Uuid>,
}

/// Split a checkout's cart snapshot into its fulfilment plan. `lines` is the
/// exact slice `pricing::price` was just handed, so this runs after the
/// coupon 422 and the subtotal-overflow 422 — an unreachable `Internal` from
/// a target-less line can never mask those. See the module doc for why the
/// match is exhaustive and why nothing is sorted here.
pub fn plan(lines: &[CheckoutLine]) -> Result<FulfilmentPlan, AppError> {
    let mut products = Vec::new();
    let mut course_ids = Vec::new();

    for line in lines {
        match line.item_type {
            CartItemType::Product => {
                let product_id = line.product_id.ok_or_else(|| {
                    AppError::Internal(anyhow::anyhow!("product line missing product_id"))
                })?;
                products.push(ProductFulfilment {
                    product_id,
                    quantity: line.quantity,
                    price_cents: line.price_cents,
                    name: line.name.clone(),
                });
            }
            CartItemType::Course => {
                let course_id = line.course_id.ok_or_else(|| {
                    AppError::Internal(anyhow::anyhow!("course line missing course_id"))
                })?;
                course_ids.push(course_id);
            }
        }
    }

    Ok(FulfilmentPlan {
        products,
        course_ids,
    })
}

/// Purchasability gate (甲案), run by `orders::service::checkout` on the raw
/// cart snapshot BEFORE `plan()`/`pricing::price` ever see it. `cart::
/// repository::find_cart_items_for_checkout_tx` deliberately no longer
/// filters its snapshot by `is_active` (see that function's doc), so a
/// deactivated product/course line comes back like any other line instead
/// of silently vanishing — this is the one place that turns "vanished" back
/// into "rejected, by name". Collects every `!is_active` line's name, in
/// the slice's own order (the cart snapshot's order — no sorting), and
/// rejects the WHOLE batch if that list is non-empty: a single stale line
/// blocks checkout of the entire cart, not just itself, matching this
/// module's existing all-or-nothing posture (a full course/duplicate
/// enrolment already rolls back the entire checkout — see `plan`'s module
/// doc). An all-active slice (including the empty slice) is `Ok(())`; once
/// this returns `Ok`, every line in `lines` is guaranteed active, and
/// nothing downstream (`plan`, `pricing::price`, the `items_data` snapshot
/// in `orders::service::checkout`) needs to look at `is_active` again.
pub fn ensure_all_purchasable(lines: &[CheckoutLine]) -> Result<(), AppError> {
    let names: Vec<String> = lines
        .iter()
        .filter(|line| !line.is_active)
        .map(|line| format!("「{}」", line.name))
        .collect();

    if names.is_empty() {
        return Ok(());
    }

    Err(AppError::Validation(format!(
        "以下項目已下架,請先自購物車移除再結帳:{}",
        names.join("、")
    )))
}

/// One order line ready for `repository::create_order_items`: the checkout
/// snapshot's `product_id`/`course_id`/`quantity`/`price_cents`/`name`
/// carried over verbatim, plus the `stock_decremented` bit `order_lines`
/// derives below. Named replacement for the anonymous six-tuple
/// `(Option<Uuid>, Option<Uuid>, i32, i64, String, bool)` `checkout` used to
/// build inline, field-for-field in the same order.
#[derive(Debug)]
pub struct OrderLine {
    pub product_id: Option<Uuid>,
    pub course_id: Option<Uuid>,
    pub quantity: i32,
    pub price_cents: i64,
    pub name: String,
    pub stock_decremented: bool,
}

/// Turn a checkout's cart snapshot into named order lines — `plan()`'s
/// sister pure function, and the single owner of the `stock_decremented`
/// derivation rule that used to live in an unnamed closure inside
/// `service::checkout` (not unit-testable there). `lines` is the same slice
/// `plan()` above just consumed; `reserved` is
/// `products::service::reserve_stock_tx`'s result (step 6 of
/// `service::checkout`) — the post-decrement row for every product line
/// that got reserved.
///
/// `stock_decremented` is `true` only when the line is a product line
/// (`product_id.is_some()`) *and* its id is in `reserved` *and* that row's
/// `stock` is `Some(_)` — finite stock, so the decrement actually moved
/// something (`None` means unlimited stock, untouched by
/// `try_decrement_stock_tx`'s NULL-preserving CASE, `products/
/// repository.rs`). A course line never carries a `product_id`, so it can
/// never reach `reserved` and is always `false`; a product line whose id is
/// missing from `reserved` (unreachable today — every product line is
/// reserved in step 6 before this runs) is also `false` — the
/// `.unwrap_or(false)` this replaces. Output preserves `lines`' order, same
/// "no sorting here" posture as `plan()`.
pub fn order_lines(lines: &[CheckoutLine], reserved: &HashMap<Uuid, Product>) -> Vec<OrderLine> {
    lines
        .iter()
        .map(|line| {
            let stock_decremented = line
                .product_id
                .and_then(|pid| reserved.get(&pid))
                .map(|p| p.stock.is_some())
                .unwrap_or(false);
            OrderLine {
                product_id: line.product_id,
                course_id: line.course_id,
                quantity: line.quantity,
                price_cents: line.price_cents,
                name: line.name.clone(),
                stock_decremented,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::products::model::ProductType;
    use chrono::Utc;

    fn product_line(name: &str) -> CheckoutLine {
        CheckoutLine {
            item_type: CartItemType::Product,
            product_id: Some(Uuid::now_v7()),
            course_id: None,
            quantity: 2,
            price_cents: 1500,
            name: name.to_string(),
            is_active: true,
        }
    }

    fn course_line() -> CheckoutLine {
        CheckoutLine {
            item_type: CartItemType::Course,
            product_id: None,
            course_id: Some(Uuid::now_v7()),
            quantity: 1,
            price_cents: 8000,
            name: "Course".to_string(),
            is_active: true,
        }
    }

    #[test]
    fn splits_a_mixed_cart_into_products_and_courses() {
        // The whole point: one exhaustive pass replaces checkout's two
        // `.filter(matches!)` walks. A cart with both kinds of line lands
        // each in its own bucket, carrying the fields fulfilment needs.
        let p = product_line("Widget");
        let c = course_line();
        let (want_pid, want_cid) = (p.product_id.unwrap(), c.course_id.unwrap());
        let lines = [p, c];

        let plan = plan(&lines).expect("plans");

        assert_eq!(plan.products.len(), 1);
        assert_eq!(plan.products[0].product_id, want_pid);
        assert_eq!(plan.products[0].quantity, 2);
        assert_eq!(plan.products[0].price_cents, 1500);
        assert_eq!(plan.products[0].name, "Widget");
        assert_eq!(plan.course_ids, vec![want_cid]);
    }

    #[test]
    fn products_only_cart_has_no_course_ids() {
        let lines = [product_line("A"), product_line("B")];
        let plan = plan(&lines).expect("plans");
        assert_eq!(plan.products.len(), 2);
        assert!(plan.course_ids.is_empty());
    }

    #[test]
    fn courses_only_cart_has_no_products() {
        let lines = [course_line(), course_line()];
        let plan = plan(&lines).expect("plans");
        assert!(plan.products.is_empty());
        assert_eq!(plan.course_ids.len(), 2);
    }

    #[test]
    fn preserves_input_order_within_each_bucket() {
        // `plan()` does NOT sort — the write-reservation order is imposed by
        // `reserve_stock_tx`/`enrol_batch_from_purchase_tx` right before they
        // take their locks (see the module doc). Here the products come back
        // in the exact slice order they went in, not sorted by product_id.
        let a = product_line("first");
        let b = product_line("second");
        let (id_a, id_b) = (a.product_id.unwrap(), b.product_id.unwrap());
        let lines = [a, b];

        let plan = plan(&lines).expect("plans");

        assert_eq!(plan.products[0].product_id, id_a, "first stays first");
        assert_eq!(plan.products[1].product_id, id_b, "second stays second");
    }

    #[test]
    fn product_line_without_product_id_is_internal_error() {
        // Unreachable under the `cart_items_one_target` CHECK — this is the
        // upgrade of the old `.expect()` panic to a 500.
        let line = CheckoutLine {
            item_type: CartItemType::Product,
            product_id: None,
            course_id: None,
            quantity: 1,
            price_cents: 100,
            name: "orphan".to_string(),
            is_active: true,
        };
        let err = plan(&[line]).expect_err("must be Internal");
        assert!(matches!(err, AppError::Internal(_)), "got: {err:?}");
    }

    #[test]
    fn course_line_without_course_id_is_internal_error() {
        let line = CheckoutLine {
            item_type: CartItemType::Course,
            product_id: None,
            course_id: None,
            quantity: 1,
            price_cents: 100,
            name: "orphan".to_string(),
            is_active: true,
        };
        let err = plan(&[line]).expect_err("must be Internal");
        assert!(matches!(err, AppError::Internal(_)), "got: {err:?}");
    }

    #[test]
    fn empty_cart_yields_an_empty_plan() {
        let plan = plan(&[]).expect("plans");
        assert!(plan.products.is_empty());
        assert!(plan.course_ids.is_empty());
    }

    // --- ensure_all_purchasable (甲案 gate) ---

    #[test]
    fn ensure_all_purchasable_ok_when_every_line_is_active() {
        let p = product_line("Widget");
        let c = course_line();
        assert!(ensure_all_purchasable(&[p, c]).is_ok());
    }

    #[test]
    fn ensure_all_purchasable_rejects_single_inactive_line_with_exact_message() {
        let mut line = product_line("Retired Widget");
        line.is_active = false;

        let err = ensure_all_purchasable(&[line]).expect_err("must reject");
        assert!(
            matches!(
                err,
                AppError::Validation(ref m)
                    if m == "以下項目已下架,請先自購物車移除再結帳:「Retired Widget」"
            ),
            "got: {err:?}"
        );
    }

    #[test]
    fn ensure_all_purchasable_lists_multiple_inactive_lines_in_snapshot_order() {
        // Snapshot order, not sorted — "Zebra" precedes "Apple" in the slice,
        // so it must precede it in the message too. The active line sitting
        // in between must not appear in the list at all.
        let mut first = product_line("Zebra");
        first.is_active = false;
        let mut middle = course_line();
        middle.is_active = true;
        let mut last = product_line("Apple");
        last.is_active = false;

        let err = ensure_all_purchasable(&[first, middle, last]).expect_err("must reject");
        assert!(
            matches!(
                err,
                AppError::Validation(ref m)
                    if m == "以下項目已下架,請先自購物車移除再結帳:「Zebra」、「Apple」"
            ),
            "got: {err:?}"
        );
    }

    // --- order_lines ---

    /// Local fixture mirroring `products::model`'s `fixture_product` shape
    /// — only `id`/`stock` vary per case, everything else is filler.
    fn fixture_product(id: Uuid, stock: Option<i32>) -> Product {
        Product {
            id,
            name: "Test Product".into(),
            slug: "test-product".into(),
            product_type: ProductType::Merchandise,
            description: None,
            price_cents: 1000,
            original_price_cents: None,
            features: vec![],
            is_highlighted: false,
            badge: None,
            stock,
            valid_days: None,
            session_count: None,
            is_active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn order_lines_product_with_finite_stock_is_stock_decremented() {
        // Finite stock (`stock: Some(n)`) on the reserved row means the
        // decrement actually moved something. Also pins the field-for-field
        // carry-over (name/quantity/price_cents/course_id) while we're here.
        let line = product_line("Widget");
        let pid = line.product_id.unwrap();
        let mut reserved = HashMap::new();
        reserved.insert(pid, fixture_product(pid, Some(4)));

        let lines = order_lines(&[line], &reserved);

        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].stock_decremented,
            "finite stock must count as decremented"
        );
        assert_eq!(lines[0].product_id, Some(pid));
        assert_eq!(lines[0].course_id, None);
        assert_eq!(lines[0].quantity, 2);
        assert_eq!(lines[0].price_cents, 1500);
        assert_eq!(lines[0].name, "Widget");
    }

    #[test]
    fn order_lines_product_with_unlimited_stock_is_not_stock_decremented() {
        // `stock: None` means unlimited — `try_decrement_stock_tx`'s
        // NULL-preserving CASE never touches it.
        let line = product_line("Widget");
        let pid = line.product_id.unwrap();
        let mut reserved = HashMap::new();
        reserved.insert(pid, fixture_product(pid, None));

        let lines = order_lines(&[line], &reserved);

        assert!(!lines[0].stock_decremented);
    }

    #[test]
    fn order_lines_course_line_is_never_stock_decremented() {
        // A course line never carries a `product_id`, so it can never reach
        // `reserved` at all — always false, regardless of what `reserved`
        // contains (here, empty).
        let line = course_line();
        let cid = line.course_id;
        let reserved: HashMap<Uuid, Product> = HashMap::new();

        let lines = order_lines(&[line], &reserved);

        assert!(!lines[0].stock_decremented);
        assert_eq!(lines[0].product_id, None);
        assert_eq!(lines[0].course_id, cid);
    }

    #[test]
    fn order_lines_preserves_input_order_for_mixed_lines() {
        // product/course interleaved — output must land in the same slice
        // order as the input, same "no sorting here" posture as `plan()`.
        let a = product_line("first");
        let b = course_line();
        let c = product_line("second");
        let (pid_a, cid_b, pid_c) = (
            a.product_id.unwrap(),
            b.course_id.unwrap(),
            c.product_id.unwrap(),
        );
        let mut reserved = HashMap::new();
        reserved.insert(pid_a, fixture_product(pid_a, Some(1)));
        reserved.insert(pid_c, fixture_product(pid_c, Some(1)));

        let lines = order_lines(&[a, b, c], &reserved);

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].product_id, Some(pid_a), "first stays first");
        assert_eq!(lines[1].course_id, Some(cid_b), "second stays second");
        assert_eq!(lines[2].product_id, Some(pid_c), "third stays third");
    }

    #[test]
    fn order_lines_product_missing_from_reserved_is_not_stock_decremented() {
        // Not reachable in practice today (every product line is reserved
        // before `order_lines` runs) — matches the `.unwrap_or(false)` this
        // replaces.
        let line = product_line("Widget");
        let reserved: HashMap<Uuid, Product> = HashMap::new();

        let lines = order_lines(&[line], &reserved);

        assert!(!lines[0].stock_decremented);
    }
}
