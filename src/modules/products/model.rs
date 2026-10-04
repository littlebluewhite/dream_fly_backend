use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use strum::VariantArray;
use uuid::Uuid;

use crate::error::AppError;
use crate::modules::cart::model::LineTarget;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ts_rs::TS, VariantArray,
)]
#[sqlx(type_name = "product_type", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ProductType {
    Ticket,
    CoursePackage,
    Membership,
    Merchandise,
}

impl ProductType {
    /// Every variant, in wire-spelling order — single owner of the value
    /// domain; `FromStr` derives from this instead of hand-copying the list.
    pub const ALL: &[Self] = Self::VARIANTS;

    /// The SQL string literal for this variant — matches the Postgres
    /// `product_type` enum's `snake_case` spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ticket => "ticket",
            Self::CoursePackage => "course_package",
            Self::Membership => "membership",
            Self::Merchandise => "merchandise",
        }
    }
}

impl std::str::FromStr for ProductType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL.iter().find(|v| v.as_str() == s).copied().ok_or(())
    }
}

/// One product line's checkout-time stock trace, read back from
/// `order_items` (`repository::find_stock_traces_by_order_tx`) — the only
/// input refund/cancel compensation needs to undo a checkout's stock
/// decrement. `stock_decremented` is the checkout-time snapshot
/// (`orders::fulfilment::order_lines` derives it; products only reads it):
/// `false` (unlimited-stock product at checkout, or a legacy row) means
/// nothing was decremented, so nothing is restored. `target` is decoded
/// once at the row boundary (`cart::model::LineTarget`); a product line
/// without a `product_id` cannot decode (the `order_items_one_target` CHECK
/// forbids it anyway).
#[derive(Debug, sqlx::FromRow)]
pub struct OrderStockTrace {
    #[sqlx(flatten)]
    pub target: LineTarget,
    pub quantity: i32,
    pub stock_decremented: bool,
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Product {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub product_type: ProductType,
    pub description: Option<String>,
    pub price_cents: i64,
    pub original_price_cents: Option<i64>,
    pub features: Vec<String>,
    pub is_highlighted: bool,
    pub badge: Option<String>,
    pub stock: Option<i32>,
    pub valid_days: Option<i32>,
    pub session_count: Option<i32>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Upper bound of a product line's quantity (`1..=MAX_LINE_QUANTITY`).
/// Owned here with [`Product::ensure_quantity_in_range`]; the cart wire
/// DTOs bind it for their `#[validate(range)]`.
pub const MAX_LINE_QUANTITY: i32 = 999;

/// The 400 message for an out-of-range line quantity — single owner of the
/// string (tests match it), derived from [`MAX_LINE_QUANTITY`].
pub fn quantity_range_msg() -> String {
    format!("quantity must be between 1 and {MAX_LINE_QUANTITY}")
}

impl Product {
    /// The range half of [`Self::ensure_line_quantity`], for callers that
    /// must reject before any row is looked up (cart's pre-lookup guards,
    /// which keep this 400 ahead of the lookup's 404): `1..=999`, else
    /// `BadRequest` (400) with [`quantity_range_msg`].
    pub fn ensure_quantity_in_range(quantity: i32) -> Result<(), AppError> {
        if !(1..=MAX_LINE_QUANTITY).contains(&quantity) {
            return Err(AppError::BadRequest(quantity_range_msg()));
        }
        Ok(())
    }

    /// A time-based entitlement: a `membership`/`ticket` with `valid_days`
    /// set and no `session_count` — its grant is one expiry date, which
    /// can't be multiplied by a quantity (ADR-0003; the grant itself is
    /// `subscriptions::entitlement::plan`).
    pub fn is_time_based_entitlement(&self) -> bool {
        matches!(
            self.product_type,
            ProductType::Membership | ProductType::Ticket
        ) && self.session_count.is_none()
            && self.valid_days.is_some()
    }

    /// Is `quantity` a legal quantity for one line of this product — the
    /// single owner of that rule (cart add, cart update and checkout all
    /// ask here). [`Self::ensure_quantity_in_range`] (400); a time-based
    /// entitlement only at `1`, else `Validation` (422). Says nothing about
    /// `is_active` or stock — see [`Self::ensure_purchasable`].
    ///
    /// Error strings are load-bearing (substring-matched in tests):
    /// [`quantity_range_msg`],
    /// `"time-based subscription quantity must be 1"`.
    pub fn ensure_line_quantity(&self, quantity: i32) -> Result<(), AppError> {
        Self::ensure_quantity_in_range(quantity)?;
        if self.is_time_based_entitlement() && quantity != 1 {
            return Err(AppError::Validation(
                "time-based subscription quantity must be 1".into(),
            ));
        }
        Ok(())
    }

    /// Can a cart line hold `quantity` units of this product? `quantity` is
    /// the line's *whole* quantity — after a repeat add has merged into it
    /// (`cart::service::add_product_item`), or the final value of an update
    /// — never just an increment. In order: inactive → 400, then
    /// [`Self::ensure_line_quantity`] (400/422), then — if the product
    /// tracks stock at all (`stock: None` means unlimited) — `quantity`
    /// above `stock` → 409.
    ///
    /// This is a cart-time check against a stock value that can move before
    /// checkout. The authoritative stock check is the atomic decrement at
    /// checkout (`repository::try_decrement_stock_tx`, via
    /// `service::reserve_stock_tx`).
    ///
    /// Error strings are load-bearing (substring-matched in
    /// `tests/service_cart.rs`): `"product is not available"` /
    /// `BadRequest` (400), `"insufficient stock: only {stock} available"` /
    /// `Conflict` (409), plus [`Self::ensure_line_quantity`]'s two.
    pub fn ensure_purchasable(&self, quantity: i32) -> Result<(), AppError> {
        if !self.is_active {
            return Err(AppError::BadRequest("product is not available".into()));
        }

        self.ensure_line_quantity(quantity)?;

        if let Some(stock) = self.stock {
            if quantity > stock {
                return Err(AppError::Conflict(format!(
                    "insufficient stock: only {stock} available"
                )));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal fixture for `ensure_purchasable` tests — only `is_active` and
    /// `stock` are varied per case, everything else is filler.
    fn fixture_product(is_active: bool, stock: Option<i32>) -> Product {
        entitlement_product(ProductType::Merchandise, None, None, is_active, stock)
    }

    /// Fixture for the quantity-rule tests — the fields
    /// `is_time_based_entitlement` branches on, plus `is_active`/`stock`.
    fn entitlement_product(
        product_type: ProductType,
        session_count: Option<i32>,
        valid_days: Option<i32>,
        is_active: bool,
        stock: Option<i32>,
    ) -> Product {
        Product {
            id: Uuid::now_v7(),
            name: "Test Product".into(),
            slug: "test-product".into(),
            product_type,
            description: None,
            price_cents: 1000,
            original_price_cents: None,
            features: vec![],
            is_highlighted: false,
            badge: None,
            stock,
            valid_days,
            session_count,
            is_active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // --- ensure_purchasable ---

    #[test]
    fn ensure_purchasable_rejects_inactive_product() {
        let product = fixture_product(false, Some(10));
        let err = product.ensure_purchasable(1).expect_err("must reject");
        assert!(
            matches!(err, AppError::BadRequest(ref m) if m == "product is not available"),
            "got: {err:?}"
        );
    }

    #[test]
    fn ensure_purchasable_allows_any_in_range_quantity_when_stock_is_untracked() {
        let product = fixture_product(true, None);
        assert!(product.ensure_purchasable(999).is_ok());
    }

    #[test]
    fn ensure_purchasable_checks_line_quantity_before_stock() {
        // 1000 is both out of range and above stock — the range 400 wins.
        let product = fixture_product(true, Some(3));
        let err = product.ensure_purchasable(1000).expect_err("must reject");
        assert!(
            matches!(err, AppError::BadRequest(ref m) if *m == quantity_range_msg()),
            "got: {err:?}"
        );

        // A time-based entitlement at quantity 2 with stock 1: the 422 wins.
        let product = entitlement_product(ProductType::Membership, None, Some(30), true, Some(1));
        let err = product.ensure_purchasable(2).expect_err("must reject");
        assert!(matches!(err, AppError::Validation(_)), "got: {err:?}");
    }

    #[test]
    fn ensure_purchasable_checks_inactive_before_line_quantity() {
        let product = fixture_product(false, None);
        let err = product.ensure_purchasable(0).expect_err("must reject");
        assert!(
            matches!(err, AppError::BadRequest(ref m) if m == "product is not available"),
            "got: {err:?}"
        );
    }

    // --- ensure_line_quantity ---

    #[test]
    fn ensure_line_quantity_accepts_1_to_999() {
        let product = fixture_product(true, None);
        assert!(product.ensure_line_quantity(1).is_ok());
        assert!(product.ensure_line_quantity(999).is_ok());
    }

    #[test]
    fn ensure_line_quantity_rejects_out_of_range_as_bad_request() {
        let product = fixture_product(true, None);
        for quantity in [0, -1, 1000] {
            let err = product.ensure_line_quantity(quantity).expect_err("must reject");
            assert!(
                matches!(err, AppError::BadRequest(ref m) if *m == quantity_range_msg()),
                "quantity {quantity} got: {err:?}"
            );
        }
    }

    // Moved from `subscriptions::entitlement`'s tests: the quantity rule
    // used to be checked inside the grant itself, after every other
    // checkout check.
    #[test]
    fn ensure_line_quantity_time_based_quantity_other_than_one_is_validation_error() {
        let product = entitlement_product(ProductType::Membership, None, Some(90), true, None);

        let err = product
            .ensure_line_quantity(2)
            .expect_err("quantity=2 for a time-based product must fail");

        match err {
            AppError::Validation(msg) => {
                assert_eq!(msg, "time-based subscription quantity must be 1")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
        assert!(product.ensure_line_quantity(1).is_ok());
    }

    #[test]
    fn ensure_line_quantity_allows_multiples_of_non_time_based_products() {
        let cases = [
            // session-count ticket: sessions multiply by quantity
            entitlement_product(ProductType::Ticket, Some(10), None, true, None),
            // session-count + valid_days: sessions still drive the quota
            entitlement_product(ProductType::Ticket, Some(5), Some(90), true, None),
            // unlimited membership
            entitlement_product(ProductType::Membership, None, None, true, None),
            // valid_days on a non-entitlement type grants nothing
            entitlement_product(ProductType::Merchandise, None, Some(30), true, None),
        ];
        for product in cases {
            assert!(
                product.ensure_line_quantity(2).is_ok(),
                "{:?} session_count={:?} valid_days={:?}",
                product.product_type,
                product.session_count,
                product.valid_days
            );
        }
    }

    #[test]
    fn is_time_based_entitlement_only_for_membership_or_ticket_with_valid_days_only() {
        let yes = |t, s, v| entitlement_product(t, s, v, true, None).is_time_based_entitlement();
        assert!(yes(ProductType::Membership, None, Some(30)));
        assert!(yes(ProductType::Ticket, None, Some(30)));
        assert!(!yes(ProductType::Ticket, Some(10), Some(30)));
        assert!(!yes(ProductType::Membership, None, None));
        assert!(!yes(ProductType::CoursePackage, None, Some(30)));
        assert!(!yes(ProductType::Merchandise, None, Some(30)));
    }

    #[test]
    fn ensure_purchasable_allows_quantity_within_stock() {
        let product = fixture_product(true, Some(5));
        assert!(product.ensure_purchasable(5).is_ok());
    }

    #[test]
    fn ensure_purchasable_rejects_quantity_above_stock() {
        let product = fixture_product(true, Some(3));
        let err = product.ensure_purchasable(4).expect_err("must reject");
        assert!(
            matches!(err, AppError::Conflict(ref m) if m == "insufficient stock: only 3 available"),
            "got: {err:?}"
        );
    }

    #[test]
    fn as_str_matches_the_snake_case_sql_spelling() {
        assert_eq!(ProductType::Ticket.as_str(), "ticket");
        assert_eq!(ProductType::CoursePackage.as_str(), "course_package");
        assert_eq!(ProductType::Membership.as_str(), "membership");
        assert_eq!(ProductType::Merchandise.as_str(), "merchandise");
    }

    #[test]
    fn from_str_roundtrips_every_all_entry() {
        for &v in ProductType::ALL {
            assert_eq!(v.as_str().parse::<ProductType>(), Ok(v));
        }
    }

    #[test]
    fn as_str_and_from_str_round_trip_for_every_variant() {
        for &v in ProductType::ALL {
            let s = v.as_str();
            let parsed: ProductType = s.parse().expect("as_str output must parse");
            assert_eq!(parsed.as_str(), s);
        }
    }

    #[test]
    fn from_str_rejects_unknown_value() {
        assert!("bogus".parse::<ProductType>().is_err());
    }
}
