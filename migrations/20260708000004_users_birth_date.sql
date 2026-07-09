-- =============================================================================
-- Round 4 Phase 4 (報表資料源) — reporting-groundwork field #1.
--
-- `birth_date` backs a future age-bracket breakdown report. This migration
-- only adds the column; the write path (registration/profile DTO, PATCH
-- validation) is Task P4-B2's scope, not this one.
-- =============================================================================

ALTER TABLE users ADD COLUMN birth_date DATE;
