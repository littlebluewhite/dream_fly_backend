//! Entitlement Grant — the four-branch rule `grant_from_purchase_tx` used to
//! compute inline, pulled out into a pure function (zero IO, owned output,
//! same discipline as `orders::fulfilment`/`orders::pricing`). `plan()`
//! decides *what* a purchase of `quantity` units of `product` entitles the
//! buyer to; `service::grant_from_purchase_tx` is left with just calling
//! this and then the one `repository::insert_tx` write.
//!
//! These are the ADR-0003 Decision-section grant rules, moved here verbatim:
//! - `product.product_type` not in {membership, ticket} → `Ok(None)`.
//! - `session_count` set → one row, `total_sessions = remaining_sessions =
//!   session_count * quantity`. If `valid_days` is *also* set, `expires_at`
//!   is populated too (both constraints apply — sessions still drive the
//!   quota).
//! - else `valid_days` set → `expires_at = now + valid_days`, no session
//!   quota; `quantity` must be 1 (a time-based grant can't be multiplied
//!   into one row), otherwise `AppError::Validation`.
//! - neither set → unlimited membership record (no expiry, no quota).
//!
//! `now` is the caller's sampled clock (checkout's own `now`) rather than a
//! `Utc::now()` read in here — the same reason `orders::pricing`/
//! `orders::fulfilment` never call out to `Utc::now()`, the DB, or anything
//! else non-deterministic: it lets every branch's `expires_at` be asserted
//! against an exact `now + Duration::days(n)` value in `#[test]` below, no
//! DB round-trip and no ±1 day window. This module never touches
//! `subscription_derived_status` — that SQL function alone owns read-time
//! expiry/status derivation (ADR-0003 Addendum); `plan()` only computes what
//! to grant at write time.

use chrono::{DateTime, Duration, Utc};

use crate::error::AppError;
use crate::modules::products::model::{Product, ProductType};

/// What a purchase entitles the buyer to: session quota, expiry, or both —
/// exactly the fields `repository::insert_tx` writes besides the
/// unconditional `user_id`/`product_id`/`order_id`/`price_cents`.
#[derive(Debug)]
pub struct EntitlementGrant {
    pub total_sessions: Option<i32>,
    pub remaining_sessions: Option<i32>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Compute the entitlement grant for `quantity` units of `product`, sampled
/// at `now`. `Ok(None)` means `product` isn't entitlement-eligible at all
/// (not `membership`/`ticket`) — the caller writes no row. See the module
/// doc for the four branches.
pub fn plan(
    product: &Product,
    quantity: i32,
    now: DateTime<Utc>,
) -> Result<Option<EntitlementGrant>, AppError> {
    if !matches!(
        product.product_type,
        ProductType::Membership | ProductType::Ticket
    ) {
        return Ok(None);
    }

    let (total_sessions, remaining_sessions, expires_at) =
        if let Some(session_count) = product.session_count {
            let total = session_count * quantity;
            let expires_at = product
                .valid_days
                .map(|days| now + Duration::days(days as i64));
            (Some(total), Some(total), expires_at)
        } else if let Some(valid_days) = product.valid_days {
            if quantity != 1 {
                return Err(AppError::Validation(
                    "time-based subscription quantity must be 1".into(),
                ));
            }
            (None, None, Some(now + Duration::days(valid_days as i64)))
        } else {
            (None, None, None)
        };

    Ok(Some(EntitlementGrant {
        total_sessions,
        remaining_sessions,
        expires_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// Local fixture mirroring `products::model`'s `fixture_product` shape
    /// — only the fields these rules branch on vary; everything else is
    /// filler.
    fn fixture_product(
        product_type: ProductType,
        session_count: Option<i32>,
        valid_days: Option<i32>,
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
            stock: None,
            valid_days,
            session_count,
            is_active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // Mirrors `tests/service_subscriptions.rs`'s `grant_from_purchase_tx`
    // cases (two of the six stay there as integration tests, guarding the
    // DB wiring these pure cases can't reach) — same six scenarios, direct
    // calls instead of a seeded product + DB transaction.

    #[test]
    fn session_count_multiplies_by_quantity() {
        let product = fixture_product(ProductType::Ticket, Some(10), None);

        let grant = plan(&product, 3, Utc::now())
            .expect("grant")
            .expect("expected Some(EntitlementGrant)");

        assert_eq!(grant.total_sessions, Some(30));
        assert_eq!(grant.remaining_sessions, Some(30));
        assert!(grant.expires_at.is_none());
    }

    #[test]
    fn session_count_with_valid_days_also_sets_expiry() {
        let product = fixture_product(ProductType::Ticket, Some(5), Some(90));
        let now = Utc::now();

        let grant = plan(&product, 2, now)
            .expect("grant")
            .expect("expected Some(EntitlementGrant)");

        // Both constraints apply: sessions still drive the quota...
        assert_eq!(grant.total_sessions, Some(10));
        assert_eq!(grant.remaining_sessions, Some(10));
        // ...and expires_at is populated too, since valid_days was also
        // set — exact value, no DB round-trip so no ±1 day window needed.
        assert_eq!(grant.expires_at, Some(now + Duration::days(90)));
    }

    #[test]
    fn valid_days_only_sets_expiry_and_no_sessions() {
        let product = fixture_product(ProductType::Membership, None, Some(30));
        let now = Utc::now();

        let grant = plan(&product, 1, now)
            .expect("grant")
            .expect("expected Some(EntitlementGrant)");

        assert!(grant.total_sessions.is_none());
        assert!(grant.remaining_sessions.is_none());
        assert_eq!(grant.expires_at, Some(now + Duration::days(30)));
    }

    #[test]
    fn no_entitlement_fields_creates_unlimited_membership() {
        let product = fixture_product(ProductType::Membership, None, None);

        let grant = plan(&product, 1, Utc::now())
            .expect("grant")
            .expect("expected Some(EntitlementGrant)");

        assert!(grant.total_sessions.is_none());
        assert!(grant.remaining_sessions.is_none());
        assert!(grant.expires_at.is_none());
    }

    #[test]
    fn non_entitlement_product_type_returns_none() {
        // product_type is the only field this early-return branch inspects
        // — session_count/valid_days are irrelevant to it (left None).
        let product = fixture_product(ProductType::Merchandise, None, None);

        let grant = plan(&product, 1, Utc::now())
            .expect("grant should not error for a non-entitlement product");

        assert!(grant.is_none());
    }

    #[test]
    fn time_based_with_quantity_other_than_one_is_validation_error() {
        let product = fixture_product(ProductType::Membership, None, Some(90));

        let err =
            plan(&product, 2, Utc::now()).expect_err("quantity=2 for a time-based product must fail");

        match err {
            AppError::Validation(msg) => {
                assert_eq!(msg, "time-based subscription quantity must be 1")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }
}
