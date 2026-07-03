-- =============================================================================
-- Commerce extension: course cart lines, coupons, subscriptions, enrolments,
-- waitlist, points ledger, and presentation columns on existing tables.
--
-- This is additive on top of 20260410000001_init.sql. Nothing in the
-- application consumes the new tables yet (that lands in later migrations'
-- companion tasks) — this migration only establishes the schema and exposes
-- new columns on the existing products/coaches/courses modules.
-- =============================================================================


-- ----------------------------------------------------------------------------
-- New enum types
-- ----------------------------------------------------------------------------
CREATE TYPE cart_item_type      AS ENUM ('product', 'course');
CREATE TYPE subscription_status AS ENUM ('active', 'expired', 'cancelled');
CREATE TYPE enrolment_status    AS ENUM ('active', 'cancelled');
CREATE TYPE waitlist_status     AS ENUM ('waiting', 'cancelled');
CREATE TYPE point_reason        AS ENUM ('checkout_earn', 'checkout_redeem', 'admin_adjust');


-- ----------------------------------------------------------------------------
-- cart_items: support course line items alongside product line items
-- ----------------------------------------------------------------------------
-- Each cart row now targets exactly one of product_id/course_id, discriminated
-- by item_type. The old single-target UNIQUE(user_id, product_id) constraint
-- is replaced by two partial unique indexes, one per target column, so a user
-- cannot add the same product (or the same course) to their cart twice.
ALTER TABLE cart_items
    ADD COLUMN item_type cart_item_type NOT NULL DEFAULT 'product',
    ADD COLUMN course_id UUID REFERENCES courses(id),
    ALTER COLUMN product_id DROP NOT NULL;
ALTER TABLE cart_items DROP CONSTRAINT cart_items_user_id_product_id_key;
ALTER TABLE cart_items ADD CONSTRAINT cart_items_one_target CHECK (
    (item_type = 'product' AND product_id IS NOT NULL AND course_id IS NULL) OR
    (item_type = 'course'  AND course_id  IS NOT NULL AND product_id IS NULL));
ALTER TABLE cart_items ADD CONSTRAINT cart_items_course_qty CHECK (item_type <> 'course' OR quantity = 1);
CREATE UNIQUE INDEX uniq_cart_items_product ON cart_items(user_id, product_id) WHERE product_id IS NOT NULL;
CREATE UNIQUE INDEX uniq_cart_items_course  ON cart_items(user_id, course_id)  WHERE course_id  IS NOT NULL;


-- ----------------------------------------------------------------------------
-- order_items: mirror the same product/course dual-target shape
-- ----------------------------------------------------------------------------
ALTER TABLE order_items
    ADD COLUMN item_type cart_item_type NOT NULL DEFAULT 'product',
    ADD COLUMN course_id UUID REFERENCES courses(id),
    ALTER COLUMN product_id DROP NOT NULL;
ALTER TABLE order_items ADD CONSTRAINT order_items_one_target CHECK (
    (item_type = 'product' AND product_id IS NOT NULL AND course_id IS NULL) OR
    (item_type = 'course'  AND course_id  IS NOT NULL AND product_id IS NULL));


-- ----------------------------------------------------------------------------
-- orders: coupon + points columns for checkout
-- ----------------------------------------------------------------------------
ALTER TABLE orders
    ADD COLUMN coupon_code   VARCHAR(50),
    ADD COLUMN points_used   BIGINT NOT NULL DEFAULT 0 CHECK (points_used >= 0),
    ADD COLUMN points_earned BIGINT NOT NULL DEFAULT 0 CHECK (points_earned >= 0);


-- ----------------------------------------------------------------------------
-- coupons
-- ----------------------------------------------------------------------------
CREATE TABLE coupons (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code           VARCHAR(50) NOT NULL UNIQUE,
    discount_cents BIGINT NOT NULL CHECK (discount_cents > 0),
    is_active      BOOLEAN NOT NULL DEFAULT TRUE,
    expires_at     TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW());


-- ----------------------------------------------------------------------------
-- users: points balance + point_ledger history
-- ----------------------------------------------------------------------------
ALTER TABLE users ADD COLUMN points_balance BIGINT NOT NULL DEFAULT 0 CHECK (points_balance >= 0);

CREATE TABLE point_ledger (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id       UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    delta         BIGINT NOT NULL,
    balance_after BIGINT NOT NULL,
    reason        point_reason NOT NULL,
    order_id      UUID REFERENCES orders(id),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW());
CREATE INDEX idx_point_ledger_user ON point_ledger(user_id, created_at DESC);


-- ----------------------------------------------------------------------------
-- products: subscription/package presentation columns + subscriptions table
-- ----------------------------------------------------------------------------
ALTER TABLE products
    ADD COLUMN valid_days    INT CHECK (valid_days > 0),
    ADD COLUMN session_count INT CHECK (session_count > 0);

CREATE TABLE subscriptions (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id            UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    product_id         UUID NOT NULL REFERENCES products(id),
    order_id           UUID REFERENCES orders(id),
    status             subscription_status NOT NULL DEFAULT 'active',
    started_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at         TIMESTAMPTZ,
    total_sessions     INT CHECK (total_sessions > 0),
    remaining_sessions INT CHECK (remaining_sessions >= 0),
    price_cents        BIGINT NOT NULL,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT NOW());
CREATE INDEX idx_subscriptions_user ON subscriptions(user_id, created_at DESC);
CREATE TRIGGER trigger_subscriptions_updated_at BEFORE UPDATE ON subscriptions
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();


-- ----------------------------------------------------------------------------
-- enrolments
-- ----------------------------------------------------------------------------
CREATE TABLE enrolments (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    course_id   UUID NOT NULL REFERENCES courses(id),
    order_id    UUID REFERENCES orders(id),
    status      enrolment_status NOT NULL DEFAULT 'active',
    enrolled_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW());
CREATE UNIQUE INDEX uniq_enrolments_active ON enrolments(user_id, course_id) WHERE status = 'active';
CREATE INDEX idx_enrolments_course_active ON enrolments(course_id) WHERE status = 'active';
CREATE TRIGGER trigger_enrolments_updated_at BEFORE UPDATE ON enrolments
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();


-- ----------------------------------------------------------------------------
-- waitlist_entries
-- ----------------------------------------------------------------------------
CREATE TABLE waitlist_entries (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    course_id  UUID NOT NULL REFERENCES courses(id),
    status     waitlist_status NOT NULL DEFAULT 'waiting',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW());
CREATE UNIQUE INDEX uniq_waitlist_waiting ON waitlist_entries(user_id, course_id) WHERE status = 'waiting';
CREATE TRIGGER trigger_waitlist_updated_at BEFORE UPDATE ON waitlist_entries
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();


-- ----------------------------------------------------------------------------
-- coaches / courses: presentation columns
-- ----------------------------------------------------------------------------
ALTER TABLE coaches ADD COLUMN slug VARCHAR(100) UNIQUE, ADD COLUMN photo_url TEXT;
ALTER TABLE courses ADD COLUMN category VARCHAR(50),
    ADD COLUMN schedule_text VARCHAR(100),
    ADD COLUMN is_highlighted BOOLEAN NOT NULL DEFAULT FALSE;
