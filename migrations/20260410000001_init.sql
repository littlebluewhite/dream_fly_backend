-- =============================================================================
-- Dream Fly initial schema (consolidated v1 baseline).
--
-- This is the single source of truth for the database schema. It replaces the
-- 15 iteratively-authored migrations from the pre-v1 development phase. Any
-- further schema changes MUST be added as a new migration file; do NOT edit
-- this file after the first commit.
--
-- Layout:
--   1. Extensions
--   2. Shared trigger function (set_updated_at)
--   3. Enum types
--   4. Tables (in FK-dependency order), each followed by its indexes, CHECK
--      constraints, and updated_at trigger
--   5. Seed data (default roles)
--   6. Events outbox (Kafka transactional outbox)
-- =============================================================================


-- ----------------------------------------------------------------------------
-- 1. Extensions
-- ----------------------------------------------------------------------------
-- citext     : case-insensitive TEXT for users.email (so `Foo@X` = `foo@x`
--              under the authoritative UNIQUE constraint, not just in code).
-- btree_gist : enables GIST indexes that mix btree equality columns with
--              range columns — required by the anti-overlap EXCLUDE
--              constraints on time_slots and coach_schedules.
CREATE EXTENSION IF NOT EXISTS citext;
CREATE EXTENSION IF NOT EXISTS btree_gist;


-- ----------------------------------------------------------------------------
-- 2. Shared trigger function
-- ----------------------------------------------------------------------------
-- Used by every table that has an `updated_at` column. Defined once here,
-- referenced by BEFORE UPDATE triggers throughout the file.
CREATE OR REPLACE FUNCTION set_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;


-- ----------------------------------------------------------------------------
-- 3. Enum types
-- ----------------------------------------------------------------------------
CREATE TYPE course_level      AS ENUM ('beginner', 'intermediate', 'advanced');
CREATE TYPE slot_status       AS ENUM ('available', 'limited', 'full', 'closed');
CREATE TYPE booking_status    AS ENUM ('pending', 'confirmed', 'cancelled', 'completed', 'no_show');
CREATE TYPE product_type      AS ENUM ('ticket', 'course_package', 'membership', 'merchandise');
CREATE TYPE order_status      AS ENUM ('pending', 'paid', 'processing', 'completed', 'cancelled', 'refunded');
CREATE TYPE post_category     AS ENUM ('announcement', 'article', 'promotion', 'event');
CREATE TYPE post_status       AS ENUM ('draft', 'published', 'archived');
CREATE TYPE notification_type AS ENUM (
    'booking_confirmed',
    'booking_cancelled',
    'order_placed',
    'order_status',
    'system',
    'promotion'
);
CREATE TYPE inquiry_status    AS ENUM ('new', 'in_progress', 'resolved', 'closed');


-- ============================================================================
-- 4. Tables
-- ============================================================================


-- ----------------------------------------------------------------------------
-- users + refresh_tokens
-- ----------------------------------------------------------------------------
-- email is CITEXT so `Foo@X.com` and `foo@x.com` cannot coexist — the UNIQUE
-- constraint is authoritative without LOWER() tricks in application code.
-- users_has_auth_method guarantees every row has at least one usable login
-- method (password OR Google OAuth); without this a row could be inserted
-- with neither and nobody could ever log into it.
CREATE TABLE users (
    id             UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    email          CITEXT       NOT NULL UNIQUE,
    name           VARCHAR(100) NOT NULL,
    phone          VARCHAR(20),
    phone_verified BOOLEAN      NOT NULL DEFAULT false,
    avatar_url     TEXT,
    password_hash  TEXT,
    google_id      VARCHAR(255) UNIQUE,
    is_active      BOOLEAN      NOT NULL DEFAULT true,
    last_login     TIMESTAMPTZ,
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    CONSTRAINT users_has_auth_method
        CHECK (password_hash IS NOT NULL OR google_id IS NOT NULL)
);

CREATE INDEX idx_users_google_id ON users(google_id);

