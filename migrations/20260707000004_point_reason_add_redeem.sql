-- =============================================================================
-- Add the 'redeem' point_reason label used by rewards redemption
-- (src/modules/rewards::service::redeem writing to point_ledger).
--
-- Must be its own standalone migration: PostgreSQL forbids using a
-- freshly-added enum value inside the same transaction that added it, and
-- sqlx runs each migration file in its own transaction — so this statement
-- must not share a migration with anything that references 'redeem'.
-- =============================================================================

ALTER TYPE point_reason ADD VALUE IF NOT EXISTS 'redeem';
