-- =============================================================================
-- Relax orders_discount_bound: allow coupons up to the full subtotal.
--
-- The original constraint (20260410000001_init.sql) was
--     CHECK (discount_cents >= 0 AND discount_cents <= total_cents)
-- which compares the discount against the *post-discount* total. With real
-- coupons live (Task 9 checkout rules: couponOff = min(discount_cents,
-- subtotal)), any coupon worth more than half the subtotal — or less, when
-- points redemption also lowers the total — violated the CHECK at INSERT and
-- surfaced as a 500, even though the spec explicitly allows discounts up to
-- 100% of the subtotal.
--
-- The intended upper bound (discount <= subtotal) is no longer expressible
-- over stored columns: subtotal isn't stored, and discount <= total + redeem
-- reduces to 0 <= total_cents + points_used*100, which is always true. The
-- application-level clamp in orders::service::checkout is the authoritative
-- guard; the database keeps only non-negativity.
-- =============================================================================

ALTER TABLE orders DROP CONSTRAINT orders_discount_bound;
ALTER TABLE orders ADD CONSTRAINT orders_discount_nonneg CHECK (discount_cents >= 0);
