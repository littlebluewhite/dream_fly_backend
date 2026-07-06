use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::model::{
    AdminLeaveRequestRow, LeaveDecisionContext, LeaveRequest, LeaveRequestForMakeup,
    LeaveRequestOwnerRow, LeaveStatus, MakeupCapacity, MyLeaveRequestRow, SessionContext,
};

/// `course_sessions` JOINed with its course's `name`/`coach_id`/
/// `max_students` — used both by `POST /leave-requests` (plain read) and the
/// makeup endpoint's target-session validation (`_tx` variant below, reading
/// through the same open transaction as the leave-request row lock).
pub async fn find_session_context(
    db: &PgPool,
    session_id: Uuid,
) -> Result<Option<SessionContext>, sqlx::Error> {
    sqlx::query_as::<_, SessionContext>(
        "SELECT cs.course_id, c.name AS course_name, cs.session_date, cs.start_time, \
                c.coach_id, c.max_students \
         FROM course_sessions cs \
         JOIN courses c ON c.id = cs.course_id \
         WHERE cs.id = $1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await
}

/// Transactional counterpart of [`find_session_context`] — see its doc comment.
pub async fn find_session_context_tx(
    tx: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
) -> Result<Option<SessionContext>, sqlx::Error> {
    sqlx::query_as::<_, SessionContext>(
        "SELECT cs.course_id, c.name AS course_name, cs.session_date, cs.start_time, \
                c.coach_id, c.max_students \
         FROM course_sessions cs \
         JOIN courses c ON c.id = cs.course_id \
         WHERE cs.id = $1",
    )
    .bind(session_id)
    .fetch_optional(&mut **tx)
    .await
}

/// The caller's active enrolment id for a course, if any — used by
/// `POST /leave-requests` to resolve `session_id` → "my enrolment" without
/// a capacity lock (creating a leave request doesn't touch course capacity).
/// Queries `enrolments` directly rather than going through
/// `enrolments::repository` (no plain, non-transactional "by user+course"
/// lookup exists there) — mirrors `sessions::repository::find_all_course_ids`'s
/// convention of reading a sibling module's table directly for a one-off need.
pub async fn find_active_enrolment(
    db: &PgPool,
    user_id: Uuid,
    course_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM enrolments WHERE user_id = $1 AND course_id = $2 AND status = 'active'",
    )
    .bind(user_id)
    .bind(course_id)
    .fetch_optional(db)
    .await
}

