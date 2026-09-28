use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::model::{
    AdminLeaveRequestRow, LeaveDecisionContext, LeaveRequest, LeaveRequestForMakeup,
    LeaveRequestOwnerRow, LeaveRequestView, LeaveStatus, SessionContext,
};

/// The leave-request read projection's 13-column select list — single owner
/// of [`super::model::LeaveRequestView`]'s shape. `lr` is the row source's
/// alias, real table or a data-modifying CTE (see [`insert`]/[`decide_tx`]/
/// [`set_makeup_session_tx`]) alike.
const VIEW_COLUMNS: &str = "lr.id, e.course_id, c.name AS course_name, lr.session_id, cs.session_date, \
    cs.start_time, lr.reason, lr.status, lr.makeup_session_id, mcs.session_date AS makeup_session_date, \
    mcs.start_time AS makeup_start_time, lr.decided_at, lr.created_at";

/// [`VIEW_COLUMNS`]'s matching JOIN clause — assumes a `lr` row source
/// already in scope (the `leave_requests` table or a `WITH lr AS (...)` CTE).
const VIEW_JOINS: &str = "JOIN enrolments e ON e.id = lr.enrolment_id JOIN courses c ON c.id = e.course_id \
    JOIN course_sessions cs ON cs.id = lr.session_id LEFT JOIN course_sessions mcs ON mcs.id = lr.makeup_session_id";

/// `course_sessions` JOINed with its course's `name` — used both by
/// `POST /leave-requests` (plain pool read) and the makeup endpoint's
/// target-session validation (called through the open transaction that
/// holds the leave-request row lock).
pub async fn find_session_context(
    executor: impl sqlx::PgExecutor<'_>,
    session_id: Uuid,
) -> Result<Option<SessionContext>, sqlx::Error> {
    sqlx::query_as::<_, SessionContext>(
        "SELECT cs.id, cs.course_id, c.name AS course_name, cs.session_date, cs.start_time \
         FROM course_sessions cs \
         JOIN courses c ON c.id = cs.course_id \
         WHERE cs.id = $1",
    )
    .bind(session_id)
    .fetch_optional(executor)
    .await
}

/// The caller's active enrolment id for a course, if any — used by
/// `POST /leave-requests` to resolve `session_id` → "my enrolment" without
/// a capacity lock (creating a leave request doesn't touch course capacity).
/// 直接讀 `active_enrolments`(view,見 migration `20260711000001`),不經
/// `enrolments::repository`(該處沒有現成的、非交易式「by user+course」
/// 查找)——沿用 `sessions::repository::find_all_course_ids` 直接讀取
/// sibling module 資料表/view 應付一次性需求的慣例。
pub async fn find_active_enrolment(
    db: &PgPool,
    user_id: Uuid,
    course_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM active_enrolments WHERE user_id = $1 AND course_id = $2",
    )
    .bind(user_id)
    .bind(course_id)
    .fetch_optional(db)
    .await
}

/// Insert a new `pending` leave request, returning its read projection row
/// directly — a data-modifying CTE (`WITH lr AS (INSERT ... RETURNING *)`)
/// feeds the insert's own output back through [`VIEW_JOINS`], so the caller
/// gets the same shape [`find_my_leave_requests`] would without a second
/// query. Duplicate (enrolment_id, session_id) while an existing row is
/// `pending`/`approved` trips the partial unique index
/// `uniq_leave_requests_active` — `service` catches that as a 23505 and maps
/// it to a friendly 409.
pub async fn insert(
    db: &PgPool,
    enrolment_id: Uuid,
    session_id: Uuid,
    reason: Option<&str>,
) -> Result<LeaveRequestView, sqlx::Error> {
    sqlx::query_as::<_, LeaveRequestView>(sqlx::AssertSqlSafe(format!(
        "WITH lr AS ( \
             INSERT INTO leave_requests \
             (id, enrolment_id, session_id, reason, status, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, 'pending'::leave_status, NOW(), NOW()) \
             RETURNING * \
         ) \
         SELECT {VIEW_COLUMNS} FROM lr {VIEW_JOINS}"
    )))
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
) -> Result<Vec<LeaveRequestView>, sqlx::Error> {
    sqlx::query_as::<_, LeaveRequestView>(sqlx::AssertSqlSafe(format!(
        "SELECT {VIEW_COLUMNS} \
         FROM leave_requests lr \
         {VIEW_JOINS} \
         WHERE e.user_id = $1 \
         ORDER BY lr.created_at DESC"
    )))
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

/// Batch counterpart of [`cancel_if_pending_tx`] for enrolment cancellation
/// (B5): every still-`pending` leave request of the given enrolments →
/// `cancelled`, same predicate. Approved/rejected/cancelled rows (and any
/// booked makeup) are left as-is (ADR-0008 gap 1). Returns the number of rows
/// actually flipped.
pub async fn cancel_pending_for_enrolments_tx(
    tx: &mut Transaction<'_, Postgres>,
    enrolment_ids: &[Uuid],
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE leave_requests SET status = 'cancelled'::leave_status, updated_at = NOW() \
         WHERE enrolment_id = ANY($1) AND status = 'pending'::leave_status",
    )
    .bind(enrolment_ids)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected())
}

