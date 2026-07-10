-- =============================================================================
-- Round 4 Phase 4 (報表資料源) — reporting-groundwork field #2.
--
-- `payment_method` records how an order was paid (credit_card/line_pay/atm/
-- jkopay/cash — see `orders::model::PAYMENT_METHODS`), for the future
-- payment-method breakdown report. Plain VARCHAR, not a DB enum, matching
-- this migration's brief. Task P4-B1 wires the full write path (checkout
-- default + validation) on top of this column.
--
-- The partial index on `paid_at` speeds up `reports::repository::
-- recent_activity`'s `WHERE paid_at IS NOT NULL ... ORDER BY paid_at DESC
-- LIMIT 20` (index-order scan instead of a full sort). It does NOT help
-- the reports module's `date_trunc('month', paid_at AT TIME ZONE ...)`
-- queries — wrapping the column in an expression makes those non-sargable
-- against this plain btree index. `orders.paid_at` already exists since
-- the initial schema (20260410000001), no fallback column needed.
-- =============================================================================

ALTER TABLE orders ADD COLUMN payment_method VARCHAR(30);

CREATE INDEX idx_orders_paid_at ON orders (paid_at) WHERE paid_at IS NOT NULL;
