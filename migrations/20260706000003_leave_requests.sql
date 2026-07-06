-- =============================================================================
-- Leave requests (Round 3 Task 3) — a member's request to skip a specific
-- course session (`course_sessions`), decided by that course's coach or an
-- admin. Approving writes `attendance_records.status = 'leave'` for that
-- session (see `leave::service::decide_leave_request`). An approved request
-- may book one makeup session in the same course (`makeup_session_id`).
-- =============================================================================

CREATE TYPE leave_status AS ENUM ('pending', 'approved', 'rejected', 'cancelled');

CREATE TABLE leave_requests (
    id                UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    enrolment_id      UUID         NOT NULL REFERENCES enrolments(id),
    session_id        UUID         NOT NULL REFERENCES course_sessions(id),
    reason            TEXT,
    status            leave_status NOT NULL DEFAULT 'pending',
    makeup_session_id UUID         REFERENCES course_sessions(id),
    decided_by        UUID         REFERENCES users(id),
    decided_at        TIMESTAMPTZ,
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at        TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

-- Only one "live" (pending or approved) leave request per (enrolment,
-- session) at a time — a cancelled/rejected request doesn't block
-- re-applying for the same session.
CREATE UNIQUE INDEX uniq_leave_requests_active ON leave_requests(enrolment_id, session_id)
    WHERE status IN ('pending', 'approved');

CREATE TRIGGER trigger_leave_requests_updated_at BEFORE UPDATE ON leave_requests
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
