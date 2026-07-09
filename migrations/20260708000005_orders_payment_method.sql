-- =============================================================================
-- Round 4 Phase 4 (報表資料源) — reporting-groundwork field #2.
--
-- `payment_method` records how an order was paid (credit_card/line_pay/atm/
-- jkopay/cash — see `orders::model::PAYMENT_METHODS`), for the future
-- payment-method breakdown report. Plain VARCHAR, not a DB enum, matching
-- this migration's brief. Task P4-B1 wires the full write path (checkout
-- default + validation) on top of this column.
--
-- The partial index on `paid_at` speeds up the reports module's existing
-- `WHERE paid_at IS NOT NULL` / `date_trunc('month', paid_at ...)` queries
-- (see `reports::repository`) — `orders.paid_at` already exists since the
-- initial schema (20260410000001), no fallback column needed.
-- =============================================================================

ALTER TABLE orders ADD COLUMN payment_method VARCHAR(30);

CREATE INDEX idx_orders_paid_at ON orders (paid_at) WHERE paid_at IS NOT NULL;
