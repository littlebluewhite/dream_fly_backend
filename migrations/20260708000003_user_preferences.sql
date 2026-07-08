-- =============================================================================
-- Member preference bag (Round 4 Task B7) — backing store for the mobile
-- settings screen's 4 local toggles (classReminder/coachMsg/promo/dark),
-- which currently only live in the app's local store and are lost on
-- re-login.
--
-- Design decision: a single free-form JSONB column on `users`, not a
-- fine-grained per-toggle schema — mirrors the `settings` table's
-- key-value philosophy (Task B6). Written through the existing
-- `PATCH /users/me` endpoint as a whole-object overwrite (no deep merge,
-- no backend key validation); the frontend-convention keys
-- (`class_reminder`/`coach_msg`/`promo`/`dark`) are documented in
-- docs/api/integration-contract.md §3.2 only, not enforced here.
-- =============================================================================

ALTER TABLE users ADD COLUMN preferences JSONB;
