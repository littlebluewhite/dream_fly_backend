-- =============================================================================
-- Extend course_level from 3 tiers to 5: add 'foundation' below 'beginner'
-- and 'elite' above 'advanced'.
--
-- Each statement only adds a label to the enum type — neither one "uses" the
-- new value (no cast/comparison against it) — so both can safely share this
-- migration's transaction. PostgreSQL only forbids using a freshly-added
-- enum value within the same transaction that added it (see the existing
-- 20260707000004 migration's note on this exact restriction).
-- =============================================================================

ALTER TYPE course_level ADD VALUE IF NOT EXISTS 'foundation' BEFORE 'beginner';
ALTER TYPE course_level ADD VALUE IF NOT EXISTS 'elite' AFTER 'advanced';