/// Coach/admin list — optional `status`/`course_id` filters, plus an
/// optional `coach_scope` (the caller's own `coaches.id`; `None` = no
/// restriction, i.e. admin sees every course's leave requests).
pub async fn find_admin_list(
    db: &PgPool,
    status_filter: Option<LeaveStatus>,
    course_id_filter: Option<Uuid>,
    coach_scope: Option<Uuid>,
    limit: u32,
    offset: u32,
) -> Result<Vec<AdminLeaveRequestRow>, sqlx::Error> {
    sqlx::query_as::<_, AdminLeaveRequestRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {VIEW_COLUMNS}, u.id AS user_id, u.name AS user_name \
         FROM leave_requests lr \
         {VIEW_JOINS} \
         JOIN users u ON u.id = e.user_id \
         WHERE ($1 IS NULL OR lr.status = $1) \
           AND ($2::uuid IS NULL OR c.id = $2) \
           AND ($3::uuid IS NULL OR c.coach_id = $3) \
         ORDER BY lr.created_at DESC \
         LIMIT $4 OFFSET $5"
    )))
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
    status_filter: Option<LeaveStatus>,
    course_id_filter: Option<Uuid>,
    coach_scope: Option<Uuid>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) \
         FROM leave_requests lr \
         JOIN enrolments e ON e.id = lr.enrolment_id \
         JOIN courses c ON c.id = e.course_id \
         WHERE ($1 IS NULL OR lr.status = $1) \
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
/// the owning student's `user_id` (notification target), the course's
/// `coach_id`/`name` (authorization + notification copy), and whether the
/// enrolment is still active (`rules::check_decidable`'s 409 for approving
/// a cancelled enrolment's leave — task 3). Deliberately keeps the raw
/// `enrolments` JOIN rather than `active_enrolments`: a cancelled enrolment
/// must still resolve to a 409 decision error, not a 404.
pub async fn find_decision_context(
    db: &PgPool,
    id: Uuid,
) -> Result<Option<LeaveDecisionContext>, sqlx::Error> {
    sqlx::query_as::<_, LeaveDecisionContext>(
        "SELECT lr.status, lr.enrolment_id, lr.session_id, e.user_id, e.course_id, \
                c.name AS course_name, c.coach_id, cs.session_date, cs.start_time, \
                EXISTS (SELECT 1 FROM active_enrolments ae WHERE ae.id = lr.enrolment_id) \
                  AS enrolment_active \
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

/// Conditional approve/reject — only succeeds while still `pending`, returning
/// its read projection row (via the same data-modifying CTE shape as
/// [`insert`]). Returns `None` if the row was raced to a different status
/// (caller maps that to 409); mirrors [`cancel_if_pending_tx`]'s guard shape.
pub async fn decide_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    new_status: LeaveStatus,
    decided_by: Uuid,
) -> Result<Option<LeaveRequestView>, sqlx::Error> {
    sqlx::query_as::<_, LeaveRequestView>(sqlx::AssertSqlSafe(format!(
        "WITH lr AS ( \
             UPDATE leave_requests \
             SET status = $2, decided_by = $3, decided_at = NOW(), updated_at = NOW() \
             WHERE id = $1 AND status = 'pending'::leave_status \
             RETURNING * \
         ) \
         SELECT {VIEW_COLUMNS} FROM lr {VIEW_JOINS}"
    )))
    .bind(id)
    .bind(new_status)
    .bind(decided_by)
    .fetch_optional(&mut **tx)
    .await
}

/// Lock the leave request row (`FOR UPDATE OF lr`) for the makeup endpoint —
/// this is the guard that makes two concurrent `POST .../makeup` calls for
/// the *same* leave request serialize, so only one can ever see
/// `makeup_session_id IS NULL` and win. JOINed with `enrolments`/`courses`
/// for the owner check and the original session's own display fields, but
/// only the `leave_requests` row itself is locked. Also reports whether the
/// enrolment is still active (`rules::check_makeup_source`'s 409 for booking
/// a makeup on a cancelled enrolment — task 3); kept as a raw `enrolments`
/// JOIN, not `active_enrolments`, so a cancelled enrolment still resolves to
/// 409 rather than 404.
pub async fn find_for_makeup_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<LeaveRequestForMakeup>, sqlx::Error> {
    sqlx::query_as::<_, LeaveRequestForMakeup>(
        "SELECT lr.id, lr.session_id, e.user_id, e.course_id, c.name AS course_name, \
                lr.status, lr.makeup_session_id, cs.session_date, cs.start_time, lr.reason, \
                EXISTS (SELECT 1 FROM active_enrolments ae WHERE ae.id = lr.enrolment_id) \
                  AS enrolment_active \
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

/// Write the makeup session onto an approved, not-yet-made-up leave request,
/// returning its read projection row — the same data-modifying CTE shape as
/// [`insert`]/[`decide_tx`], with `mcs` in [`VIEW_JOINS`] now resolving
/// against the just-written `makeup_session_id` within this one statement.
/// The `WHERE` guard is a defense-in-depth belt alongside the `FOR UPDATE`
/// row lock already held via [`find_for_makeup_tx`] — by the time this runs,
/// `service::book_makeup` has already re-validated the locked row in-process,
/// so `None` here should not occur in practice.
pub async fn set_makeup_session_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    makeup_session_id: Uuid,
) -> Result<Option<LeaveRequestView>, sqlx::Error> {
    sqlx::query_as::<_, LeaveRequestView>(sqlx::AssertSqlSafe(format!(
        "WITH lr AS ( \
             UPDATE leave_requests SET makeup_session_id = $2, updated_at = NOW() \
             WHERE id = $1 AND status = 'approved'::leave_status AND makeup_session_id IS NULL \
             RETURNING * \
         ) \
         SELECT {VIEW_COLUMNS} FROM lr {VIEW_JOINS}"
    )))
    .bind(id)
    .bind(makeup_session_id)
    .fetch_optional(&mut **tx)
    .await
}
