use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AppError;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "product_type", rename_all = "snake_case")]
pub enum ProductType {
    Ticket,
    CoursePackage,
    Membership,
    Merchandise,
}

impl ProductType {
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
        match s {
            "ticket" => Ok(Self::Ticket),
            "course_package" => Ok(Self::CoursePackage),
            "membership" => Ok(Self::Membership),
            "merchandise" => Ok(Self::Merchandise),
            _ => Err(()),
        }
    }
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

impl Product {
    /// Single-request purchasability predicate: can `quantity` units of this
    /// product be bought *in this one request*? Checks `is_active`, then —
    /// if the product tracks stock at all (`stock: Some(_)`; `None` means
    /// unlimited, e.g. tickets/memberships) — that `quantity` doesn't exceed
    /// it.
    ///
    /// This is deliberately not the owner of any cart-wide stock invariant:
    /// what `quantity` *means* is entirely up to the caller. `cart::service`
    /// has two call sites that pass different things —
    /// `add_product_item` passes the increment being added this call (the
    /// repository accumulates separately via `ON CONFLICT DO UPDATE SET
    /// quantity = cart_items.quantity + $N`), while `update_quantity` passes
    /// the item's final quantity. Because of that, repeated `add_item` calls
    /// can each individually clear this check while the cart's accumulated
    /// total drifts past `stock` — this method has no way to see that, and
    /// is not responsible for closing that gap. The authoritative,
    /// atomic-decrement check lives at checkout in
    /// `products::service::reserve_stock_tx`; this predicate is only ever a
    /// lightweight, single-request pre-check ahead of it.
    ///
    /// Error strings are load-bearing (asserted on by substring match in
    /// `tests/service_cart.rs`): `"product is not available"` /
    /// `BadRequest` (400), `"insufficient stock: only {stock} available"` /
    /// `Conflict` (409).
    pub fn ensure_purchasable(&self, quantity: i32) -> Result<(), AppError> {
        if !self.is_active {
            return Err(AppError::BadRequest("product is not available".into()));
        }

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
        Product {
            id: Uuid::now_v7(),
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
    fn ensure_purchasable_allows_any_quantity_when_stock_is_untracked() {
        let product = fixture_product(true, None);
        assert!(product.ensure_purchasable(1_000_000).is_ok());
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
    fn from_str_parses_every_as_str_output_back_to_its_variant() {
        assert!(matches!("ticket".parse::<ProductType>(), Ok(ProductType::Ticket)));
        assert!(matches!(
            "course_package".parse::<ProductType>(),
            Ok(ProductType::CoursePackage)
        ));
        assert!(matches!(
            "membership".parse::<ProductType>(),
            Ok(ProductType::Membership)
        ));
        assert!(matches!(
            "merchandise".parse::<ProductType>(),
            Ok(ProductType::Merchandise)
        ));
    }

    #[test]
    fn as_str_and_from_str_round_trip_for_every_variant() {
        for v in [
            ProductType::Ticket,
            ProductType::CoursePackage,
            ProductType::Membership,
            ProductType::Merchandise,
        ] {
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
