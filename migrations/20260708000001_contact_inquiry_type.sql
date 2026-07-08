-- =============================================================================
-- Trial-booking specialization for contact_inquiries (Round 4 Task B5).
--
-- Design decision: the mobile "trial class" (試上) booking flow does not
-- consume a course seat or create a real booking — it piggybacks on the
-- existing contact inquiry table as a new `inquiry_type`, with the
-- trial-specific structured fields (category/student_age/preferred_day/
-- preferred_slot/parent_name/parent_phone/student_name/note) assembled by
-- the frontend and stored opaquely in `metadata`. Admin follows up manually
-- via `PATCH /contact/inquiries/{id}`.
--
-- `inquiry_type` intentionally stays a plain VARCHAR with application-layer
-- validation (general/trial) rather than a DB CHECK or enum, matching this
-- migration's brief.
-- =============================================================================

ALTER TABLE contact_inquiries ADD COLUMN inquiry_type VARCHAR(20) NOT NULL DEFAULT 'general';
ALTER TABLE contact_inquiries ADD COLUMN metadata JSONB;