/// Insert a new `pending` leave request. Duplicate (enrolment_id, session_id)
/// while an existing row is `pending`/`approved` trips the partial unique
/// index `uniq_leave_requests_active` — `service` catches that as a 23505
/// and maps it to a friendly 409.
pub async fn insert(
    db: &PgPool,
    enrolment_id: Uuid,
    session_id: Uuid,
    reason: Option<&str>,
) -> Result<LeaveRequest, sqlx::Error> {
    sqlx::query_as::<_, LeaveRequest>(
        "INSERT INTO leave_requests \
         (id, enrolment_id, session_id, reason, status, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'pending'::leave_status, NOW(), NOW()) \
         RETURNING id, enrolment_id, session_id, reason, status, makeup_session_id, \
                   decided_by, decided_at, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(enrolment_id)
    .bind(session_id)
    .bind(reason)
    .fetch_one(db)
    .await
}

/// This user's leave requests JOINed with their course and both the
/// original and (if booked) makeup session's date/time — one query, no N+1.
pub async fn find_my_leave_requests(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<MyLeaveRequestRow>, sqlx::Error> {
    sqlx::query_as::<_, MyLeaveRequestRow>(
        "SELECT lr.id, e.course_id, c.name AS course_name, lr.session_id, \
                cs.session_date, cs.start_time, lr.reason, lr.status, lr.makeup_session_id, \
                mcs.session_date AS makeup_session_date, mcs.start_time AS makeup_start_time, \
                lr.decided_at, lr.created_at \
         FROM leave_requests lr \
         JOIN enrolments e ON e.id = lr.enrolment_id \
         JOIN courses c ON c.id = e.course_id \
         JOIN course_sessions cs ON cs.id = lr.session_id \
         LEFT JOIN course_sessions mcs ON mcs.id = lr.makeup_session_id \
         WHERE e.user_id = $1 \
         ORDER BY lr.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// Ownership context for `DELETE /leave-requests/{id}`, locked so the
/// subsequent conditional cancel can't race a concurrent decide/cancel.
pub async fn find_owner_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<LeaveRequestOwnerRow>, sqlx::Error> {
    sqlx::query_as::<_, LeaveRequestOwnerRow>(
        "SELECT lr.id, e.user_id, lr.status \
         FROM leave_requests lr \
         JOIN enrolments e ON e.id = lr.enrolment_id \
         WHERE lr.id = $1 \
         FOR UPDATE OF lr",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
}

/// Conditional cancel — only succeeds while still `pending`. Returns `None`
/// if the row was already decided/cancelled (caller maps that to 409).
pub async fn cancel_if_pending_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<LeaveRequest>, sqlx::Error> {
    sqlx::query_as::<_, LeaveRequest>(
        "UPDATE leave_requests SET status = 'cancelled'::leave_status, updated_at = NOW() \
         WHERE id = $1 AND status = 'pending'::leave_status \
         RETURNING id, enrolment_id, session_id, reason, status, makeup_session_id, \
                   decided_by, decided_at, created_at, updated_at",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
}

/// Coach/admin list — optional `status`/`course_id` filters, plus an
/// optional `coach_scope` (the caller's own `coaches.id`; `None` = no
/// restriction, i.e. admin sees every course's leave requests).
pub async fn find_admin_list(
    db: &PgPool,
    status_filter: Option<&str>,
    course_id_filter: Option<Uuid>,
    coach_scope: Option<Uuid>,
    limit: u32,
    offset: u32,
) -> Result<Vec<AdminLeaveRequestRow>, sqlx::Error> {
    sqlx::query_as::<_, AdminLeaveRequestRow>(
        "SELECT lr.id, e.course_id, c.name AS course_name, u.id AS user_id, u.name AS user_name, \
                lr.session_id, cs.session_date, cs.start_time, lr.reason, lr.status, \
                lr.makeup_session_id, mcs.session_date AS makeup_session_date, \
                mcs.start_time AS makeup_start_time, lr.decided_at, lr.created_at \
         FROM leave_requests lr \
         JOIN enrolments e ON e.id = lr.enrolment_id \
         JOIN courses c ON c.id = e.course_id \
         JOIN users u ON u.id = e.user_id \
         JOIN course_sessions cs ON cs.id = lr.session_id \
         LEFT JOIN course_sessions mcs ON mcs.id = lr.makeup_session_id \
         WHERE ($1::text IS NULL OR lr.status = $1::leave_status) \
           AND ($2::uuid IS NULL OR c.id = $2) \
           AND ($3::uuid IS NULL OR c.coach_id = $3) \
         ORDER BY lr.created_at DESC \
         LIMIT $4 OFFSET $5",
    )
    .bind(status_filter)
    .bind(course_id_filter)
    .bind(coach_scope)
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(db)
    .await
}

/// Count counterpart of [`find_admin_list`] — same filters, no LIMIT/OFFSET.
pub async fn count_admin_list(
    db: &PgPool,
    status_filter: Option<&str>,
    course_id_filter: Option<Uuid>,
    coach_scope: Option<Uuid>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) \
         FROM leave_requests lr \
         JOIN enrolments e ON e.id = lr.enrolment_id \
         JOIN courses c ON c.id = e.course_id \
         WHERE ($1::text IS NULL OR lr.status = $1::leave_status) \
           AND ($2::uuid IS NULL OR c.id = $2) \
           AND ($3::uuid IS NULL OR c.coach_id = $3)",
    )
    .bind(status_filter)
    .bind(course_id_filter)
    .bind(coach_scope)
    .fetch_one(db)
    .await
}

/// Everything `PATCH /leave-requests/{id}` needs in one read: current
/// status, the (enrolment_id, session_id) pair for the attendance upsert,
/// the owning student's `user_id` (notification target), and the course's
/// `coach_id`/`name` (authorization + notification copy).
pub async fn find_decision_context(
    db: &PgPool,
    id: Uuid,
) -> Result<Option<LeaveDecisionContext>, sqlx::Error> {
    sqlx::query_as::<_, LeaveDecisionContext>(
        "SELECT lr.status, lr.enrolment_id, lr.session_id, e.user_id, e.course_id, \
                c.name AS course_name, c.coach_id, cs.session_date, cs.start_time \
         FROM leave_requests lr \
         JOIN enrolments e ON e.id = lr.enrolment_id \
         JOIN courses c ON c.id = e.course_id \
         JOIN course_sessions cs ON cs.id = lr.session_id \
         WHERE lr.id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Conditional approve/reject — only succeeds while still `pending`. Returns
/// `None` if the row was raced to a different status (caller maps that to
/// 409); mirrors [`cancel_if_pending_tx`]'s guard shape.
pub async fn decide_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    new_status: LeaveStatus,
    decided_by: Uuid,
) -> Result<Option<LeaveRequest>, sqlx::Error> {
    sqlx::query_as::<_, LeaveRequest>(
        "UPDATE leave_requests \
         SET status = $2::leave_status, decided_by = $3, decided_at = NOW(), updated_at = NOW() \
         WHERE id = $1 AND status = 'pending'::leave_status \
         RETURNING id, enrolment_id, session_id, reason, status, makeup_session_id, \
                   decided_by, decided_at, created_at, updated_at",
    )
    .bind(id)
    .bind(new_status.as_str())
    .bind(decided_by)
    .fetch_optional(&mut **tx)
    .await
}

/// Lock the leave request row (`FOR UPDATE OF lr`) for the makeup endpoint —
/// this is the guard that makes two concurrent `POST .../makeup` calls for
/// the *same* leave request serialize, so only one can ever see
/// `makeup_session_id IS NULL` and win. JOINed with `enrolments`/`courses`
/// for the owner check and the original session's own display fields, but
/// only the `leave_requests` row itself is locked.
pub async fn find_for_makeup_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<LeaveRequestForMakeup>, sqlx::Error> {
    sqlx::query_as::<_, LeaveRequestForMakeup>(
        "SELECT lr.id, lr.session_id, e.user_id, e.course_id, c.name AS course_name, \
                lr.status, lr.makeup_session_id, cs.session_date, cs.start_time, lr.reason \
         FROM leave_requests lr \
         JOIN enrolments e ON e.id = lr.enrolment_id \
         JOIN courses c ON c.id = e.course_id \
         JOIN course_sessions cs ON cs.id = lr.session_id \
         WHERE lr.id = $1 \
         FOR UPDATE OF lr",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
}

/// Capacity inputs for the makeup formula (see `service::book_makeup`) — one
/// query combining the course's `max_students` with three correlated counts,
/// mirroring `courses::repository`'s correlated-subquery style.
pub async fn find_makeup_capacity_tx(
    tx: &mut Transaction<'_, Postgres>,
    course_id: Uuid,
    target_session_id: Uuid,
) -> Result<Option<MakeupCapacity>, sqlx::Error> {
    sqlx::query_as::<_, MakeupCapacity>(
        "SELECT c.max_students, \
                (SELECT COUNT(*) FROM enrolments \
                  WHERE course_id = c.id AND status = 'active') AS active_count, \
                (SELECT COUNT(*) FROM leave_requests \
                  WHERE session_id = $2 AND status = 'approved'::leave_status) AS approved_leave_count, \
                (SELECT COUNT(*) FROM leave_requests \
                  WHERE makeup_session_id = $2) AS makeup_count \
         FROM courses c WHERE c.id = $1",
    )
    .bind(course_id)
    .bind(target_session_id)
    .fetch_optional(&mut **tx)
    .await
}

/// Write the makeup session onto an approved, not-yet-made-up leave request.
/// The `WHERE` guard is a defense-in-depth belt alongside the `FOR UPDATE`
/// row lock already held via [`find_for_makeup_tx`] — by the time this runs,
/// `service::book_makeup` has already re-validated the locked row in-process,
/// so `None` here should not occur in practice.
pub async fn set_makeup_session_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    makeup_session_id: Uuid,
) -> Result<Option<LeaveRequest>, sqlx::Error> {
    sqlx::query_as::<_, LeaveRequest>(
        "UPDATE leave_requests SET makeup_session_id = $2, updated_at = NOW() \
         WHERE id = $1 AND status = 'approved'::leave_status AND makeup_session_id IS NULL \
         RETURNING id, enrolment_id, session_id, reason, status, makeup_session_id, \
                   decided_by, decided_at, created_at, updated_at",
    )
    .bind(id)
    .bind(makeup_session_id)
    .fetch_optional(&mut **tx)
    .await
}
