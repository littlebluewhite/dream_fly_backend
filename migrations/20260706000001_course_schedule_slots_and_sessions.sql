-- =============================================================================
-- Course schedule slots + course sessions (Round 3 Task 1).
--
-- `course_schedule_slots` is the structured weekly pattern for a course —
-- mirrors `coach_schedules` (day_of_week smallint 0-6 + start_time/end_time
-- time), replacing the free-text `courses.schedule_text` as the machine-
-- readable source of truth for a course's weekly meeting times.
--
-- `course_sessions` is the materialized calendar-date occurrence of a slot
-- (one row per actual date the course meets). Rows are generated on demand
-- from `course_schedule_slots` by the `sessions` module's `materialize_range`
-- (idempotent — `ON CONFLICT DO NOTHING` on the same unique key used below).
-- There is no `status` column: v1 has no course-suspension feature, and
-- "live"/"done" are derived from wall-clock time by the caller, not stored.
--
-- day_of_week convention: 0=Sunday .. 6=Saturday, matching PostgreSQL's
-- native `EXTRACT(DOW FROM date)` (used by `materialize_range`'s date-to-slot
-- matching) and JavaScript's `Date.getDay()`. See docs/api/integration-
-- contract.md §3.18 for the frontend-facing statement of this convention.
-- =============================================================================

CREATE TABLE course_schedule_slots (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    course_id   UUID        NOT NULL REFERENCES courses(id) ON DELETE CASCADE,
    day_of_week SMALLINT    NOT NULL CHECK (day_of_week BETWEEN 0 AND 6),
    start_time  TIME        NOT NULL,
    end_time    TIME        NOT NULL,
    venue       TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT course_schedule_slots_time_order CHECK (end_time > start_time),
    CONSTRAINT course_schedule_slots_unique UNIQUE (course_id, day_of_week, start_time)
);

CREATE TABLE course_sessions (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    course_id    UUID        NOT NULL REFERENCES courses(id),
    session_date DATE        NOT NULL,
    start_time   TIME        NOT NULL,
    end_time     TIME        NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT course_sessions_time_order CHECK (end_time > start_time),
    CONSTRAINT course_sessions_unique UNIQUE (course_id, session_date, start_time)
);

-- Supports `GET /sessions/today`, which filters by `session_date` alone
-- across many/all courses (the UNIQUE index above is course_id-leading, so
-- it doesn't serve that access pattern).
CREATE INDEX idx_course_sessions_date ON course_sessions(session_date);
