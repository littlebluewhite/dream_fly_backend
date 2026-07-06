-- =============================================================================
-- Attendance records (Round 3 Task 2) — per-session, per-enrolment attendance
-- status recorded by a coach (of that session's course) or an admin. One row
-- per (session, enrolment); an enrolment with no row yet for a given session
-- is "unmarked" — surfaced as `null` by the attendance module's roster
-- endpoint, not a stored table state.
-- =============================================================================

CREATE TYPE attendance_status AS ENUM ('present', 'absent', 'leave');

CREATE TABLE attendance_records (
    id           UUID              PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id   UUID              NOT NULL REFERENCES course_sessions(id) ON DELETE CASCADE,
    enrolment_id UUID              NOT NULL REFERENCES enrolments(id),
    status       attendance_status NOT NULL,
    marked_by    UUID              NOT NULL REFERENCES users(id),
    marked_at    TIMESTAMPTZ       NOT NULL DEFAULT NOW(),
    created_at   TIMESTAMPTZ       NOT NULL DEFAULT NOW(),
    CONSTRAINT attendance_records_unique UNIQUE (session_id, enrolment_id)
);

-- Supports `GET /enrolments/me`'s per-enrolment attendance aggregate (LEFT
-- JOIN keyed by enrolment_id alone) — the UNIQUE index above is
-- session_id-leading, so it doesn't serve that access pattern.
CREATE INDEX idx_attendance_records_enrolment ON attendance_records(enrolment_id);
