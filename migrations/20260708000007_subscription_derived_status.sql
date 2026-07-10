-- =============================================================================
-- C7: subscriptions 到期規則下沉為 SQL function 單一真相。
--
-- Before this migration, "is this subscription active/expired/cancelled"
-- had two independent implementations that had to be kept in sync by hand:
-- `subscriptions::model::derive_status` (Rust, used for the `derived_status`
-- read-time field) and `subscriptions::repository::redeem_one_session`'s
-- `WHERE` clause (SQL, re-encoding the same expiry/session-quota predicate
-- for the atomic redeem path). This function makes the SQL side the single
-- source of truth; the Rust twin is deleted and every subscription query
-- reads `derived_status` from this function instead.
--
-- Returns the `subscription_status` enum (not TEXT) so a typo in a caller's
-- comparison fails at the type level. STABLE (not IMMUTABLE) because it
-- calls NOW(). NULL semantics mirror the Rust `derive_status` it replaces:
-- `expires_at IS NULL` means "no expiry" (never counts as expired by date),
-- `remaining_sessions IS NULL` means "unlimited" (never counts as expired by
-- session quota — `remaining_sessions = 0` is false, not true, when NULL).
-- `status` itself is only ever persisted as `active` or `cancelled` (ADR-0003:
-- `expired` is a read-time-only value, never written back).
-- =============================================================================

CREATE FUNCTION subscription_derived_status(status subscription_status, expires_at TIMESTAMPTZ, remaining_sessions INT)
RETURNS subscription_status LANGUAGE sql STABLE AS $$
  SELECT CASE
    WHEN status = 'cancelled'::subscription_status THEN 'cancelled'::subscription_status
    WHEN (expires_at IS NOT NULL AND expires_at <= NOW()) OR remaining_sessions = 0 THEN 'expired'::subscription_status
    ELSE 'active'::subscription_status
  END $$;