-- Phone uniqueness is enforced ONLY for verified phones. Unverified rows
-- can still collide, which matters because two users can start registration
-- with the same number before OTP confirmation.
CREATE UNIQUE INDEX uq_users_phone_verified
    ON users(phone)
    WHERE phone_verified = true;

CREATE TRIGGER trigger_users_updated_at
    BEFORE UPDATE ON users
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();


CREATE TABLE refresh_tokens (
    id         UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID         NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash VARCHAR(255) NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ  NOT NULL,
    revoked    BOOLEAN      NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

-- Note: no plain index on `token_hash` — the UNIQUE constraint already
-- backs it with a btree index. Creating a second one was a redundancy
-- inherited from an older draft of this schema.
CREATE INDEX idx_refresh_tokens_user_id ON refresh_tokens(user_id);

-- Partial indexes for the two hot paths on this table:
--   1. expiry cleanup / lookup of still-valid tokens
--   2. `revoke_all_user_tokens(user_id)` which scans active tokens per user
CREATE INDEX idx_refresh_tokens_expires_at
    ON refresh_tokens(expires_at)
    WHERE revoked = false;

CREATE INDEX idx_refresh_tokens_user_active
    ON refresh_tokens(user_id)
    WHERE revoked = false;


-- ----------------------------------------------------------------------------
-- roles / permissions / RBAC junction tables
-- ----------------------------------------------------------------------------
CREATE TABLE roles (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    name        VARCHAR(50) NOT NULL UNIQUE,
    description TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE permissions (
    id         UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    resource   VARCHAR(100) NOT NULL,
    action     VARCHAR(50)  NOT NULL,
    created_at TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    UNIQUE (resource, action)
);

CREATE TABLE role_permissions (
    role_id       UUID NOT NULL REFERENCES roles(id)       ON DELETE CASCADE,
    permission_id UUID NOT NULL REFERENCES permissions(id) ON DELETE CASCADE,
    PRIMARY KEY (role_id, permission_id)
);

CREATE TABLE user_roles (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role_id UUID NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, role_id)
);

CREATE TABLE permission_conditions (
    id            UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    permission_id UUID        NOT NULL REFERENCES permissions(id) ON DELETE CASCADE,
    role_id       UUID        NOT NULL REFERENCES roles(id)       ON DELETE CASCADE,
    condition     JSONB       NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- FK supporting indexes (Postgres does NOT auto-create these for FK columns).
CREATE INDEX idx_permission_conditions_permission_id ON permission_conditions(permission_id);
CREATE INDEX idx_permission_conditions_role_id       ON permission_conditions(role_id);


-- ----------------------------------------------------------------------------
-- coaches / coach_schedules / clock_records
-- ----------------------------------------------------------------------------
CREATE TABLE coaches (
    id             UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id        UUID         NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    title          VARCHAR(100) NOT NULL,
    bio            TEXT,
    experience     TEXT,
    specialties    TEXT[]       NOT NULL DEFAULT '{}',
    certifications TEXT[]       NOT NULL DEFAULT '{}',
    is_active      BOOLEAN      NOT NULL DEFAULT true,
    display_order  INT          NOT NULL DEFAULT 0,
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE TRIGGER trigger_coaches_updated_at
    BEFORE UPDATE ON coaches
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();


CREATE TABLE coach_schedules (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    coach_id     UUID        NOT NULL REFERENCES coaches(id) ON DELETE CASCADE,
    day_of_week  SMALLINT    NOT NULL CHECK (day_of_week BETWEEN 0 AND 6),
    start_time   TIME        NOT NULL,
    end_time     TIME        NOT NULL,
    is_available BOOLEAN     NOT NULL DEFAULT true,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT coach_schedules_time_order CHECK (end_time > start_time),
    -- Prevent overlapping weekly recurring schedules for the same coach
    -- on the same day of week. Anchored to a fixed date so TIME values can
    -- be composed into a tsrange for the GIST overlap operator.
    CONSTRAINT coach_schedules_no_overlap
        EXCLUDE USING gist (
            coach_id    WITH =,
            day_of_week WITH =,
            tsrange(
                ('2000-01-01'::date + start_time)::timestamp,
                ('2000-01-01'::date + end_time)::timestamp
            ) WITH &&
        )
);

CREATE INDEX idx_coach_schedules_coach_id
    ON coach_schedules(coach_id, day_of_week, start_time);


CREATE TABLE clock_records (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    coach_id   UUID        NOT NULL REFERENCES coaches(id) ON DELETE CASCADE,
    clock_in   TIMESTAMPTZ NOT NULL,
    clock_out  TIMESTAMPTZ,
    note       TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Only one open (not-yet-clocked-out) record per coach. Any second clock-in
-- while an open row exists violates this constraint, so double clock-in is
-- impossible at the DB layer.
CREATE UNIQUE INDEX uq_clock_records_open
    ON clock_records(coach_id)
    WHERE clock_out IS NULL;

CREATE INDEX idx_clock_records_coach_clock_in
    ON clock_records(coach_id, clock_in DESC);


-- ----------------------------------------------------------------------------
-- courses
-- ----------------------------------------------------------------------------
CREATE TABLE courses (
    id               UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    name             VARCHAR(100) NOT NULL,
    slug             VARCHAR(100) NOT NULL,
    level            course_level NOT NULL,
    description      TEXT,
    duration_minutes INT          NOT NULL,
    price_cents      BIGINT       NOT NULL,
    max_students     INT          NOT NULL,
    min_age          INT,
    max_age          INT,
    features         TEXT[]       NOT NULL DEFAULT '{}',
    is_active        BOOLEAN      NOT NULL DEFAULT true,
    coach_id         UUID         REFERENCES coaches(id),
    created_at       TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    CONSTRAINT courses_price_nonneg     CHECK (price_cents >= 0),
    CONSTRAINT courses_duration_pos     CHECK (duration_minutes > 0),
    CONSTRAINT courses_max_students_pos CHECK (max_students > 0),
    CONSTRAINT courses_age_range
        CHECK (min_age IS NULL OR max_age IS NULL OR max_age >= min_age)
);

-- Slug is case-insensitively unique: "Beginners" and "beginners" both map
-- to the same public URL, so they must not both exist. We use a functional
-- LOWER() unique index instead of a plain UNIQUE column constraint.
CREATE UNIQUE INDEX uq_courses_slug_lower ON courses (LOWER(slug));

CREATE INDEX idx_courses_coach_id ON courses(coach_id);

CREATE TRIGGER trigger_courses_updated_at
    BEFORE UPDATE ON courses
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();


-- ----------------------------------------------------------------------------
-- venue_categories / venues
-- ----------------------------------------------------------------------------
CREATE TABLE venue_categories (
    id            UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    name          VARCHAR(100) NOT NULL,
    slug          VARCHAR(100) NOT NULL UNIQUE,
    icon          VARCHAR(50),
    display_order INT          NOT NULL DEFAULT 0,
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);


CREATE TABLE venues (
    id          UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    category_id UUID         REFERENCES venue_categories(id),
    name        VARCHAR(100) NOT NULL,
    slug        VARCHAR(100) NOT NULL,
    description TEXT,
    features    TEXT[]       NOT NULL DEFAULT '{}',
    image_url   TEXT,
    is_active   BOOLEAN      NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX uq_venues_slug_lower ON venues (LOWER(slug));
CREATE INDEX        idx_venues_category_id ON venues(category_id);

CREATE TRIGGER trigger_venues_updated_at
    BEFORE UPDATE ON venues
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();


-- ----------------------------------------------------------------------------
-- time_slots
-- ----------------------------------------------------------------------------
CREATE TABLE time_slots (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    date       DATE        NOT NULL,
    start_time TIME        NOT NULL,
    end_time   TIME        NOT NULL,
    venue_id   UUID        REFERENCES venues(id),
    course_id  UUID        REFERENCES courses(id),
    capacity   INT         NOT NULL,
    booked     INT         NOT NULL DEFAULT 0,
    status     slot_status NOT NULL DEFAULT 'available',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT time_slots_time_order       CHECK (end_time > start_time),
    CONSTRAINT time_slots_capacity_nonneg  CHECK (capacity >= 0),
    CONSTRAINT time_slots_booked_bound     CHECK (booked >= 0 AND booked <= capacity),
    -- Authoritative guard against physically double-booking a room:
    -- application-level availability checks can race under concurrency,
    -- this constraint cannot. `WHERE venue_id IS NOT NULL` lets virtual
    -- slots (no venue) bypass the rule.
    CONSTRAINT time_slots_venue_no_overlap
        EXCLUDE USING gist (
            venue_id WITH =,
            date     WITH =,
            tsrange(
                (date + start_time)::timestamp,
                (date + end_time)::timestamp
            ) WITH &&
        )
        WHERE (venue_id IS NOT NULL)
);

CREATE INDEX idx_time_slots_date     ON time_slots(date);
CREATE INDEX idx_time_slots_venue_id ON time_slots(venue_id);
CREATE INDEX idx_time_slots_course_id ON time_slots(course_id);

CREATE TRIGGER trigger_time_slots_updated_at
    BEFORE UPDATE ON time_slots
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();


-- ----------------------------------------------------------------------------
-- bookings
-- ----------------------------------------------------------------------------
CREATE TABLE bookings (
    id           UUID           PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id      UUID           NOT NULL REFERENCES users(id),
    time_slot_id UUID           NOT NULL REFERENCES time_slots(id),
    status       booking_status NOT NULL DEFAULT 'pending',
    note         TEXT,
    created_at   TIMESTAMPTZ    NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ    NOT NULL DEFAULT NOW()
);

-- Prevent the same user from holding two active bookings for the same slot
-- (active = anything except 'cancelled'). A re-book after cancellation is
-- allowed because the cancelled row is excluded from the partial index.
CREATE UNIQUE INDEX uq_bookings_user_slot_active
    ON bookings(user_id, time_slot_id)
    WHERE status <> 'cancelled';

-- Composite index covers the hot path
-- `WHERE user_id = $1 ORDER BY created_at DESC LIMIT N` without an in-memory
-- sort. This replaces a plain `idx_bookings_user_id` that required sorting.
CREATE INDEX idx_bookings_user_created  ON bookings(user_id, created_at DESC);
CREATE INDEX idx_bookings_time_slot_id  ON bookings(time_slot_id);
CREATE INDEX idx_bookings_created_at    ON bookings(created_at DESC);

CREATE TRIGGER trigger_bookings_updated_at
    BEFORE UPDATE ON bookings
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();


-- ----------------------------------------------------------------------------
-- products
-- ----------------------------------------------------------------------------
CREATE TABLE products (
    id                   UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    name                 VARCHAR(200) NOT NULL,
    slug                 VARCHAR(200) NOT NULL,
    product_type         product_type NOT NULL,
    description          TEXT,
    price_cents          BIGINT       NOT NULL,
    original_price_cents BIGINT,
    features             TEXT[]       NOT NULL DEFAULT '{}',
    is_highlighted       BOOLEAN      NOT NULL DEFAULT false,
    badge                VARCHAR(50),
    stock                INT,
    is_active            BOOLEAN      NOT NULL DEFAULT true,
    created_at           TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at           TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    CONSTRAINT products_price_nonneg
        CHECK (price_cents >= 0),
    CONSTRAINT products_original_price_nonneg
        CHECK (original_price_cents IS NULL OR original_price_cents >= 0),
    CONSTRAINT products_stock_nonneg
        CHECK (stock IS NULL OR stock >= 0)
);

CREATE UNIQUE INDEX uq_products_slug_lower ON products (LOWER(slug));

-- Partial index for the primary product-listing query
-- (WHERE is_active = true ORDER BY name).
CREATE INDEX idx_products_active
    ON products(name) WHERE is_active = true;

CREATE TRIGGER trigger_products_updated_at
    BEFORE UPDATE ON products
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();


-- ----------------------------------------------------------------------------
-- cart_items / orders / order_items / order_idempotency
-- ----------------------------------------------------------------------------
CREATE TABLE cart_items (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    product_id UUID        NOT NULL REFERENCES products(id),
    quantity   INT         NOT NULL DEFAULT 1 CHECK (quantity > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (user_id, product_id)
);

CREATE INDEX idx_cart_items_product_id ON cart_items(product_id);

CREATE TRIGGER trigger_cart_items_updated_at
    BEFORE UPDATE ON cart_items
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();


CREATE TABLE orders (
    id             UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id        UUID         NOT NULL REFERENCES users(id),
    order_number   VARCHAR(50)  NOT NULL UNIQUE,
    status         order_status NOT NULL DEFAULT 'pending',
    total_cents    BIGINT       NOT NULL,
    discount_cents BIGINT       NOT NULL DEFAULT 0,
    paid_at        TIMESTAMPTZ,
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    CONSTRAINT orders_total_nonneg    CHECK (total_cents >= 0),
    CONSTRAINT orders_discount_bound  CHECK (discount_cents >= 0 AND discount_cents <= total_cents)
);

CREATE INDEX idx_orders_user_id_created_at
    ON orders(user_id, created_at DESC);

CREATE TRIGGER trigger_orders_updated_at
    BEFORE UPDATE ON orders
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();


CREATE TABLE order_items (
    id               UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id         UUID        NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
    product_id       UUID        NOT NULL REFERENCES products(id),
    quantity         INT         NOT NULL,
    unit_price_cents BIGINT      NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT order_items_quantity_pos     CHECK (quantity > 0),
    CONSTRAINT order_items_unit_price_nonneg CHECK (unit_price_cents >= 0)
);

CREATE INDEX idx_order_items_order_id   ON order_items(order_id);
CREATE INDEX idx_order_items_product_id ON order_items(product_id);


-- Idempotency-key ledger for the checkout flow. A `(user_id, idempotency_key)`
-- pair maps to the order that was created. Retries see the existing row via
-- the primary key and the service layer returns the cached order instead of
-- charging the user twice.
CREATE TABLE order_idempotency (
    user_id         UUID         NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
    idempotency_key VARCHAR(128) NOT NULL,
    order_id        UUID         NOT NULL REFERENCES orders(id),
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, idempotency_key)
);

CREATE INDEX idx_order_idempotency_order_id ON order_idempotency(order_id);


-- ----------------------------------------------------------------------------
-- posts
-- ----------------------------------------------------------------------------
CREATE TABLE posts (
    id           UUID          PRIMARY KEY DEFAULT gen_random_uuid(),
    author_id    UUID          NOT NULL REFERENCES users(id),
    title        VARCHAR(200)  NOT NULL,
    slug         VARCHAR(200)  NOT NULL,
    content      TEXT          NOT NULL,
    excerpt      TEXT,
    category     post_category NOT NULL,
    status       post_status   NOT NULL DEFAULT 'draft',
    cover_image  TEXT,
    published_at TIMESTAMPTZ,
    created_at   TIMESTAMPTZ   NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ   NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX uq_posts_slug_lower ON posts (LOWER(slug));
CREATE INDEX        idx_posts_author_id ON posts(author_id);

-- Public feed hot path: list published posts ordered by published_at DESC.
-- The partial + DESC NULLS LAST matches the default query exactly.
CREATE INDEX idx_posts_published_feed
    ON posts(published_at DESC NULLS LAST)
    WHERE status = 'published';

CREATE TRIGGER trigger_posts_updated_at
    BEFORE UPDATE ON posts
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();


-- ----------------------------------------------------------------------------
-- notifications
-- ----------------------------------------------------------------------------
CREATE TABLE notifications (
    id         UUID              PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID              NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    type       notification_type NOT NULL,
    title      VARCHAR(200)      NOT NULL,
    message    TEXT              NOT NULL,
    is_read    BOOLEAN           NOT NULL DEFAULT false,
    metadata   JSONB,
    created_at TIMESTAMPTZ       NOT NULL DEFAULT NOW()
);

-- Three indexes for the three real access patterns:
--   - unread count / unread list  → partial on is_read = false
--   - paginated full inbox        → composite (user_id, created_at DESC)
--   - generic filter by read flag → composite (user_id, is_read)
CREATE INDEX idx_notifications_user_unread
    ON notifications(user_id, created_at DESC)
    WHERE is_read = false;
CREATE INDEX idx_notifications_user_created
    ON notifications(user_id, created_at DESC);
CREATE INDEX idx_notifications_user_id_is_read
    ON notifications(user_id, is_read);


-- ----------------------------------------------------------------------------
-- contact_inquiries
-- ----------------------------------------------------------------------------
CREATE TABLE contact_inquiries (
    id          UUID           PRIMARY KEY DEFAULT gen_random_uuid(),
    name        VARCHAR(100)   NOT NULL,
    email       VARCHAR(255)   NOT NULL,
    phone       VARCHAR(20),
    subject     VARCHAR(200)   NOT NULL,
    message     TEXT           NOT NULL,
    status      inquiry_status NOT NULL DEFAULT 'new',
    assigned_to UUID           REFERENCES users(id),
    created_at  TIMESTAMPTZ    NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ    NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_contact_inquiries_assigned_to ON contact_inquiries(assigned_to);

-- Composite index for admin filtering of contact inquiries by status
-- with newest-first ordering.
CREATE INDEX idx_contact_inquiries_status_created
    ON contact_inquiries(status, created_at DESC);

CREATE TRIGGER trigger_contact_inquiries_updated_at
    BEFORE UPDATE ON contact_inquiries
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();


-- ----------------------------------------------------------------------------
-- audit_log
-- ----------------------------------------------------------------------------
-- ip_address is native INET (not VARCHAR) from day one so operators can
-- query by network range without casting.
CREATE TABLE audit_log (
    id          UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID         REFERENCES users(id),
    action      VARCHAR(100) NOT NULL,
    resource    VARCHAR(100) NOT NULL,
    resource_id UUID,
    old_value   JSONB,
    new_value   JSONB,
    ip_address  INET,
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_audit_log_user_id               ON audit_log(user_id);
CREATE INDEX idx_audit_log_resource_resource_id  ON audit_log(resource, resource_id);
CREATE INDEX idx_audit_log_user_created_at       ON audit_log(user_id, created_at DESC);
CREATE INDEX idx_audit_log_created_at            ON audit_log(created_at DESC);


-- ============================================================================
-- 5. Seed data
-- ============================================================================
INSERT INTO roles (name, description) VALUES
    ('admin', 'Full system administrator'),
    ('coach', 'Coach with schedule and class management'),
    ('member', 'Registered member'),
    ('guest', 'Guest user with limited access');


-- ============================================================================
-- 6. Events outbox (Kafka transactional outbox)
-- ============================================================================
-- Writers (service layer) INSERT into this table inside the same transaction
-- as the business-data write, so an event is persisted if and only if the
-- triggering business change is persisted. A background dispatcher polls
-- `published_at IS NULL` rows and publishes them to Kafka, marking them
-- published on success.
--
-- This gives at-least-once delivery: a server crash or Kafka outage between
-- the DB commit and the Kafka ack is replayed from the outbox table rather
-- than being silently lost, as the old fire-and-forget `publish_event` did.
CREATE TABLE events_outbox (
    id UUID PRIMARY KEY,
    topic TEXT NOT NULL,
    -- Kafka partition key (usually a user or resource id). Stored verbatim.
    kafka_key TEXT NOT NULL,
    -- Fully-serialized KafkaEvent envelope (see src/kafka/events.rs). Stored
    -- as JSONB rather than TEXT so future dispatchers can inspect fields
    -- (e.g., schema version) without round-tripping through deserialize.
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    published_at TIMESTAMPTZ,
    attempts INT NOT NULL DEFAULT 0,
    last_error TEXT
);

-- Partial index the dispatcher scans every tick. Partial-on-`published_at IS
-- NULL` keeps the index small even as the table grows unboundedly: already-
-- published rows do not appear in the index at all, so polling cost is
-- proportional to the current backlog, not history.
CREATE INDEX idx_events_outbox_pending
    ON events_outbox (created_at)
    WHERE published_at IS NULL;
