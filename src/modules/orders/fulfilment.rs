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

use uuid::Uuid;

use crate::error::AppError;
use crate::modules::cart::model::{CartItemType, CheckoutLine};

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

#[cfg(test)]
mod tests {
    use super::*;

    fn product_line(name: &str) -> CheckoutLine {
        CheckoutLine {
            item_type: CartItemType::Product,
            product_id: Some(Uuid::now_v7()),
            course_id: None,
            quantity: 2,
            price_cents: 1500,
            name: name.to_string(),
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
}
