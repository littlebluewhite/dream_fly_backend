-- =============================================================================
-- Round 4 Phase 4 (報表資料源) — reporting-groundwork field #3.
--
-- `price_cents` on `time_slots` (the bookable venue-rental slot) and
-- `bookings` (a user's booking of one) backs a future venue-rental revenue
-- report. Table names confirmed against the initial schema
-- (20260410000001_init.sql) — the venue-rental "slot"/"booking" tables are
-- literally named `time_slots`/`bookings`, matching the brief verbatim.
--
-- This migration only adds the columns (NOT NULL DEFAULT 0 so existing rows
-- backfill to zero rather than needing a nullable + backfill step); the
-- write path (schedule/bookings DTOs, admin pricing input) is Task P4-B2's
-- scope, not this one.
-- =============================================================================

ALTER TABLE time_slots ADD COLUMN price_cents BIGINT NOT NULL DEFAULT 0 CHECK (price_cents >= 0);
ALTER TABLE bookings ADD COLUMN price_cents BIGINT NOT NULL DEFAULT 0 CHECK (price_cents >= 0);
