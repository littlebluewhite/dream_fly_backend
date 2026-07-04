-- =============================================================================
-- order_items name snapshot.
--
-- OrderSummary/AdminOrderSummary need a per-line "name" for their `items`
-- brief (see orders::repository::find_by_user / find_all_with_user) that
-- reflects what the buyer purchased at checkout time, not whatever the
-- product/course is currently called. `order_items` had no such column —
-- this adds it and backfills existing rows from the current catalog (the
-- best available approximation for historical rows; going forward,
-- orders::repository::create_order_items always supplies the real
-- checkout-time name from the cart snapshot).
-- =============================================================================

ALTER TABLE order_items ADD COLUMN name VARCHAR(200);

-- Backfill: order_items.product_id/course_id have no ON DELETE, so the
-- referenced product/course row is guaranteed to still exist.
UPDATE order_items oi SET name = p.name
    FROM products p WHERE oi.product_id = p.id AND oi.name IS NULL;
UPDATE order_items oi SET name = c.name
    FROM courses c WHERE oi.course_id = c.id AND oi.name IS NULL;

ALTER TABLE order_items ALTER COLUMN name SET NOT NULL;
