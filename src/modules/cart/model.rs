use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AppError;

/// Discriminates whether a cart (or checkout) line targets a product or a
/// course. Maps to the Postgres `cart_item_type` enum.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "cart_item_type", rename_all = "snake_case")]
pub enum CartItemType {
    Product,
    Course,
}

impl CartItemType {
    /// The SQL string literal for this variant. The Postgres `cart_item_type`
    /// enum, the `item_type` columns, and this method must all agree on these
    /// two spellings, and — outside `fulfilment::plan` — the type system
    /// cannot enforce that: the spellings are hand-written into SQL at a
    /// spread of sites.
    ///
    /// SQL-literal sites, by function (each hard-codes `'product'`/`'course'`):
    /// - `cart::repository::add_product_item` — `'product'::cart_item_type` on insert
    /// - `cart::repository::add_course_item` — `'course'::cart_item_type` on insert
    /// - `cart::repository::find_cart_items_for_checkout_tx` — ×4: the
    ///   `'product'`/`'course'` SELECT literal plus the `item_type = '…'`
    ///   filter, once in each of the two (product, course) branch queries
    /// - `orders::repository::create_order_items` — the `CASE WHEN
    ///   u.product_id IS NOT NULL THEN 'product' ELSE 'course' END` derivation
    /// - `reports::repository` income-source `CASE` — maps `oi.item_type =
    ///   'course'` into the `course` revenue bucket
    /// - `bin/seed.rs` order-line `CASE` — the same product/course derivation
    ///   for the deterministic reporting dataset
    ///
    /// Adding a variant — the full checklist:
    /// 1. `ALTER TYPE cart_item_type ADD VALUE '…'` migration.
    /// 2. Re-sync the `cart_items` target + quantity CHECKs
    ///    (`cart_items_one_target`, `cart_items_course_qty`; migration
    ///    `20260704000001`, lines 34–49) — a new target column and its
    ///    exclusivity/quantity rules.
    /// 3. Every SQL-literal site listed above.
    /// 4. `orders::fulfilment::plan`'s exhaustive `match` — the compiler forces
    ///    this one (no `_` arm); it is the only site the type system catches.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Product => "product",
            Self::Course => "course",
        }
    }

    /// Single owner of the per-type quantity rule: `Product` allows
    /// `1..=999`, `Course` allows only `1`. Error variant/message are
    /// unchanged from the call sites this replaces (`cart::service`'s
    /// `add_product_item`, `add_course_item`, and the two post-lookup
    /// branches of `update_quantity`).
    ///
    /// `update_quantity`'s pre-lookup guard is a separate, deliberately
    /// duplicated inline check — see the comment there for why it isn't
    /// just a call to this method.
    pub fn validate_quantity(&self, qty: i32) -> Result<(), AppError> {
        match self {
            Self::Product => {
                if !(1..=999).contains(&qty) {
                    return Err(AppError::BadRequest(
                        "quantity must be between 1 and 999".into(),
                    ));
                }
            }
            Self::Course => {
                if qty != 1 {
                    return Err(AppError::Validation("course quantity must be 1".into()));
                }
            }
        }
        Ok(())
    }
}

impl std::str::FromStr for CartItemType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "product" => Ok(Self::Product),
            "course" => Ok(Self::Course),
            _ => Err(()),
        }
    }
}

/// Raw `cart_items` row. Exactly one of `product_id`/`course_id` is set,
/// matching `item_type` (enforced by the `cart_items_one_target` CHECK).
#[derive(Debug, sqlx::FromRow)]
pub struct CartItem {
    pub id: Uuid,
    pub user_id: Uuid,
    pub item_type: CartItemType,
    pub product_id: Option<Uuid>,
    pub course_id: Option<Uuid>,
    pub quantity: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Cart row joined against whichever table `item_type` targets, to surface
/// the display name/slug/price for `CartResponse`.
#[derive(Debug, sqlx::FromRow)]
pub struct CartItemJoined {
    pub id: Uuid,
    pub user_id: Uuid,
    pub item_type: CartItemType,
    pub product_id: Option<Uuid>,
    pub course_id: Option<Uuid>,
    pub quantity: i32,
    pub name: String,
    pub slug: String,
    pub price_cents: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Cart line snapshot consumed by `orders::service::checkout` to build order
/// items. Produced by `repository::find_cart_items_for_checkout_tx`.
#[derive(Debug, sqlx::FromRow)]
pub struct CheckoutLine {
    pub item_type: CartItemType,
    pub product_id: Option<Uuid>,
    pub course_id: Option<Uuid>,
    pub quantity: i32,
    pub price_cents: i64,
    pub name: String,
}

/// 行小計的溢位安全乘法——pricing 與 CartResponse 共用,溢位文案各自保留。
pub fn checked_line_subtotal(price_cents: i64, quantity: i32) -> Option<i64> {
    price_cents.checked_mul(quantity as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- validate_quantity: Product (1..=999) ---

    #[test]
    fn product_quantity_within_1_to_999_is_ok() {
        assert!(CartItemType::Product.validate_quantity(1).is_ok());
        assert!(CartItemType::Product.validate_quantity(500).is_ok());
        assert!(CartItemType::Product.validate_quantity(999).is_ok());
    }

    #[test]
    fn product_quantity_outside_1_to_999_is_bad_request() {
        for qty in [i32::MIN, -1, 0, 1000, i32::MAX] {
            let err = CartItemType::Product
                .validate_quantity(qty)
                .expect_err("must reject");
            assert!(
                matches!(err, AppError::BadRequest(ref m) if m == "quantity must be between 1 and 999"),
                "got: {err:?} for qty={qty}"
            );
        }
    }

    // --- validate_quantity: Course (== 1) ---

    #[test]
    fn course_quantity_of_one_is_ok() {
        assert!(CartItemType::Course.validate_quantity(1).is_ok());
    }

    #[test]
    fn course_quantity_other_than_one_is_validation_error() {
        for qty in [i32::MIN, -1, 0, 2, 999, 1000, i32::MAX] {
            let err = CartItemType::Course
                .validate_quantity(qty)
                .expect_err("must reject");
            assert!(
                matches!(err, AppError::Validation(ref m) if m == "course quantity must be 1"),
                "got: {err:?} for qty={qty}"
            );
        }
    }
}
